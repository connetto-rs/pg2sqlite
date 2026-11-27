//! The logical statements a translation emits, each run as one unit by a
//! managed connection.

use alloc::string::String;

/// The locking mode a `BEGIN` opens its transaction in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BeginMode {
    /// `BEGIN DEFERRED`, which takes no lock until the first read or write.
    #[default]
    Deferred,
    /// `BEGIN IMMEDIATE`, which takes the write lock at once.
    Immediate,
    /// `BEGIN EXCLUSIVE`, which takes the write lock and refuses readers in
    /// other journal modes than WAL.
    Exclusive,
}

impl BeginMode {
    /// The statement opening a transaction in this mode.
    #[cfg(feature = "managed")]
    pub(crate) const fn statement(self) -> &'static str {
        match self {
            Self::Deferred => "BEGIN DEFERRED",
            Self::Immediate => "BEGIN IMMEDIATE",
            Self::Exclusive => "BEGIN EXCLUSIVE",
        }
    }
}

/// A transaction control, which a managed connection runs through its
/// transaction manager instead of handing its text to SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionControl<Name = String> {
    /// `BEGIN` in the given mode.
    Begin(BeginMode),
    /// `COMMIT`, opening a new transaction afterwards when `chain` is set,
    /// as PostgreSQL's `COMMIT AND CHAIN` does.
    Commit {
        /// Whether a new transaction opens after the commit succeeds.
        chain: bool,
    },
    /// `ROLLBACK`, opening a new transaction afterwards when `chain` is set,
    /// as PostgreSQL's `ROLLBACK AND CHAIN` does.
    Rollback {
        /// Whether a new transaction opens after the rollback succeeds.
        chain: bool,
    },
    /// `SAVEPOINT name`.
    Savepoint(Name),
    /// `RELEASE name`.
    Release(Name),
    /// `ROLLBACK TO name`.
    RollbackTo(Name),
}

impl<Name: AsRef<str>> TransactionControl<Name> {
    /// The control naming its savepoint by reference.
    #[must_use]
    pub fn as_deref(&self) -> TransactionControl<&str> {
        match self {
            Self::Begin(mode) => TransactionControl::Begin(*mode),
            Self::Commit { chain } => TransactionControl::Commit { chain: *chain },
            Self::Rollback { chain } => TransactionControl::Rollback { chain: *chain },
            Self::Savepoint(name) => TransactionControl::Savepoint(name.as_ref()),
            Self::Release(name) => TransactionControl::Release(name.as_ref()),
            Self::RollbackTo(name) => TransactionControl::RollbackTo(name.as_ref()),
        }
    }
}

/// One logical source statement: the SQLite statements emitted for it, which
/// a managed connection runs under one statement instant, or a transaction
/// control, which it runs as typed and whose statements only spell it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionGroup<Statements, Name = String> {
    control: Option<TransactionControl<Name>>,
    statements: Statements,
}

impl<Statements, Name> ExecutionGroup<Statements, Name> {
    /// The group of `statements`, none of them a transaction control.
    pub const fn statements(statements: Statements) -> Self {
        Self { control: None, statements }
    }

    /// The group running `control`, spelled as `statements`.
    pub const fn control(control: TransactionControl<Name>, statements: Statements) -> Self {
        Self { control: Some(control), statements }
    }

    /// The transaction control the group runs, if it is one.
    pub const fn transaction_control(&self) -> Option<&TransactionControl<Name>> {
        self.control.as_ref()
    }

    /// The SQLite statements emitted for the group.
    pub const fn emitted(&self) -> &Statements {
        &self.statements
    }

    /// The SQLite statements emitted for the group, to rewrite in place.
    pub const fn emitted_mut(&mut self) -> &mut Statements {
        &mut self.statements
    }

    /// The SQLite statements emitted for the group, consuming it.
    pub fn into_emitted(self) -> Statements {
        self.statements
    }

    /// Applies `f` to the emitted statements, keeping the control.
    pub fn map<Mapped>(self, f: impl FnOnce(Statements) -> Mapped) -> ExecutionGroup<Mapped, Name> {
        ExecutionGroup { control: self.control, statements: f(self.statements) }
    }

    /// Splits the group into its control and its emitted statements.
    #[cfg(feature = "managed")]
    pub(crate) fn into_parts(self) -> (Option<TransactionControl<Name>>, Statements) {
        (self.control, self.statements)
    }
}

impl<Statement, Name: AsRef<str>> ExecutionGroup<alloc::vec::Vec<Statement>, Name> {
    /// The group borrowing its statements and savepoint name.
    pub fn borrowed(&self) -> ExecutionGroup<&[Statement], &str> {
        ExecutionGroup {
            control: self.control.as_ref().map(TransactionControl::as_deref),
            statements: &self.statements,
        }
    }
}
