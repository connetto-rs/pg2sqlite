//! The emitted script, split by how long each statement's effect lasts.
//!
//! A SQLite database file keeps its schema, while pragmas and `TEMP` objects
//! are connection state lost on close, so the file part runs once and the
//! setup and connection parts on every open.

use sqlparser::ast::Statement;

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::{Error, TranslationRefusal},
    initialization::InitializationPlan,
};

/// Why the connection part cannot be produced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionError {
    /// A replayed statement reads a relation the final schema no longer has.
    #[error("connection replay needs dropped relation `{relation}` at statement {statement}")]
    UnavailableRelation {
        /// The dropped relation.
        relation: String,
        /// The zero-based source statement position.
        statement: usize,
    },
    /// A replayed statement reads a column the final schema no longer has.
    #[error(
        "connection replay needs dropped column `{column}` of `{relation}` at statement {statement}"
    )]
    UnavailableColumn {
        /// The relation that held the column.
        relation: String,
        /// The dropped column.
        column: String,
        /// The zero-based source statement position.
        statement: usize,
    },
    /// Replay cannot preserve the source semantics.
    #[error(transparent)]
    Translation(#[from] TranslationRefusal),
}

/// Which newly created database files an initialization runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshFiles {
    /// The main database file.
    Main,
    /// The attached database file.
    Attached,
    /// Every database file the translation creates.
    Both,
}

/// A helper the replica runs as a table-valued function, as PostgreSQL calls
/// it instead of inlining its body.
///
/// A call site reads it as `name(arguments)` in a `FROM` list, binding each
/// argument to the parameter `?k` of `body` at the same position, and reads
/// its rows under `columns`. The body runs as a statement of its own, so its
/// relations open no cursor in the program of the statement calling it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperFunction<S = Statement> {
    /// The name a call site reads the function by.
    pub name: String,
    /// The parameters, which a call site passes as arguments in this order.
    pub parameters: Vec<String>,
    /// The columns of each row the function answers.
    pub columns: Vec<String>,
    /// The query answering a call, reading each argument as `?k`.
    pub body: S,
}

/// A translation's statements, in parts run in field order.
///
/// Every connection runs [`setup`](Self::setup). The connection creating the
/// database then runs the full file initialization through
/// [`files`](Self::files) or [`initialize`](Self::initialize), and every
/// later connection runs [`connect`](Self::connect) instead. Every connection
/// registers [`functions`](Self::functions) before running any part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script<S = Statement> {
    /// Connection pragmas, `PRAGMA case_sensitive_like = 1` whenever the
    /// script is not empty, so `LIKE` matches case as PostgreSQL does, and
    /// `PRAGMA foreign_keys = 1` when a table declares a foreign key, since
    /// SQLite enforces one only under that pragma.
    pub setup: Vec<S>,
    /// The source-ordered initialization of the newly created database files,
    /// with a prevalidated selection per newly created file.
    pub files: InitializationPlan<S>,
    /// The committed `TEMP` objects and rows the source leaves behind,
    /// recreated on a connection to an existing database file.
    pub connection: Result<Vec<S>, ConnectionError>,
    /// The statements to run before every `COMMIT` of a transaction that
    /// writes the database, empty unless the source declares a `DEFERRABLE
    /// INITIALLY DEFERRED` constraint trigger.
    ///
    /// They run the checks such a trigger queued, against the rows as they
    /// stand, and fail with the trigger's own message on a violation, leaving
    /// the transaction open to roll back as PostgreSQL rolls it back. A
    /// `COMMIT` with a check still queued fails with `FOREIGN KEY constraint
    /// failed`, and so does an autocommit write that queues one, so writes to
    /// a table carrying such a trigger run inside a transaction. The script's
    /// own translated `COMMIT`s already run them.
    pub before_commit: Vec<S>,
    /// The helpers the script's statements call as table-valued functions,
    /// connection state every connection registers before running any part.
    pub functions: Vec<HelperFunction<S>>,
}

impl<S> Script<S> {
    /// Whether no part holds a statement.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.setup.is_empty()
            && self.files.is_empty()
            && self.connection.as_ref().is_ok_and(Vec::is_empty)
    }

    /// The statements a connection creating the database runs, in order,
    /// every file initialized from scratch.
    pub fn iter(&self) -> impl Iterator<Item = &S> {
        self.setup.iter().chain(self.files.iter())
    }

    /// The statements a connection creating the selected fresh files runs, in
    /// source order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Initialization`] when the selection needs source
    /// history the target file does not have, or names a file the
    /// translation does not create.
    pub fn initialize(&self, files: FreshFiles) -> Result<impl Iterator<Item = &S>, Error> {
        let initialization = self.files.select(files)?;
        Ok(self.setup.iter().chain(initialization))
    }

    /// The statements a connection to an existing database file runs, in
    /// order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Connection`] when the connection part needs a
    /// relation or column the final schema dropped, or would repeat a write
    /// to the database file.
    pub fn connect(&self) -> Result<impl Iterator<Item = &S>, Error> {
        let connection =
            self.connection.as_ref().map_err(|error| Error::Connection(error.clone()))?;
        Ok(self.setup.iter().chain(connection))
    }

    /// The statements a connection creating the database runs, in order,
    /// every file initialized from scratch.
    #[must_use]
    pub fn into_vec(self) -> Vec<S> {
        let mut statements = self.setup;
        statements.extend(self.files.into_statements());
        statements
    }

    /// Applies `f` to every statement, keeping each in its part.
    #[must_use]
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> Script<T> {
        Script {
            setup: self.setup.into_iter().map(&mut f).collect(),
            files: self.files.map(&mut f),
            connection: self
                .connection
                .map(|statements| statements.into_iter().map(&mut f).collect()),
            before_commit: self.before_commit.into_iter().map(&mut f).collect(),
            functions: self
                .functions
                .into_iter()
                .map(|function| {
                    HelperFunction {
                        name: function.name,
                        parameters: function.parameters,
                        columns: function.columns,
                        body: f(function.body),
                    }
                })
                .collect(),
        }
    }
}

#[cfg(feature = "diesel")]
impl<S: core::fmt::Display> Script<S> {
    /// Registers every helper function on `connection`, which a connection
    /// does before running any part of the script.
    ///
    /// # Errors
    ///
    /// The error SQLite answers when it refuses a registration.
    pub fn register_functions(
        &self,
        connection: &mut diesel::sqlite::SqliteConnection,
    ) -> diesel::QueryResult<()> {
        self.functions.iter().try_for_each(|function| function.register(connection))
    }
}

#[cfg(feature = "diesel")]
impl<S: core::fmt::Display> HelperFunction<S> {
    /// Registers the function on `connection` as a table-valued function
    /// running its body as a statement of its own on that connection, with
    /// each argument bound to its parameter as SQLite passed it.
    ///
    /// # Errors
    ///
    /// The error SQLite answers when it refuses the registration.
    pub fn register(
        &self,
        connection: &mut diesel::sqlite::SqliteConnection,
    ) -> diesel::QueryResult<()> {
        use diesel::RunQueryDsl;
        use diesel_dynamic_schema::dynamic_value::DynamicRow;

        let body = self.body.to_string();
        connection.register_table_valued_function(
            ResultColumns { function: &self.name, columns: &self.columns },
            Parameters(self.parameters.clone()),
            diesel::sqlite::SqliteFunctionBehavior::INNOCUOUS,
            move |connection, arguments: Vec<StoredValue>| {
                let query = arguments.iter().fold(
                    diesel::sql_query(body.as_str()).into_boxed::<diesel::sqlite::Sqlite>(),
                    |query, argument| argument.bind_to(query),
                );
                let rows = query.load::<DynamicRow<StoredValue>>(connection)?;
                Ok(rows.into_iter().map(|row| StoredRow(row.into_iter().collect())))
            },
        )
    }
}

/// The relation declaring a helper function, whose columns and parameters
/// are named at run time.
#[cfg(feature = "diesel")]
struct HelperRelation;

/// The columns a helper function returns.
#[cfg(feature = "diesel")]
struct ResultColumns<'s> {
    function: &'s str,
    columns: &'s [String],
}

#[cfg(feature = "diesel")]
impl diesel::sqlite::SqliteTableFunctionColumns<HelperRelation> for ResultColumns<'_> {
    type SqlType = ();

    fn function_name(&self) -> diesel::QueryResult<&str> {
        Ok(self.function)
    }

    fn column_names(&self) -> Vec<&str> {
        self.columns.iter().map(String::as_str).collect()
    }

    fn relation_column_names(&self) -> Option<Vec<&str>> {
        None
    }
}

/// The parameters of a helper function, each argument read in the storage
/// class SQLite passes it in.
#[cfg(feature = "diesel")]
struct Parameters(Vec<String>);

#[cfg(feature = "diesel")]
impl diesel::sqlite::SqliteTableFunctionParameters<HelperRelation, Vec<StoredValue>>
    for Parameters
{
    fn parameter_names(&self, _function_name: &str) -> diesel::QueryResult<Vec<&str>> {
        Ok(self.0.iter().map(String::as_str).collect())
    }

    fn arguments<'a>(
        &self,
        row: &impl diesel::row::Row<'a, diesel::sqlite::Sqlite>,
    ) -> diesel::deserialize::Result<Vec<StoredValue>> {
        use diesel::{deserialize::FromSql, row::Field, sqlite::Sqlite};
        use diesel_dynamic_schema::dynamic_value::Any;
        (0..row.field_count())
            .map(|index| {
                let field = row.get(index);
                <StoredValue as FromSql<Any, Sqlite>>::from_nullable_sql(
                    field.as_ref().and_then(Field::value),
                )
            })
            .collect()
    }
}

/// One row a helper function answers.
#[cfg(feature = "diesel")]
struct StoredRow(Vec<StoredValue>);

#[cfg(feature = "diesel")]
impl diesel::sqlite::SqliteTableFunctionRow<()> for StoredRow {
    fn for_each_value(
        &self,
        mut sink: impl FnMut(diesel::sqlite::SqliteBindValue<'_>),
    ) -> diesel::QueryResult<()> {
        use diesel::sqlite::SqliteBindValue;
        for value in &self.0 {
            sink(match value {
                StoredValue::Null => SqliteBindValue::from(None::<i64>),
                StoredValue::Integer(value) => SqliteBindValue::from(*value),
                StoredValue::Real(value) => SqliteBindValue::from(*value),
                StoredValue::Text(value) => SqliteBindValue::from(value.as_str()),
                StoredValue::Blob(value) => SqliteBindValue::from(value.as_slice()),
            });
        }
        Ok(())
    }
}

/// A value in the storage class SQLite holds it in.
#[cfg(feature = "diesel")]
enum StoredValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[cfg(feature = "diesel")]
impl StoredValue {
    /// `query` with the value bound to its next placeholder, typed as its
    /// storage class.
    fn bind_to<'f, Query>(
        &'f self,
        query: diesel::query_builder::BoxedSqlQuery<'f, diesel::sqlite::Sqlite, Query>,
    ) -> diesel::query_builder::BoxedSqlQuery<'f, diesel::sqlite::Sqlite, Query> {
        use diesel::sql_types::{BigInt, Binary, Double, Nullable, Text};
        match self {
            Self::Null => query.bind::<Nullable<BigInt>, _>(None::<i64>),
            Self::Integer(value) => query.bind::<BigInt, _>(value),
            Self::Real(value) => query.bind::<Double, _>(value),
            Self::Text(value) => query.bind::<Text, _>(value),
            Self::Blob(value) => query.bind::<Binary, _>(value),
        }
    }
}

#[cfg(feature = "diesel")]
impl diesel::deserialize::FromSql<diesel_dynamic_schema::dynamic_value::Any, diesel::sqlite::Sqlite>
    for StoredValue
{
    /// `value` in its storage class, SQLite holding anything neither an
    /// integer, a real nor text as a blob.
    fn from_sql(
        value: diesel::sqlite::SqliteValue<'_, '_, '_>,
    ) -> diesel::deserialize::Result<Self> {
        use diesel::{
            deserialize::FromSql,
            sql_types::{BigInt, Binary, Double, Text},
            sqlite::{Sqlite, SqliteType},
        };
        match value.value_type() {
            Some(SqliteType::Long) => {
                <i64 as FromSql<BigInt, Sqlite>>::from_sql(value).map(Self::Integer)
            }
            Some(SqliteType::Double) => {
                <f64 as FromSql<Double, Sqlite>>::from_sql(value).map(Self::Real)
            }
            Some(SqliteType::Text) => {
                <String as FromSql<Text, Sqlite>>::from_sql(value).map(Self::Text)
            }
            _ => <Vec<u8> as FromSql<Binary, Sqlite>>::from_sql(value).map(Self::Blob),
        }
    }

    fn from_nullable_sql(
        value: Option<diesel::sqlite::SqliteValue<'_, '_, '_>>,
    ) -> diesel::deserialize::Result<Self> {
        value.map_or(Ok(Self::Null), Self::from_sql)
    }
}
