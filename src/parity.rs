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
use pg2sqlite_runtime::{ExecutionGroup, JiffUtcProvider, ManagedConnection};
use sql_traits::{
    errors::DeploymentError,
    parity::{Answer, Case, EngineAnswer, Table, identifier, text_of},
    structs::{ParserDB, WrittenRelation},
    traits::{ColumnLike, TableLike, ViewLike},
};
use sqlparser::ast::{Ident, ObjectName, Statement, TableObject, TriggerPeriod};

use crate::{
    impls::translator_impls::view_writes::{output_names, resolve_view},
    prelude::*,
    script::{FreshFiles, OpenError},
};

/// The managed connection a replica runs on.
type ReplicaConnection = ManagedConnection<JiffUtcProvider>;

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
    /// A write probes a measured view whose key the replica cannot trace to
    /// columns of the table the write lands on, so it cannot name the rows
    /// the write changes.
    #[error("Cannot name the rows a write through view {view} changes")]
    UntracedView {
        /// The measured view.
        view: String,
        /// Why its key reaches no column of that table.
        #[source]
        reason: UntracedKey,
    },
    /// The case declares no caller, so it has no deployment.
    #[error(transparent)]
    Deployment(#[from] DeploymentError),
}

/// Why the key of a measured view reaches no column of the table a write
/// through the view lands on.
#[derive(Debug, thiserror::Error)]
pub enum UntracedKey {
    /// The view outputs no column of the key's name.
    #[error("the view outputs no column {0}")]
    MissingColumn(String),
    /// An `INSTEAD OF` trigger of the schema takes the write for a view the
    /// write passes through, writing whatever its function writes.
    #[error("an INSTEAD OF trigger takes the write for view {0}")]
    TakenByTrigger(String),
    /// A view the write passes through is not automatically updatable.
    #[error("view {0} is not automatically updatable")]
    NotUpdatable(String),
    /// The key column computes its value from the relation a view reads.
    #[error("key column {0} computes its value")]
    Computed(String),
    /// Resolving a view the write passes through against the schema failed.
    #[error(transparent)]
    Lookup(#[from] Box<Error>),
}

/// The options a case translates its replica under before choosing its acting
/// role: the case's deployment, UUIDs stored as blobs and minted by `uuidv4`,
/// the `rls_audit` table, `connetto_write_exempt` exempting the seed, the
/// `jsonb` functions, and every setting the deployment declares answered by
/// `case_setting_N`, `N` counting the declarations from zero.
///
/// # Errors
///
/// Returns [`ParityError::Deployment`] when the case declares no caller.
pub fn case_options(case: &Case) -> Result<Pg2SqliteOptions, ParityError> {
    let deployment = case.config.deployment()?;
    let options = deployment.settings().iter().enumerate().fold(
        Pg2SqliteOptions::default()
            .with_uuid_representation(UuidRepresentation::Blob)
            .with_uuid_function_name("uuidv4")
            .with_rls_audit_table_name("rls_audit".to_owned())
            .with_rls_exemption_function("connetto_write_exempt")
            .with_jsonb_functions(JsonbFunctions::new(
                "jsonb_canonical",
                "jsonb_text",
                "jsonb_order",
            )),
        |options, (index, setting)| {
            options.with_session_variable(SessionVariableMapping::new(
                setting.name(),
                format!("case_setting_{index}"),
            ))
        },
    );
    Ok(options.with_deployment(deployment))
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
/// Returns a [`ParityError`] when a replica cannot be built or seeded, a
/// probe names an undeclared caller or table, or a write applies through a
/// measured view whose key the replica cannot trace to a published table.
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
    connection: &mut ReplicaConnection,
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
    connection: ReplicaConnection,
    session: Arc<Session>,
    schema: ParserDB,
    options: Pg2SqliteOptions,
    /// How the replica stores each published table, read to spell its keys as
    /// text.
    manifest: Vec<TableManifestEntry>,
    /// Why the key of each measured view a write cannot be traced through
    /// reaches no column of the table the write lands on.
    untraced: BTreeMap<String, UntracedKey>,
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
        let mut connection = ReplicaConnection::establish(":memory:")
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
        let mut registration = connection
            .registration()
            .map_err(DieselError::from)
            .map_err(database("Opening the registration".to_owned()))?;
        register_functions(registration.sqlite(), owner, &session)
            .map_err(database("Registering the replica functions".to_owned()))?;
        script.initialize_managed(&mut connection, FreshFiles::Both).map_err(|error| {
            match error {
                OpenError::Unavailable(source) => failed(source),
                OpenError::Execution(source) => {
                    database("Initializing the script".to_owned())(source.into())
                }
            }
        })?;
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
        let untraced = track_changes(&mut connection, &case.config.tables, &schema, &manifest)
            .map_err(database("Tracking the changed rows".to_owned()))?;
        Ok(Prepared::Ready(Self { connection, session, schema, options, manifest, untraced }))
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
            Ok(script) => {
                match script.register_managed(&mut self.connection) {
                    Ok(()) => script.into_vec(),
                    Err(source) => {
                        return Err(ParityError::Database {
                            role: self.options.get_acting_role().unwrap_or_default().to_owned(),
                            action: format!("Registering the helper functions of {statement}"),
                            source: source.into(),
                        });
                    }
                }
            }
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

    /// Run the translated write as one statement in a savepoint that is
    /// rolled back, collecting the keys of the rows of `table` it wrote.
    fn write(&mut self, table: &Table, translated: &[Statement]) -> Result<Measured, ParityError> {
        let Self { connection, options, untraced, .. } = self;
        let role = options.get_acting_role().unwrap_or_default();
        let database = |action: &str| {
            let action = action.to_owned();
            move |source| ParityError::Database { role: role.to_owned(), action, source }
        };
        connection.batch_execute("SAVEPOINT probe").map_err(database("Opening a savepoint"))?;
        let measured = (|| {
            // The case's write is runtime fixture data, as pg2sqlite
            // translated it.
            let group: ExecutionGroup<_> = ExecutionGroup::statements(translated);
            if let Err(error) = connection.execute_group(group) {
                let error = DieselError::from(error);
                if let DieselError::DatabaseError(_, information) = &error
                    && let Some(refusal) = postgres_error(information.message())
                {
                    return Measured::Rejected(refusal.sqlstate().to_string());
                }
                return Measured::Failed(error.to_string());
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
        if let Measured::Applied(_) = measured
            && let Some((view, reason)) = untraced.remove_entry(&table.name)
        {
            return Err(ParityError::UntracedView { view, reason });
        }
        Ok(measured)
    }
}

/// Register on `connection` the functions `options` name: each session
/// variable's function answering the current caller's setting as the replica
/// stores it, the RLS exemption, the UUID minter, and the `jsonb` functions
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
    if let Some(function) = options.get_rls_exemption_function() {
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

/// Where the rows a write to a measured relation changes are stored on the
/// replica.
struct Landing<'m, 'n> {
    /// The published table the write lands on.
    entry: &'m TableManifestEntry,
    /// The column of that table holding each key column, by key column name.
    columns: BTreeMap<&'n str, &'n str>,
}

/// The published table a write to the measured relation `table` lands on,
/// which is the table itself or the table a measured view writes through
/// every view it reads, and `None` when the replica holds neither.
///
/// # Errors
///
/// Returns [`UntracedKey`] when a view's key reaches no column of the table
/// the write lands on.
fn landing<'m, 'n>(
    table: &'n Table,
    schema: &'n ParserDB,
    manifest: &'m [TableManifestEntry],
) -> Result<Option<Landing<'m, 'n>>, UntracedKey> {
    let published = |name: &str| manifest.iter().find(|entry| entry.logical == name);
    if let Some(entry) = published(&table.name) {
        let columns = table.key.iter().map(|key| (key.as_str(), key.as_str())).collect();
        return Ok(Some(Landing { entry, columns }));
    }
    let name = ObjectName::from(vec![Ident::with_quote('"', &table.name)]);
    let Some(mut view) = resolve_view(&name, schema).map_err(Box::new)? else { return Ok(None) };
    let names = output_names(view, schema).map_err(Box::new)?;
    let mut positions = table
        .key
        .iter()
        .map(|key| {
            names
                .iter()
                .position(|name| name.as_deref() == Some(key.as_str()))
                .ok_or_else(|| UntracedKey::MissingColumn(key.clone()))
        })
        .collect::<Result<Vec<usize>, _>>()?;
    let computed = |index: usize| UntracedKey::Computed(table.key[index].clone());
    loop {
        let view_name = || view.stored_view_name().into_owned();
        if view
            .triggers(schema)
            .any(|trigger| matches!(trigger.period, Some(TriggerPeriod::InsteadOf)))
        {
            return Err(UntracedKey::TakenByTrigger(view_name()));
        }
        let base = view.updatable_base(schema).map_err(Error::from).map_err(Box::new)?;
        let Some(base) = base else { return Err(UntracedKey::NotUpdatable(view_name())) };
        // A foreign table, like a table the acting role holds no privilege
        // on, is left off the replica and publishes nothing to watch.
        let (written, columns) = match base.relation {
            WrittenRelation::View { view: inner, columns, .. } => {
                for (index, position) in positions.iter_mut().enumerate() {
                    *position =
                        columns.get(*position).copied().flatten().ok_or_else(|| computed(index))?;
                }
                view = inner;
                continue;
            }
            WrittenRelation::Table { table: written, columns, .. }
            | WrittenRelation::ForeignTable { table: written, columns, .. } => (written, columns),
        };
        let columns = table
            .key
            .iter()
            .zip(positions)
            .enumerate()
            .map(|(index, (key, position))| {
                let column =
                    columns.get(position).copied().flatten().ok_or_else(|| computed(index))?;
                Ok::<_, UntracedKey>((key.as_str(), column.column_name()))
            })
            .collect::<Result<_, _>>()?;
        return Ok(published(written.table_name()).map(|entry| Landing { entry, columns }));
    }
}

/// Record in `probe_changes` the logical name and key of every row a statement
/// writes to each of `tables` that the replica publishes, a measured view's
/// rows being those of the table a write through it lands on, named under the
/// view's key. Answers why each measured view whose key reaches no column of
/// the table a write through it lands on goes unwatched.
fn track_changes(
    connection: &mut ReplicaConnection,
    tables: &[Table],
    schema: &ParserDB,
    manifest: &[TableManifestEntry],
) -> QueryResult<BTreeMap<String, UntracedKey>> {
    connection.batch_execute(
        "CREATE TEMP TABLE probe_changes (logical TEXT NOT NULL, key TEXT NOT NULL)",
    )?;
    let mut untraced = BTreeMap::new();
    for (index, table) in tables.iter().enumerate() {
        let Landing { entry, columns } = match landing(table, schema, manifest) {
            Ok(Some(landing)) => landing,
            // A relation the translation omits has no physical counterpart to
            // watch.
            Ok(None) => continue,
            Err(reason) => {
                untraced.insert(table.name.clone(), reason);
                continue;
            }
        };
        let logical = format!("'{}'", table.name.replace('\'', "''"));
        let physical = identifier(&entry.physical);
        let key = |qualifier: &str| {
            table.key_sql(|key| replica_text(Some(entry), qualifier, columns[key]))
        };
        let old = format!("({logical}, {})", key("OLD"));
        let new = format!("({logical}, {})", key("NEW"));
        for (event, rows) in
            [("INSERT", new.clone()), ("UPDATE", format!("{old}, {new}")), ("DELETE", old)]
        {
            connection.batch_execute(&format!(
                "CREATE TEMP TRIGGER probe_{index}_{event} AFTER {event} ON main.{physical} \
                 BEGIN INSERT INTO probe_changes VALUES {rows}; END"
            ))?;
        }
    }
    Ok(untraced)
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
        fn case_setting_0() -> Nullable<Text>;
        fn case_setting_1() -> Nullable<Text>;
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
            let actual =
                diesel::select((case_setting_0(), case_setting_1(), connetto_write_exempt()))
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

    /// The caller and row of every row the case's callers see on its one
    /// table, three rows keyed 1 to 3, the replica refusing no acting role.
    fn visible_rows(case: &Case) -> Vec<(String, String)> {
        let table = &case.config.tables[0];
        let statement = table.statement(Shape::Select);
        let rows: Vec<String> = (1..=3).map(|key| format!("{}:{key}", table.name)).collect();
        let probes: Vec<Probe<'_>> = case
            .config
            .callers
            .iter()
            .flat_map(|caller| {
                rows.iter().map(|row| Probe { caller: &caller.name, row, statement: &statement })
            })
            .collect();
        let owner = case_options(case).expect("the case has a deployment");
        let replay = replay(case, &owner, &probes).expect("replays");
        assert_eq!(replay.refusal, None);
        replay
            .answers
            .into_iter()
            .filter(|answer| answer.answer == Answer::Visible)
            .map(|answer| (answer.caller, answer.row))
            .collect()
    }

    /// Every setting the case declares is answered in its declared type, so
    /// a `uuid` setting beside the text caller matches a blob-stored `uuid`
    /// column and each caller sees the rows PostgreSQL shows it.
    #[test]
    fn every_declared_setting_is_answered() {
        let case = Case::parse(
            "uuid_setting_beside_text_caller",
            "CREATE ROLE r0 NOLOGIN;
             CREATE ROLE r1 NOLOGIN;
             CREATE TABLE t0 (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, c0 UUID);
             ALTER TABLE t0 ENABLE ROW LEVEL SECURITY;
             GRANT SELECT ON t0 TO r0;
             GRANT SELECT ON t0 TO r1;
             CREATE POLICY p0 ON t0 USING (
                 (c0 = current_setting('app.user_uuid', true)::uuid)
                 OR (owner = current_setting('app.user_id', true))
             );",
            "INSERT INTO t0 (id, owner, c0) VALUES
                 (1, '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002'),
                 (2, '00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000003'),
                 (3, '00000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-000000000001');",
            r#"
            [[settings]]
            name = "app.user_id"
            value = { caller = "text" }

            [[settings]]
            name = "app.user_uuid"
            value = { caller = "uuid" }

            [[settings]]
            name = "app.subjects"
            value = { set = { element = "text", packing = { delimited = "," } } }

            [[tables]]
            name = "t0"
            key = ["id"]
            columns = ["id", "owner", "c0"]

            [[callers]]
            name = "c0"
            role = "r0"
            settings = { "app.user_id" = "00000000-0000-0000-0000-000000000001", "app.user_uuid" = "00000000-0000-0000-0000-000000000001", "app.subjects" = "" }

            [[callers]]
            name = "c1"
            role = "r1"
            settings = { "app.user_id" = "00000000-0000-0000-0000-000000000002", "app.user_uuid" = "00000000-0000-0000-0000-000000000002", "app.subjects" = "red" }
            "#,
        )
        .expect("the case reads");
        assert_eq!(
            visible_rows(&case),
            [
                ("c0".into(), "t0:1".into()),
                ("c0".into(), "t0:3".into()),
                ("c1".into(), "t0:1".into())
            ]
        );
    }

    /// A case declaring neither `app.user_id` nor `app.subjects` replays, its
    /// one setting answered and no undeclared setting paired.
    #[test]
    fn a_case_without_the_default_settings_replays() {
        let case = Case::parse(
            "tenant_only",
            "CREATE ROLE r0 NOLOGIN;
             CREATE TABLE t0 (id INTEGER PRIMARY KEY, tenant TEXT NOT NULL);
             ALTER TABLE t0 ENABLE ROW LEVEL SECURITY;
             GRANT SELECT ON t0 TO r0;
             CREATE POLICY p0 ON t0 USING (tenant = current_setting('app.tenant', true));",
            "INSERT INTO t0 (id, tenant) VALUES (1, 'red'), (2, 'blue'), (3, 'red');",
            r#"
            [[settings]]
            name = "app.tenant"
            value = { scalar = "text" }

            [[tables]]
            name = "t0"
            key = ["id"]
            columns = ["id", "tenant"]

            [[callers]]
            name = "c0"
            role = "r0"
            settings = { "app.tenant" = "red" }
            "#,
        )
        .expect("the case reads");
        assert_eq!(
            visible_rows(&case),
            [("c0".into(), "t0:1".into()), ("c0".into(), "t0:3".into())]
        );
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

    /// The answers of `app`, acting as `app_user` on the replica of `schema`,
    /// to each row and statement of `probes`, `resource_moves` measured with
    /// `key` over the rows of `resources` keyed 1 to 3.
    fn view_writes(
        schema: &str,
        key: &str,
        probes: &[(&str, &str)],
    ) -> Result<Vec<Answer>, ParityError> {
        let case = Case::parse(
            "view_writes",
            format!(
                "CREATE ROLE app_user NOLOGIN;
                 CREATE TABLE resources (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL);
                 {schema}"
            ),
            "INSERT INTO resources (id, parent_id) VALUES (1, 1), (2, 2), (3, 3);",
            format!(
                r#"
                [[tables]]
                name = "resource_moves"
                key = ["{key}"]
                columns = ["{key}", "parent_id"]

                [[callers]]
                name = "app"
                role = "app_user"
                settings = {{ "app.user_id" = "1", "app.subjects" = "" }}
                "#
            ),
        )
        .expect("the case reads");
        let owner = case_options(&case).expect("the case has a deployment");
        let probes: Vec<Probe<'_>> = probes
            .iter()
            .map(|&(row, statement)| Probe { caller: "app", row, statement })
            .collect();
        let replay = replay(&case, &owner, &probes)?;
        assert_eq!(replay.refusal, None);
        Ok(replay.answers.into_iter().map(|answer| answer.answer).collect())
    }

    const MOVE_ONE: &str = r#"UPDATE "resource_moves" SET "parent_id" = 2 WHERE "id" = 1"#;

    const OWNER_RIGHTS: &str = "CREATE ROLE authz_writer NOLOGIN BYPASSRLS;
         ALTER TABLE resources ENABLE ROW LEVEL SECURITY;
         GRANT SELECT ON resources TO app_user;
         GRANT SELECT, UPDATE (parent_id) ON resources TO authz_writer;
         CREATE POLICY resources_select ON resources FOR SELECT TO app_user USING (parent_id > 0);
         CREATE VIEW resource_moves WITH (check_option = cascaded) AS
             SELECT id, parent_id, id + 10 AS code FROM resources WHERE parent_id > 0;
         ALTER VIEW resource_moves OWNER TO authz_writer;
         GRANT SELECT, UPDATE (parent_id) ON resource_moves TO app_user;";

    const NESTED: &str = "CREATE VIEW resource_parents AS SELECT parent_id, id FROM resources;
         CREATE VIEW resource_moves AS
             SELECT id + 10 AS code, parent_id, id FROM resource_parents WHERE parent_id > 0;
         GRANT SELECT, UPDATE ON resources, resource_parents, resource_moves TO app_user;";

    /// A write through a measured view changes the rows of the table it
    /// lands on, named under the view's key, through every view it reads,
    /// leaves the rows it does not reach skipped, and a row the view's check
    /// option refuses rejected.
    #[test]
    fn a_write_through_a_view_changes_its_rows() {
        let check_option = r#"UPDATE "resource_moves" SET "parent_id" = 0 WHERE "id" = 2"#;
        let probes = [
            ("resource_moves:1", MOVE_ONE),
            ("resource_moves:2", MOVE_ONE),
            ("resource_moves:2", check_option),
        ];
        assert_eq!(
            view_writes(OWNER_RIGHTS, "id", &probes).expect("replays"),
            [Answer::Changed, Answer::Skipped, Answer::Rejected("44000".into())]
        );
        assert_eq!(
            view_writes(NESTED, "id", &probes[..2]).expect("replays"),
            [Answer::Changed, Answer::Skipped]
        );
    }

    /// A write applied through a measured view whose key the replica cannot
    /// trace to a column of the table the write lands on names no rows, so
    /// the replay refuses it with the reason, and a write through such a view
    /// that SQLite refuses answers that refusal.
    #[test]
    fn an_untraced_view_key_cannot_measure_writes() {
        let probe = [("resource_moves:1", MOVE_ONE)];
        let untraced = |schema: &str, key: &str| {
            match view_writes(schema, key, &probe) {
                Err(ParityError::UntracedView { view, reason }) if view == "resource_moves" => {
                    reason.to_string()
                }
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(untraced(OWNER_RIGHTS, "code"), "key column code computes its value");
        assert_eq!(untraced(NESTED, "code"), "key column code computes its value");
        assert_eq!(untraced(NESTED, "nope"), "the view outputs no column nope");
        let distinct = "CREATE VIEW resource_moves AS SELECT DISTINCT id, parent_id FROM resources;
             GRANT SELECT, UPDATE ON resources, resource_moves TO app_user;";
        assert_eq!(
            untraced(
                &format!(
                    "{distinct}
                     CREATE FUNCTION move_resource() RETURNS trigger LANGUAGE plpgsql AS $$
                     BEGIN
                         UPDATE resources SET parent_id = NEW.parent_id WHERE id = OLD.id;
                         RETURN NEW;
                     END $$;
                     CREATE TRIGGER resource_moves_update INSTEAD OF UPDATE ON resource_moves
                         FOR EACH ROW EXECUTE FUNCTION move_resource();"
                ),
                "id"
            ),
            "an INSTEAD OF trigger takes the write for view resource_moves"
        );
        assert!(matches!(
            view_writes(distinct, "id", &probe).as_deref(),
            Ok([Answer::Rejected(sqlstate)]) if sqlstate == "55000"
        ));
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
