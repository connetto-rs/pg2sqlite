//! The triggers and views a SQLite connection holds that SQLite refuses to
//! prepare.
//!
//! SQLite compiles a trigger when it prepares a write firing it and a view
//! when it prepares a read of it, so a schema loads whole and fails on the
//! first statement a client prepares.
//!
//! ```
//! use diesel::{connection::SimpleConnection, prelude::*};
//! use pg2sqlite::programs::unprepared_programs;
//!
//! let mut connection = SqliteConnection::establish(":memory:")?;
//! let mut script = String::from(
//!     "CREATE TABLE t (x);
//!      CREATE VIEW v0 AS WITH c (x) AS (SELECT x FROM t) SELECT x FROM c;",
//! );
//! for level in 1..=10 {
//!     let previous = level - 1;
//!     script.push_str(&format!(
//!         "CREATE VIEW v{level} AS SELECT x FROM v{previous} UNION ALL SELECT x FROM v{previous};"
//!     ));
//! }
//! script.push_str("CREATE TRIGGER t_deleted AFTER DELETE ON t BEGIN SELECT x FROM v10; END;");
//! connection.batch_execute(&script)?;
//! let unprepared = unprepared_programs(&mut connection)?;
//! assert!(unprepared.iter().any(|program| program.programs == ["t_deleted"]));
//! assert!(unprepared.iter().any(|program| program.programs == ["v10"]));
//! assert!(unprepared.iter().all(|program| program.programs != ["v1"]));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use alloc::collections::{BTreeMap, BTreeSet};

use diesel::{
    RunQueryDsl,
    dsl::sql,
    result::Error as DieselError,
    sql_types::{Nullable, Text},
    sqlite::SqliteConnection,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// A statement SQLite refuses to prepare, and the triggers or the view it
/// compiles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnpreparedProgram {
    /// The write firing the triggers, or the read of the view.
    pub statement: String,
    /// The triggers the write fires, or the view read.
    pub programs: Vec<String>,
    /// What SQLite answers preparing the statement.
    pub message: String,
}

impl core::fmt::Display for UnpreparedProgram {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "`{}`, compiling {}, fails to prepare: {}",
            self.statement,
            self.programs.join(", "),
            self.message
        )
    }
}

/// Why the programs of a connection could not be listed.
#[derive(Debug, thiserror::Error)]
pub enum ProgramsError {
    /// The connection failed other than by refusing a statement.
    #[error(transparent)]
    Connection(#[from] DieselError),
    /// A trigger whose stored definition does not open the way SQLite writes
    /// one, which only a write to `sqlite_schema` leaves.
    #[error("trigger `{name}` in `{schema}` is not stored as SQLite writes one")]
    UnreadTrigger {
        /// The schema holding the trigger.
        schema: String,
        /// The trigger's name.
        name: String,
    },
}

/// Every trigger and view `connection` holds that its SQLite refuses to
/// prepare, in every schema the connection has attached, each with the
/// statement compiling it and SQLite's message.
///
/// # Errors
///
/// Returns [`ProgramsError`] when the schema cannot be read, or a trigger is
/// not stored as SQLite writes one.
pub fn unprepared_programs(
    connection: &mut SqliteConnection,
) -> Result<Vec<UnpreparedProgram>, ProgramsError> {
    let schemas = schemas(connection)?;
    let mut relations: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut views = Vec::new();
    let mut triggers = Vec::new();
    for schema in &schemas {
        let held = relations.entry(schema.as_str()).or_default();
        for object in objects(connection, schema)? {
            if object.kind == "trigger" {
                triggers.push((schema.as_str(), object));
            } else {
                held.insert(object.name.to_ascii_lowercase());
                if object.kind == "view" {
                    views.push((schema.as_str(), object.name));
                }
            }
        }
    }
    let mut writes: BTreeMap<(&str, String, Event), (String, Vec<String>)> = BTreeMap::new();
    for (schema, trigger) in triggers {
        let Some(event) = trigger.sql.as_deref().and_then(Event::of) else {
            return Err(ProgramsError::UnreadTrigger {
                schema: schema.to_owned(),
                name: trigger.name,
            });
        };
        let relation = trigger.relation.to_ascii_lowercase();
        // A temporary trigger fires on the first relation its name finds.
        let target = if schema == "temp" {
            schemas
                .iter()
                .map(String::as_str)
                .find(|holder| relations.get(holder).is_some_and(|held| held.contains(&relation)))
                .unwrap_or(schema)
        } else {
            schema
        };
        writes
            .entry((target, relation, event))
            .or_insert_with(|| (trigger.relation, Vec::new()))
            .1
            .push(trigger.name);
    }
    let mut unprepared = Vec::new();
    for ((schema, _, event), (relation, programs)) in writes {
        let target = format!("{}.{}", quoted(schema), quoted(&relation));
        let statement = match event {
            Event::Delete => format!("DELETE FROM {target}"),
            Event::Insert => format!("INSERT INTO {target} DEFAULT VALUES"),
            Event::Update => {
                match columns(connection, schema, &relation) {
                    Ok(columns) => {
                        let assignments: Vec<String> = columns
                            .iter()
                            .map(|column| format!("{0} = {0}", quoted(column)))
                            .collect();
                        format!("UPDATE {target} SET {}", assignments.join(", "))
                    }
                    // A view's columns are read by preparing its query.
                    Err(error) => {
                        let message = error.to_string();
                        let statement = format!("UPDATE {target}");
                        unprepared.push(UnpreparedProgram { statement, programs, message });
                        continue;
                    }
                }
            }
        };
        prepare(connection, statement, programs, &mut unprepared);
    }
    for (schema, view) in views {
        let statement = format!("SELECT * FROM {}.{}", quoted(schema), quoted(&view));
        prepare(connection, statement, vec![view], &mut unprepared);
    }
    Ok(unprepared)
}

/// Prepares `statement` under `EXPLAIN`, which compiles every program it
/// runs and executes none, listing it in `unprepared` when SQLite refuses.
fn prepare(
    connection: &mut SqliteConnection,
    statement: String,
    programs: Vec<String>,
    unprepared: &mut Vec<UnpreparedProgram>,
) {
    // The statement is built from the schema at run time, which the typed DSL
    // cannot name.
    if let Err(error) = diesel::sql_query(format!("EXPLAIN {statement}")).execute(connection) {
        unprepared.push(UnpreparedProgram { statement, programs, message: error.to_string() });
    }
}

/// The schemas `connection` holds, in the order SQLite searches them.
fn schemas(connection: &mut SqliteConnection) -> Result<Vec<String>, DieselError> {
    let names: Vec<String> = diesel::select(sql::<Text>(
        "name FROM pragma_database_list WHERE name <> 'temp' ORDER BY seq",
    ))
    .load(connection)?;
    // The temporary schema is searched first, and listed only once something
    // creates it.
    Ok(core::iter::once("temp".to_owned()).chain(names).collect())
}

struct Object {
    kind: String,
    name: String,
    relation: String,
    sql: Option<String>,
}

/// The tables, views and triggers of `schema`.
fn objects(connection: &mut SqliteConnection, schema: &str) -> Result<Vec<Object>, DieselError> {
    let rows: Vec<(String, String, String, Option<String>)> =
        diesel::select(sql::<(Text, Text, Text, Nullable<Text>)>(&format!(
            "type, name, tbl_name, sql FROM {}.sqlite_schema \
             WHERE type IN ('table', 'view', 'trigger') ORDER BY name",
            quoted(schema)
        )))
        .load(connection)?;
    Ok(rows
        .into_iter()
        .map(|(kind, name, relation, sql)| Object { kind, name, relation, sql })
        .collect())
}

/// The columns of `relation` in `schema` a write may set.
fn columns(
    connection: &mut SqliteConnection,
    schema: &str,
    relation: &str,
) -> Result<Vec<String>, DieselError> {
    diesel::select(
        sql::<Text>("name FROM pragma_table_xinfo(")
            .bind::<Text, _>(relation)
            .sql(", ")
            .bind::<Text, _>(schema)
            .sql(") WHERE hidden = 0 ORDER BY cid"),
    )
    .load(connection)
}

/// `name` as a quoted SQLite identifier.
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The write a trigger fires on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Event {
    Delete,
    Insert,
    Update,
}

impl Event {
    /// The event the trigger `sql` fires on, read from the head `CREATE
    /// TRIGGER name [timing] event` SQLite stores every trigger under.
    fn of(sql: &str) -> Option<Self> {
        let mut tokens = Tokens { rest: sql.strip_prefix("CREATE TRIGGER ")? };
        tokens.next()?;
        let mut word = tokens.next()?;
        while ["before", "after", "instead", "of"]
            .iter()
            .any(|timing| word.eq_ignore_ascii_case(timing))
        {
            word = tokens.next()?;
        }
        [("delete", Self::Delete), ("insert", Self::Insert), ("update", Self::Update)]
            .into_iter()
            .find_map(|(name, event)| word.eq_ignore_ascii_case(name).then_some(event))
    }
}

/// The tokens of SQLite text, skipping whitespace and comments, a quoted
/// identifier with its quotes so it never reads as a keyword.
struct Tokens<'s> {
    rest: &'s str,
}

impl<'s> Tokens<'s> {
    fn next(&mut self) -> Option<&'s str> {
        loop {
            self.rest =
                self.rest.trim_start_matches(|character: char| character.is_ascii_whitespace());
            if let Some(comment) = self.rest.strip_prefix("--") {
                self.rest = comment.find('\n').map_or("", |end| &comment[end..]);
            } else if let Some(comment) = self.rest.strip_prefix("/*") {
                self.rest = comment.find("*/").map_or("", |end| &comment[end + 2..]);
            } else {
                break;
            }
        }
        let closing = match self.rest.chars().next()? {
            '"' => Some('"'),
            '\'' => Some('\''),
            '`' => Some('`'),
            '[' => Some(']'),
            _ => None,
        };
        let end = match closing {
            Some(closing) => {
                let mut position = 1;
                loop {
                    position += self.rest[position..].find(closing)? + 1;
                    // A doubled closing quote stands for itself, except in
                    // brackets.
                    if closing == ']' || !self.rest[position..].starts_with(closing) {
                        break position;
                    }
                    position += 1;
                }
            }
            None => {
                self.rest
                    .find(|character: char| {
                        !(character.is_ascii_alphanumeric()
                            || character == '_'
                            || character == '$'
                            || !character.is_ascii())
                    })
                    .unwrap_or(self.rest.len())
            }
        };
        let (token, rest) = self.rest.split_at(end);
        self.rest = rest;
        Some(token)
    }
}
