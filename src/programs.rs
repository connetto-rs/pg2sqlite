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
    QueryableByName, RunQueryDsl,
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
    /// A trigger whose definition names no event SQLite fires it on.
    #[error("trigger `{name}` in `{schema}` names no event")]
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
/// Returns [`ProgramsError`] when the schema cannot be read, or a trigger's
/// definition names no event.
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
            match object.kind.as_str() {
                "table" => {
                    held.insert(object.name.to_ascii_lowercase());
                }
                "view" => {
                    held.insert(object.name.to_ascii_lowercase());
                    views.push((schema.as_str(), object.name));
                }
                "trigger" => triggers.push((schema.as_str(), object)),
                _ => {}
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
                        let message = refusal(error)?;
                        let statement = format!("UPDATE {target}");
                        unprepared.push(UnpreparedProgram { statement, programs, message });
                        continue;
                    }
                }
            }
        };
        prepare(connection, statement, programs, &mut unprepared)?;
    }
    for (schema, view) in views {
        let statement = format!("SELECT * FROM {}.{}", quoted(schema), quoted(&view));
        prepare(connection, statement, vec![view], &mut unprepared)?;
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
) -> Result<(), ProgramsError> {
    // The statement is built from the schema at run time, which the typed DSL
    // cannot name.
    match diesel::sql_query(format!("EXPLAIN {statement}")).execute(connection) {
        Ok(_) => Ok(()),
        Err(error) => {
            let message = refusal(error)?;
            unprepared.push(UnpreparedProgram { statement, programs, message });
            Ok(())
        }
    }
}

/// SQLite's message refusing a statement, or `error` when the connection
/// failed otherwise.
fn refusal(error: DieselError) -> Result<String, ProgramsError> {
    match error {
        DieselError::DatabaseError(_, information) => Ok(information.message().to_owned()),
        error => Err(error.into()),
    }
}

#[derive(QueryableByName)]
struct Name {
    #[diesel(sql_type = Text)]
    name: String,
}

/// The schemas `connection` holds, in the order SQLite searches them.
fn schemas(connection: &mut SqliteConnection) -> Result<Vec<String>, DieselError> {
    let names: Vec<Name> = diesel::sql_query(
        "SELECT name FROM pragma_database_list WHERE name <> 'temp' ORDER BY seq",
    )
    .load(connection)?;
    // The temporary schema is searched first, and listed only once something
    // creates it.
    Ok(core::iter::once("temp".to_owned()).chain(names.into_iter().map(|name| name.name)).collect())
}

#[derive(QueryableByName)]
struct Object {
    #[diesel(sql_type = Text)]
    kind: String,
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Text)]
    relation: String,
    #[diesel(sql_type = Nullable<Text>)]
    sql: Option<String>,
}

/// The tables, views and triggers of `schema`.
fn objects(connection: &mut SqliteConnection, schema: &str) -> Result<Vec<Object>, DieselError> {
    diesel::sql_query(format!(
        "SELECT type AS kind, name, tbl_name AS relation, sql FROM {}.sqlite_schema \
         WHERE type IN ('table', 'view', 'trigger') ORDER BY name",
        quoted(schema)
    ))
    .load(connection)
}

/// The columns of `relation` in `schema` a write may set.
fn columns(
    connection: &mut SqliteConnection,
    schema: &str,
    relation: &str,
) -> Result<Vec<String>, DieselError> {
    let names: Vec<Name> = diesel::sql_query(
        "SELECT name FROM pragma_table_xinfo(?, ?) WHERE hidden = 0 ORDER BY cid",
    )
    .bind::<Text, _>(relation)
    .bind::<Text, _>(schema)
    .load(connection)?;
    Ok(names.into_iter().map(|name| name.name).collect())
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
    /// The event the `CREATE TRIGGER` statement `sql` fires on, read from its
    /// head `CREATE [TEMP] TRIGGER [IF NOT EXISTS] [schema.]name [timing]
    /// event`.
    fn of(sql: &str) -> Option<Self> {
        let mut tokens = Tokens { rest: sql };
        while !tokens.next()?.is_word("trigger") {}
        let mut name = tokens.next()?;
        if name.is_word("if") {
            let mut lookahead = Tokens { rest: tokens.rest };
            if lookahead.next()?.is_word("not") && lookahead.next()?.is_word("exists") {
                tokens = lookahead;
                name = tokens.next()?;
            }
        }
        if !matches!(name, Token::Word(_) | Token::Quoted) {
            return None;
        }
        let mut token = tokens.next()?;
        if token == Token::Other('.') {
            tokens.next()?;
            token = tokens.next()?;
        }
        while ["before", "after", "instead", "of"].iter().any(|word| token.is_word(word)) {
            token = tokens.next()?;
        }
        [("delete", Self::Delete), ("insert", Self::Insert), ("update", Self::Update)]
            .into_iter()
            .find_map(|(word, event)| token.is_word(word).then_some(event))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token<'s> {
    Word(&'s str),
    Quoted,
    Other(char),
}

impl Token<'_> {
    fn is_word(self, word: &str) -> bool {
        matches!(self, Token::Word(read) if read.eq_ignore_ascii_case(word))
    }
}

/// The tokens of SQLite text, skipping whitespace and comments.
struct Tokens<'s> {
    rest: &'s str,
}

impl<'s> Tokens<'s> {
    fn next(&mut self) -> Option<Token<'s>> {
        loop {
            self.rest = self.rest.trim_start();
            if let Some(comment) = self.rest.strip_prefix("--") {
                self.rest = comment.find('\n').map_or("", |end| &comment[end..]);
            } else if let Some(comment) = self.rest.strip_prefix("/*") {
                self.rest = comment.find("*/").map_or("", |end| &comment[end + 2..]);
            } else {
                break;
            }
        }
        let first = self.rest.chars().next()?;
        let closing = match first {
            '"' => Some('"'),
            '\'' => Some('\''),
            '`' => Some('`'),
            '[' => Some(']'),
            _ => None,
        };
        if let Some(closing) = closing {
            let mut position = first.len_utf8();
            loop {
                let end = position + self.rest[position..].find(closing)?;
                position = end + closing.len_utf8();
                // A doubled closing quote stands for itself, except in
                // brackets.
                if closing == ']' || !self.rest[position..].starts_with(closing) {
                    break;
                }
                position += closing.len_utf8();
            }
            self.rest = &self.rest[position..];
            return Some(Token::Quoted);
        }
        let word_end = self
            .rest
            .find(|character: char| {
                !(character.is_ascii_alphanumeric()
                    || character == '_'
                    || character == '$'
                    || !character.is_ascii())
            })
            .unwrap_or(self.rest.len());
        if word_end == 0 {
            self.rest = &self.rest[first.len_utf8()..];
            return Some(Token::Other(first));
        }
        let word = &self.rest[..word_end];
        self.rest = &self.rest[word_end..];
        Some(Token::Word(word))
    }
}
