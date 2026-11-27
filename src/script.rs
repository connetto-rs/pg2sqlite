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

/// Why a script cannot open a managed connection.
#[cfg(feature = "managed")]
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The script cannot produce the part the connection runs.
    #[error(transparent)]
    Unavailable(#[from] Error),
    /// The connection refused a registration or a statement.
    #[error(transparent)]
    Execution(#[from] pg2sqlite_runtime::ExecutionError),
}

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

/// A helper the replica runs as a table-valued function, its body a
/// translated statement unless `S` names another form.
pub type HelperFunction<S = Statement> = pg2sqlite_runtime::HelperFunction<S>;

/// One logical source statement of a script part, the statements emitted for
/// it or the transaction control it runs as.
pub type Group<S = Statement> = pg2sqlite_runtime::ExecutionGroup<Vec<S>>;

/// A translation's statements, in parts run in field order.
///
/// Every connection runs [`setup`](Self::setup). The connection creating the
/// database then runs the full file initialization through
/// [`files`](Self::files) or [`initialize`](Self::initialize), and every
/// later connection runs [`connect`](Self::connect) instead. Every connection
/// registers [`functions`](Self::functions) before running any part.
///
/// The translation calls clock readers that only a
/// [`ManagedConnection`](pg2sqlite_runtime::ManagedConnection) answers, from
/// the instants it samples at each logical statement and transaction start.
/// [`initialize_managed`](Self::initialize_managed) and
/// [`connect_managed`](Self::connect_managed) run the parts as such groups.
/// The flat statement iterators export the text, and executing that text
/// statement by statement loses those boundaries.
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
    /// recreated on a connection to an existing database file, inside the
    /// transactions the source wrote them in.
    pub connection: Result<Vec<Group<S>>, ConnectionError>,
    /// The statements a managed connection runs before every commit ending
    /// a transaction, empty unless the source declares a `DEFERRABLE
    /// INITIALLY DEFERRED` constraint trigger.
    ///
    /// They run the checks such a trigger queued, against the rows as they
    /// stand, and fail with the trigger's own message on a violation, which
    /// rolls the transaction back as PostgreSQL does. A `COMMIT` with a check
    /// still queued fails with `FOREIGN KEY constraint failed`, and so does
    /// an autocommit write that queues one, so writes to a table carrying
    /// such a trigger run inside a transaction.
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
        Ok(self.setup.iter().chain(connection.iter().flat_map(Group::emitted)))
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
            connection: self.connection.map(|groups| {
                groups
                    .into_iter()
                    .map(|group| {
                        group.map(|statements| statements.into_iter().map(&mut f).collect())
                    })
                    .collect()
            }),
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

#[cfg(feature = "managed")]
impl<S: core::fmt::Display> Script<S> {
    /// Registers the script's helpers and pre-commit checks on `connection`,
    /// then runs the setup and the initialization of the selected fresh
    /// files, one group per logical source statement.
    ///
    /// # Errors
    ///
    /// [`OpenError::Unavailable`] when the selection is unavailable, before
    /// anything runs, or the connection's refusal.
    pub fn initialize_managed<Provider: pg2sqlite_runtime::UtcProvider + Send + 'static>(
        &self,
        connection: &mut pg2sqlite_runtime::ManagedConnection<Provider>,
        files: FreshFiles,
    ) -> Result<(), OpenError> {
        let groups = self.files.select_groups(files)?;
        self.prepare(connection)?;
        for group in groups {
            connection.execute_group(group)?;
        }
        Ok(())
    }

    /// Registers the script's helpers and pre-commit checks on `connection`
    /// to an existing database, then runs the setup and the connection part.
    ///
    /// # Errors
    ///
    /// [`OpenError::Unavailable`] when the connection part is unavailable,
    /// before anything runs, or the connection's refusal.
    pub fn connect_managed<Provider: pg2sqlite_runtime::UtcProvider + Send + 'static>(
        &self,
        connection: &mut pg2sqlite_runtime::ManagedConnection<Provider>,
    ) -> Result<(), OpenError> {
        let groups = self.connection.as_ref().map_err(|error| Error::Connection(error.clone()))?;
        self.prepare(connection)?;
        for group in groups {
            connection.execute_group(group.borrowed())?;
        }
        Ok(())
    }

    /// Registers the script's helpers and pre-commit checks on
    /// `connection`, which every connection does before running any part of
    /// the script. [`initialize_managed`](Self::initialize_managed) and
    /// [`connect_managed`](Self::connect_managed) start with it.
    ///
    /// # Errors
    ///
    /// The connection's refusal, or
    /// [`ExecutionError::InvalidTransactionState`](pg2sqlite_runtime::ExecutionError::InvalidTransactionState)
    /// inside a transaction.
    pub fn register_managed<Provider: pg2sqlite_runtime::UtcProvider + Send + 'static>(
        &self,
        connection: &mut pg2sqlite_runtime::ManagedConnection<Provider>,
    ) -> Result<(), pg2sqlite_runtime::ExecutionError> {
        let mut registration = connection.registration()?;
        for function in &self.functions {
            registration.helper(function)?;
        }
        registration.before_commit(&self.before_commit)
    }

    /// Registers the helpers and the pre-commit checks, then runs the setup.
    fn prepare<Provider: pg2sqlite_runtime::UtcProvider + Send + 'static>(
        &self,
        connection: &mut pg2sqlite_runtime::ManagedConnection<Provider>,
    ) -> Result<(), pg2sqlite_runtime::ExecutionError> {
        self.register_managed(connection)?;
        for statement in &self.setup {
            let group: pg2sqlite_runtime::ExecutionGroup<_> =
                pg2sqlite_runtime::ExecutionGroup::statements(core::slice::from_ref(statement));
            connection.execute_group(group)?;
        }
        Ok(())
    }
}
