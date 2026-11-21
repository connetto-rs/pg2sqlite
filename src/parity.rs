//! Replays rls-parity's row-level security cases on pg2sqlite's SQLite
//! replica, so each answer can be judged against PostgreSQL's recorded one
//! with [`Fixture::judge`](sql_traits::parity::Fixture::judge).
//!
//! [`replay`] translates one replica per acting role the case's callers name,
//! seeds it as the table owner, and answers every probe on the replica of its
//! caller's acting role. It needs no PostgreSQL.
//!
//! ```
//! use pg2sqlite::parity::{ParityError, Probe, case_options, replay};
//! use sql_traits::parity::{Engine, fixtures};
//!
//! let fixture = fixtures()
//!     .find_map(|fixture| fixture.ok().filter(|fixture| fixture.case.name == "delete_own_rows"))
//!     .expect("the case is embedded");
//! let probes: Vec<Probe<'_>> = fixture
//!     .recorded
//!     .answer
//!     .iter()
//!     .map(|recorded| {
//!         Probe { caller: &recorded.caller, row: &recorded.row, statement: &recorded.statement }
//!     })
//!     .collect();
//! let replay = replay(&fixture.case, &case_options(&fixture.case)?, &probes)?;
//! let discrepancies = fixture.judge(Engine::Sqlite, replay.refusal.as_deref(), &replay.answers);
//! assert!(discrepancies.is_empty(), "{discrepancies:?}");
//! # Ok::<(), ParityError>(())
//! ```

use alloc::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    sync::Arc,
};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use diesel::{
    connection::SimpleConnection,
    deserialize::{self, FromSql},
    prelude::*,
    result::Error as DieselError,
    serialize::{self, IsNull, Output, ToSql},
    sql_types::{Binary, Bool, Nullable, Text},
    sqlite::{Sqlite, SqliteFunctionBehavior, SqliteType, SqliteValue},
};
use sql_traits::{
    errors::DeploymentError,
    parity::{Answer, Case, EngineAnswer, Table, identifier, text_of},
    structs::ParserDB,
};
use sqlparser::ast::{Ident, ObjectName, Statement, TableObject};

use crate::prelude::*;

/// One caller, row and statement to answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Probe<'a> {
    /// The name of one of the case's callers.
    pub caller: &'a str,
    /// The row, its table name and key joined by a colon, the key spelled as
    /// [`Table::row_key`] spells it.
    pub row: &'a str,
    /// The PostgreSQL statement the caller runs, as given.
    pub statement: &'a str,
}

/// The replica's run of a case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Replay {
    /// Why pg2sqlite refuses the schema, one line per acting role whose
    /// translation it refuses, `None` when it translates every one.
    pub refusal: Option<String>,
    /// One answer per probe, in the probes' order.
    pub answers: Vec<EngineAnswer>,
}

/// Why a case cannot be replayed.
#[derive(Debug, thiserror::Error)]
pub enum ParityError {
    /// SQLite cannot open an in-memory database.
    #[error("Cannot open the replica of {role}")]
    Connection {
        /// The acting role.
        role: String,
        /// The cause.
        #[source]
        source: diesel::ConnectionError,
    },
    /// SQLite refuses a statement that builds or seeds a replica, or that
    /// registers its functions.
    #[error("{action} on the replica of {role}")]
    Database {
        /// The acting role.
        role: String,
        /// What the replica was doing.
        action: String,
        /// The cause.
        #[source]
        source: DieselError,
    },
    /// pg2sqlite translated the schema and then failed to describe it,
    /// refused the seed, or has no stored form for a caller's setting.
    #[error("Cannot build the replica of {role}")]
    Translation {
        /// The acting role.
        role: String,
        /// The cause.
        #[source]
        source: Box<Error>,
    },
    /// A seed statement inserts into something other than a named table.
    #[error("The seed must insert into a named table: {0}")]
    SeedTarget(String),
    /// A probe names a caller the case does not declare.
    #[error("No caller is named {0}")]
    UnknownCaller(String),
    /// A probe's row names no table the case declares.
    #[error("Row {0} names no declared table")]
    UnknownTable(String),
    /// The case declares no caller, so it has no deployment.
    #[error(transparent)]
    Deployment(#[from] DeploymentError),
}

/// The options a case translates its replica under before choosing its acting
/// role: the case's deployment, UUIDs stored as blobs and minted by `uuidv4`,
/// the `rls_audit` table, `connetto_write_exempt` exempting the seed, the
/// `jsonb` functions, and `app.user_id` and `app.subjects` answered by
/// `current_app_user` and `current_app_subjects`.
///
/// # Errors
///
/// Returns [`ParityError::Deployment`] when the case declares no caller.
pub fn case_options(case: &Case) -> Result<Pg2SqliteOptions, ParityError> {
    Ok(Pg2SqliteOptions::default()
        .with_deployment(case.config.deployment()?)
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv4")
        .with_rls_audit_table_name("rls_audit".to_owned())
        .with_write_exemption_function("connetto_write_exempt")
        .with_jsonb_functions(JsonbFunctions::new("jsonb_canonical", "jsonb_text", "jsonb_order"))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "current_app_user"))
        .with_session_variable(SessionVariableMapping::new("app.subjects", "current_app_subjects")))
}

/// Answer every probe on the replica of its caller's acting role, `owner`
/// being the options the schema and seed translate under before the acting
/// role is chosen.
///
/// A read answers [`Answer::Visible`] or [`Answer::Hidden`] for the row, a
/// write [`Answer::Changed`], [`Answer::Skipped`] or [`Answer::Rejected`]
/// with the SQLSTATE that aborted it, [`Answer::Refused`] when pg2sqlite
/// refuses the statement or the schema of the caller's acting role, and
/// [`Answer::Error`] when SQLite fails to run it. A write runs in a savepoint
/// that is rolled back, so every probe sees the seeded rows.
///
/// # Errors
///
/// Returns a [`ParityError`] when a replica cannot be built or seeded, or a
/// probe names an undeclared caller or table.
pub fn replay(
    case: &Case,
    owner: &Pg2SqliteOptions,
    probes: &[Probe<'_>],
) -> Result<Replay, ParityError> {
    let mut replicas = BTreeMap::new();
    for role in case.config.acting_roles() {
        replicas.insert(role, Replica::create(case, owner, role)?);
    }
    let refusals: Vec<String> = replicas
        .iter()
        .filter_map(|(role, replica)| {
            match replica {
                Prepared::Ready(_) => None,
                Prepared::Refused(reason) => Some(format!("{role}: {reason}")),
            }
        })
        .collect();
    let mut measured = BTreeMap::new();
    let mut answers = Vec::with_capacity(probes.len());
    for probe in probes {
        let caller = case
            .config
            .callers
            .iter()
            .position(|caller| caller.name == probe.caller)
            .ok_or_else(|| ParityError::UnknownCaller(probe.caller.to_owned()))?;
        let (table, key) = probe
            .row
            .split_once(':')
            .and_then(|(table, key)| Some((case.table(table)?, key)))
            .ok_or_else(|| ParityError::UnknownTable(probe.row.to_owned()))?;
        let replica = replicas
            .get_mut(case.config.callers[caller].role.as_str())
            .expect("every caller's role is an acting role, so it has a replica");
        let answer = match replica {
            Prepared::Refused(reason) => Answer::Refused(reason.clone()),
            Prepared::Ready(replica) => {
                match measured.entry((caller, table.name.as_str(), probe.statement)) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => {
                        entry.insert(replica.run(table, caller, probe.statement)?)
                    }
                }
                .answer(key)
            }
        };
        answers.push(EngineAnswer {
            caller: probe.caller.to_owned(),
            row: probe.row.to_owned(),
            statement: probe.statement.to_owned(),
            answer,
        });
    }
    Ok(Replay { refusal: (!refusals.is_empty()).then(|| refusals.join("\n")), answers })
}

/// A replica pg2sqlite built, or why it refused to.
enum Prepared<T> {
    Ready(T),
    Refused(String),
}

/// What one statement did on a replica.
enum Measured {
    /// A read returned the rows of the probed table with these keys.
    Read(BTreeSet<String>),
    /// A write ran and changed the rows of the probed table with these keys.
    Applied(BTreeSet<String>),
    /// A write aborted with this SQLSTATE.
    Rejected(String),
    /// pg2sqlite refused the statement for this reason.
    Refused(String),
    /// SQLite failed to run the statement with this message.
    Failed(String),
}

impl Measured {
    /// The answer this measurement gives for the row with `key`.
    fn answer(&self, key: &str) -> Answer {
        match self {
            Self::Read(keys) if keys.contains(key) => Answer::Visible,
            Self::Read(_) => Answer::Hidden,
            Self::Applied(keys) if keys.contains(key) => Answer::Changed,
            Self::Applied(_) => Answer::Skipped,
            Self::Rejected(sqlstate) => Answer::Rejected(sqlstate.clone()),
            Self::Refused(reason) => Answer::Refused(reason.clone()),
            Self::Failed(message) => Answer::Error(message.clone()),
        }
    }
}

/// The caller each replica function answers for, whether writes are exempt, and
/// how many UUIDs the replica has minted.
struct Session {
    /// Each caller's settings, as the replica stores them.
    callers: Vec<BTreeMap<String, Stored>>,
    caller: AtomicUsize,
    exempt: AtomicBool,
    minted: AtomicU64,
}

impl Session {
    /// The current caller's value of `setting` as the replica stores it,
    /// `NULL` when the caller sets none, as `current_setting(setting, true)`
    /// reads it.
    fn setting(&self, setting: &str) -> Option<Stored> {
        self.callers[self.caller.load(Ordering::Relaxed)].get(setting).cloned()
    }

    /// A fresh version 4 UUID as sixteen bytes, numbered from a counter so
    /// every run mints the same sequence.
    fn mint_uuid(&self) -> [u8; 16] {
        let number = self.minted.fetch_add(1, Ordering::Relaxed) + 1;
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&number.to_be_bytes());
        bytes[6] = 0x40;
        bytes[8] |= 0x80;
        bytes
    }
}

/// A value in whichever storage class SQLite holds it.
///
/// It crosses diesel as `Text`, a name only, since SQLite types each value
/// and not a function's result.
#[derive(Clone, Debug)]
enum Stored {
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl FromSql<Text, Sqlite> for Stored {
    fn from_sql(mut value: SqliteValue<'_, '_, '_>) -> deserialize::Result<Self> {
        Ok(match value.value_type() {
            Some(SqliteType::Long | SqliteType::Integer | SqliteType::SmallInt) => {
                Self::Integer(value.read_long())
            }
            Some(SqliteType::Double | SqliteType::Float) => Self::Real(value.read_double()),
            Some(SqliteType::Binary) => Self::Blob(value.read_blob().to_vec()),
            Some(SqliteType::Text) | None => Self::Text(value.read_text().to_owned()),
        })
    }
}

impl ToSql<Text, Sqlite> for Stored {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Sqlite>) -> serialize::Result {
        match self {
            Self::Integer(value) => out.set_value(*value),
            Self::Real(value) => out.set_value(*value),
            Self::Text(value) => out.set_value(value.as_str()),
            Self::Blob(value) => out.set_value(value.as_slice()),
        }
        Ok(IsNull::No)
    }
}

/// Each setting `owner` maps that `settings` sets, as the replica stores it,
/// which is what the paired function answers.
///
/// pg2sqlite's own conversion turns the case's text into the stored form, so
/// the replica runs it once per setting.
fn stored_settings(
    connection: &mut SqliteConnection,
    owner: &Pg2SqliteOptions,
    settings: &BTreeMap<String, String>,
    role: &str,
) -> Result<BTreeMap<String, Stored>, ParityError> {
    let mut stored = BTreeMap::new();
    for mapping in owner.get_session_variables() {
        let Some(text) = settings.get(&mapping.setting) else { continue };
        let answer = crate::impls::session_variable::paired_answer(mapping, text, owner).map_err(
            |source| ParityError::Translation { role: role.to_owned(), source: Box::new(source) },
        )?;
        // The case's setting is runtime fixture data, as pg2sqlite converted
        // it.
        let value = diesel::select(diesel::dsl::sql::<Nullable<Text>>(&answer.to_string()))
            .get_result::<Option<Stored>>(connection)
            .map_err(|source| {
                ParityError::Database {
                    role: role.to_owned(),
                    action: format!("Storing {} as {answer}", mapping.setting),
                    source,
                }
            })?;
        if let Some(value) = value {
            stored.insert(mapping.setting.clone(), value);
        }
    }
    Ok(stored)
}

/// The temporary table [`track_changes`] fills, private so `table!` adds
/// nothing to the public API.
mod changes {
    diesel::table! {
        /// The logical table and key of every row a write changed.
        probe_changes (logical, key) {
            logical -> Text,
            key -> Text,
        }
    }
}

use changes::probe_changes;

/// One key a read returned.
#[derive(QueryableByName)]
struct KeyRow {
    #[diesel(sql_type = Text)]
    key: String,
}

struct Replica {
    connection: SqliteConnection,
    session: Arc<Session>,
    schema: ParserDB,
    options: Pg2SqliteOptions,
    /// How the replica stores each published table, read to spell its keys as
    /// text.
    manifest: Vec<TableManifestEntry>,
}

impl Replica {
    /// Load the script emitted for `role` and seed its published physical
    /// tables as the table owner, as PostgreSQL loads the seed.
    fn create(
        case: &Case,
        owner: &Pg2SqliteOptions,
        role: &str,
    ) -> Result<Prepared<Self>, ParityError> {
        let options = owner.clone().with_acting_role(role);
        let translator = match Pg2Sqlite::default().sql(&case.schema) {
            Ok(translator) => translator,
            Err(error) => return Ok(Prepared::Refused(error.to_string())),
        };
        let script = match translator.translate(&options) {
            Ok(script) => script,
            Err(error) => return Ok(Prepared::Refused(error.to_string())),
        };
        let failed =
            |source| ParityError::Translation { role: role.to_owned(), source: Box::new(source) };
        let manifest = translator.translation_manifest(&options).map_err(failed)?;
        let schema = translator.build_schema().map_err(failed)?;
        let seed = Pg2Sqlite::default()
            .sql(&case.seed)
            .and_then(|seed| seed.translate_with_schema(&schema, owner))
            .map_err(failed)?;
        let mut connection = SqliteConnection::establish(":memory:")
            .map_err(|source| ParityError::Connection { role: role.to_owned(), source })?;
        let callers = case
            .config
            .callers
            .iter()
            .map(|caller| stored_settings(&mut connection, owner, &caller.settings, role))
            .collect::<Result<_, _>>()?;
        let session = Arc::new(Session {
            callers,
            caller: AtomicUsize::new(0),
            exempt: AtomicBool::new(false),
            minted: AtomicU64::new(0),
        });
        let database = |action: String| {
            move |source| ParityError::Database { role: role.to_owned(), action, source }
        };
        register_functions(&mut connection, owner, &session)
            .map_err(database("Registering the replica functions".to_owned()))?;
        for statement in script.iter() {
            connection
                .batch_execute(&format!("{statement};"))
                .map_err(database(format!("Applying {statement}")))?;
        }
        session.exempt.store(true, Ordering::Relaxed);
        for mut statement in seed.into_vec() {
            if let Statement::Insert(insert) = &mut statement {
                let TableObject::TableName(name) = &insert.table else {
                    return Err(ParityError::SeedTarget(statement.to_string()));
                };
                let logical = name.to_string();
                let Some(entry) =
                    manifest.iter().find(|entry| logical.trim_matches('"') == entry.logical)
                else {
                    continue;
                };
                insert.table = TableObject::TableName(ObjectName::from(vec![Ident::with_quote(
                    '"',
                    &entry.physical,
                )]));
            }
            // Seed statements describe runtime fixture schemas and retain
            // translated value conversions.
            connection
                .batch_execute(&format!("{statement};"))
                .map_err(database(format!("Seeding {statement}")))?;
        }
        session.exempt.store(false, Ordering::Relaxed);
        track_changes(&mut connection, &case.config.tables, &manifest)
            .map_err(database("Tracking the changed rows".to_owned()))?;
        Ok(Prepared::Ready(Self { connection, session, schema, options, manifest }))
    }

    /// Run pg2sqlite's own translation of `statement` with the caller's session
    /// functions, a read collecting the keys of the `table` rows it returns and
    /// a write, rolled back, the keys of the rows of `table` it wrote.
    fn run(
        &mut self,
        table: &Table,
        caller: usize,
        statement: &str,
    ) -> Result<Measured, ParityError> {
        self.session.caller.store(caller, Ordering::Relaxed);
        let source = match Pg2Sqlite::default().sql(statement) {
            Ok(source) => source,
            Err(error) => return Ok(Measured::Refused(error.to_string())),
        };
        let reads = matches!(source.statements(), [Statement::Query(_)]);
        let translated = match source.translate_with_schema(&self.schema, &self.options) {
            Ok(script) => script.into_vec(),
            Err(error) if reads => return Ok(Measured::Refused(error.to_string())),
            Err(error) => return Ok(refused_write(error.to_string())),
        };
        if reads {
            Ok(self.read(table, statement, translated))
        } else {
            self.write(table, &translated)
        }
    }

    /// Collect the keys of the `table` rows the one query `translated` holds
    /// returns.
    fn read(&mut self, table: &Table, statement: &str, translated: Vec<Statement>) -> Measured {
        let mut queries =
            translated.into_iter().filter(|translated| matches!(translated, Statement::Query(_)));
        let Some(query) = queries.next().filter(|_| queries.next().is_none()) else {
            return Measured::Failed(format!("{statement} did not translate to one query"));
        };
        let entry = self.manifest.iter().find(|entry| entry.logical == table.name);
        // The case supplies the statement at runtime, so only its text can wrap
        // it.
        match diesel::sql_query(format!(
            "SELECT {} AS key FROM ({query}) q",
            table.key_sql(|key| replica_text(entry, "q", key)),
        ))
        .load::<KeyRow>(&mut self.connection)
        {
            Ok(rows) => Measured::Read(rows.into_iter().map(|row| row.key).collect()),
            Err(error) => Measured::Failed(error.to_string()),
        }
    }

    /// Run the translated write in a savepoint that is rolled back, collecting
    /// the keys of the rows of `table` it wrote.
    fn write(&mut self, table: &Table, translated: &[Statement]) -> Result<Measured, ParityError> {
        let Self { connection, options, .. } = self;
        let role = options.get_acting_role().unwrap_or_default();
        let database = |action: &str| {
            let action = action.to_owned();
            move |source| ParityError::Database { role: role.to_owned(), action, source }
        };
        connection.batch_execute("SAVEPOINT probe").map_err(database("Opening a savepoint"))?;
        let measured = (|| {
            for statement in translated {
                // The case's write is runtime fixture data, as pg2sqlite
                // translated it.
                if let Err(error) = connection.batch_execute(&format!("{statement};")) {
                    if let DieselError::DatabaseError(_, information) = &error
                        && let Some(refusal) = postgres_error(information.message())
                    {
                        return Measured::Rejected(refusal.sqlstate().to_string());
                    }
                    return Measured::Failed(error.to_string());
                }
            }
            match probe_changes::table
                .filter(probe_changes::logical.eq(&table.name))
                .select(probe_changes::key)
                .load::<String>(connection)
            {
                Ok(keys) => Measured::Applied(keys.into_iter().collect()),
                Err(error) => Measured::Failed(error.to_string()),
            }
        })();
        connection
            .batch_execute("ROLLBACK TO probe; RELEASE probe")
            .map_err(database("Rolling back a probe"))?;
        Ok(measured)
    }
}

/// Register on `connection` the functions `options` name: each session
/// variable's function answering the current caller's setting as the replica
/// stores it, the write exemption, the UUID minter, and the `jsonb` functions
/// and collation.
fn register_functions(
    connection: &mut SqliteConnection,
    options: &Pg2SqliteOptions,
    session: &Arc<Session>,
) -> QueryResult<()> {
    let volatile = SqliteFunctionBehavior::empty();
    for mapping in options.get_session_variables() {
        let session = Arc::clone(session);
        let setting = mapping.setting.clone();
        connection.register_noarg_sql_function::<Nullable<Text>, _, _>(
            &mapping.sqlite_function,
            volatile,
            move || session.setting(&setting),
        )?;
    }
    if let Some(function) = options.get_write_exemption_function() {
        let session = Arc::clone(session);
        connection.register_noarg_sql_function::<Bool, _, _>(function, volatile, move || {
            session.exempt.load(Ordering::Relaxed)
        })?;
    }
    let minted = Arc::clone(session);
    if options.get_uuid_representation() == Some(UuidRepresentation::Blob) {
        connection.register_noarg_sql_function::<Binary, _, _>(
            options.get_uuid_function_name(),
            volatile,
            move || minted.mint_uuid().to_vec(),
        )?;
    } else {
        connection.register_noarg_sql_function::<Text, _, _>(
            options.get_uuid_function_name(),
            volatile,
            move || hyphenated(&minted.mint_uuid()),
        )?;
    }
    match options.get_jsonb_functions() {
        Some(functions) => functions.register(connection),
        None => Ok(()),
    }
}

/// A UUID's sixteen bytes in their canonical lowercase hyphenated form.
fn hyphenated(bytes: &[u8; 16]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// A write pg2sqlite refuses to translate, read as the SQLSTATE PostgreSQL
/// raises when the refusal is one of its errors.
fn refused_write(reason: String) -> Measured {
    match postgres_error(&reason).map(|error| error.sqlstate().to_string()) {
        Some(sqlstate) => Measured::Rejected(sqlstate),
        None => Measured::Refused(reason),
    }
}

/// Record in `probe_changes` the logical table and key of every row a statement
/// writes to each of `tables` that the replica publishes.
fn track_changes(
    connection: &mut SqliteConnection,
    tables: &[Table],
    manifest: &[TableManifestEntry],
) -> QueryResult<()> {
    connection.batch_execute(
        "CREATE TEMP TABLE probe_changes (logical TEXT NOT NULL, key TEXT NOT NULL)",
    )?;
    for (index, table) in tables.iter().enumerate() {
        // A table the translation omits has no physical counterpart to watch.
        let Some(entry) = manifest.iter().find(|entry| entry.logical == table.name) else {
            continue;
        };
        let logical = format!("'{}'", table.name.replace('\'', "''"));
        let physical = identifier(&entry.physical);
        let old =
            format!("({logical}, {})", table.key_sql(|key| replica_text(Some(entry), "OLD", key)));
        let new =
            format!("({logical}, {})", table.key_sql(|key| replica_text(Some(entry), "NEW", key)));
        for (event, rows) in
            [("INSERT", new.clone()), ("UPDATE", format!("{old}, {new}")), ("DELETE", old)]
        {
            connection.batch_execute(&format!(
                "CREATE TEMP TRIGGER probe_{index}_{event} AFTER {event} ON main.{physical} \
                 BEGIN INSERT INTO probe_changes VALUES {rows}; END"
            ))?;
        }
    }
    Ok(())
}

/// A replica column read through `qualifier` as the text PostgreSQL prints for
/// it, a UUID stored as sixteen bytes spelled in its canonical lowercase
/// hyphenated form.
fn replica_text(entry: Option<&TableManifestEntry>, qualifier: &str, column: &str) -> String {
    let stored_as_blob = entry.is_some_and(|entry| {
        entry
            .columns
            .iter()
            .any(|stored| stored.name == column && stored.storage == ColumnStorage::UuidBlob)
    });
    if !stored_as_blob {
        return text_of(qualifier, column);
    }
    let hex = format!("hex({qualifier}.{})", identifier(column));
    let groups: Vec<String> = [(1, 8), (9, 4), (13, 4), (17, 4), (21, 12)]
        .iter()
        .map(|(start, length)| format!("substr({hex}, {start}, {length})"))
        .collect();
    format!("lower({})", groups.join(" || '-' || "))
}

#[cfg(test)]
mod tests {
    use diesel::sql_types::{Binary, Bool, Nullable, Text};
    use sql_traits::parity::{Shape, fixtures};

    use super::*;

    #[diesel::declare_sql_function]
    extern "SQL" {
        fn current_app_user() -> Nullable<Text>;
        fn current_app_subjects() -> Nullable<Text>;
        fn connetto_write_exempt() -> Bool;
        fn uuidv4() -> Binary;
    }

    /// The embedded case named `name`.
    fn case(name: &str) -> Case {
        fixtures()
            .map(|fixture| fixture.expect("every embedded case reads").case)
            .find(|case| case.name == name)
            .expect("the case is embedded")
    }

    /// Registered session functions follow the caller, read a setting the
    /// caller leaves unset as `NULL`, leave writes unexempted, and mint version
    /// 4 UUIDs numbered from one.
    #[test]
    fn caller_session_functions() {
        let mut case = case("grant_without_update");
        case.config.callers[0].settings.insert("app.subjects".into(), "alpha,beta".into());
        case.config.callers[1].settings.remove("app.subjects");
        let owner = case_options(&case).expect("the case has a deployment");
        let role = case.config.callers[0].role.clone();
        let Ok(Prepared::Ready(mut replica)) = Replica::create(&case, &owner, &role) else {
            panic!("ownership fixture must translate");
        };
        let table = &case.config.tables[0];
        let select = table.statement(Shape::Select);
        for (index, caller) in case.config.callers.iter().enumerate() {
            assert!(matches!(
                replica.run(table, index, &select),
                Ok(Measured::Read(keys)) if !keys.is_empty()
            ));
            let actual = diesel::select((
                current_app_user(),
                current_app_subjects(),
                connetto_write_exempt(),
            ))
            .get_result::<(Option<String>, Option<String>, bool)>(&mut replica.connection)
            .expect("the session functions answer");
            assert_eq!(
                actual,
                (
                    caller.settings.get("app.user_id").cloned(),
                    caller.settings.get("app.subjects").cloned(),
                    false
                )
            );
        }
        assert_eq!(case.config.callers[1].settings.get("app.subjects"), None);
        assert!(matches!(replica.run(table, 0, "INVALID SQL"), Ok(Measured::Refused(_))));
        let minted = diesel::select((uuidv4(), uuidv4()))
            .get_result::<(Vec<u8>, Vec<u8>)>(&mut replica.connection)
            .expect("uuidv4 mints");
        let expected = |number: u8| {
            let mut bytes = vec![0; 16];
            bytes[6] = 0x40;
            bytes[8] = 0x80;
            bytes[15] = number;
            bytes
        };
        assert_eq!(minted, (expected(1), expected(2)));
        case.schema = "INVALID SQL".into();
        assert!(matches!(Replica::create(&case, &owner, &role), Ok(Prepared::Refused(_))));
    }

    /// A probe naming a caller or a table the case does not declare is refused
    /// before any answer, and a schema pg2sqlite refuses answers every probe
    /// with its reason.
    #[test]
    fn probes_resolve_against_the_case() {
        let mut case = case("grant_without_update");
        let owner = case_options(&case).expect("the case has a deployment");
        let probe = |caller, row| Probe { caller, row, statement: r#"SELECT "id" FROM "docs""# };
        assert!(matches!(
            replay(&case, &owner, &[probe("nobody", "docs:1")]),
            Err(ParityError::UnknownCaller(caller)) if caller == "nobody"
        ));
        assert!(matches!(
            replay(&case, &owner, &[probe("alice", "nodocs:1")]),
            Err(ParityError::UnknownTable(row)) if row == "nodocs:1"
        ));
        case.schema = "INVALID SQL".into();
        let replay = replay(&case, &owner, &[probe("alice", "docs:1")]).expect("replays");
        let refusal = replay.refusal.expect("the schema is refused");
        assert!(refusal.starts_with("app_reader: "), "{refusal}");
        assert!(matches!(&replay.answers[0].answer, Answer::Refused(_)));
    }

    /// A write's translation refusal carrying a PostgreSQL error answers its
    /// SQLSTATE, and any other refusal stays a refusal.
    #[test]
    fn refused_write_answers_postgres_errors() {
        assert!(matches!(
            refused_write("permission denied for table docs".into()),
            Measured::Rejected(sqlstate) if sqlstate == "42501"
        ));
        assert!(matches!(
            refused_write("Unsupported SQLite feature: write-only column".into()),
            Measured::Refused(reason) if reason == "Unsupported SQLite feature: write-only column"
        ));
    }

    /// A session function answers a one-value setting as the replica stores
    /// it, so a `uuid` caller is the sixteen bytes a blob-stored `uuid`
    /// column holds and each caller sees the row it owns.
    #[test]
    fn a_uuid_caller_answers_its_stored_bytes() {
        use sql_traits::structs::{Deployment, SessionSetting, SetPacking, SettingValue};
        use sqlparser::ast::DataType;

        let case = case("uuid_owner_blob");
        let deployment = Deployment::new(
            case.config.callers[0].role.as_str(),
            case.config.acting_roles(),
            [
                SessionSetting::value("app.user_id", SettingValue::Caller(DataType::Uuid))
                    .expect("a caller setting"),
                SessionSetting::value(
                    "app.subjects",
                    SettingValue::Set {
                        element: DataType::Text,
                        packing: SetPacking::Delimited(",".into()),
                    },
                )
                .expect("a set setting"),
            ],
        )
        .expect("a deployment");
        let owner =
            case_options(&case).expect("the case has a deployment").with_deployment(deployment);
        let statement = case.config.tables[0].statement(Shape::Select);
        let probes: Vec<Probe<'_>> =
            [("a", "udocs:1"), ("a", "udocs:2"), ("b", "udocs:1"), ("b", "udocs:2")]
                .into_iter()
                .map(|(caller, row)| Probe { caller, row, statement: &statement })
                .collect();
        let replay = replay(&case, &owner, &probes).expect("replays");
        assert_eq!(replay.refusal, None);
        let answers: Vec<&Answer> = replay.answers.iter().map(|answer| &answer.answer).collect();
        assert_eq!(answers, [&Answer::Visible, &Answer::Hidden, &Answer::Hidden, &Answer::Visible]);
    }

    /// A UUID minted as text spells its bytes in the canonical hyphenated form.
    #[test]
    fn hyphenated_uuid() {
        let bytes = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xf0, 0x0f, 0x10, 0x32, 0x54, 0x76,
            0x98, 0xba,
        ];
        assert_eq!(hyphenated(&bytes), "01234567-89ab-cdef-f00f-1032547698ba");
    }
}
