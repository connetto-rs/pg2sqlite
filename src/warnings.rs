//! Warnings emitted during translation when constructs have no SQLite
//! equivalent and are dropped or downgraded.
//!
//! Each translation call owns its warning collector and passes it explicitly
//! through every translator. The plain `translate` API discards the collected
//! warnings after the same translation path completes.
//!
//! # When a warning is the right answer
//!
//! A warning is permitted ONLY when the drop provably cannot affect a query
//! result: server administration, publish and subscribe, access control,
//! planner hints. If the reason cannot be stated in one sentence, the construct
//! is a hard error instead, which is the default. A construct whose effect the
//! pipeline realises elsewhere is neither: it emits nothing and warns nothing,
//! and it belongs to the closed list documented in
//! `impls::translator_impls::statement`, which also carries the per-statement
//! classification. `tests/it/test_no_statement_is_silently_dropped.rs` enforces
//! the whole policy. A drop the caller asks for through an option is the one
//! warning that changes results, and
//! [`TranslationWarning::LeftToServer`] names each, with
//! [`TranslationWarning::ReadsLeftToServer`] naming every construct the
//! replica keeps that reads what such a drop changes.
//!
//! Do not add a fourth mechanism. There are three outcomes, plus that closed
//! list.

use sql_traits::{
    structs::{IdentifierCase, ParserDB},
    traits::DatabaseLike,
};
use sqlparser::ast::{CreateIndex, CreatePolicy, Statement};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    history::placement::primary_write_target, impls::object_name::last_ident_value_or_display,
    script::Script, traits::schema::lookup_target,
};

/// A warning emitted during translation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TranslationWarning {
    /// A construct was dropped: either PostgreSQL has it and SQLite has no
    /// equivalent, or PostgreSQL itself performs it as a no-op where it stands,
    /// as it does for a `BEGIN` inside an open transaction, so that emitting
    /// nothing is what reproduces the server.
    ///
    /// Fields are owned so a warning can name the object it concerns. A
    /// `&'static str` could only ever name the construct kind, which left the
    /// reader to find the affected table, column, or index themselves.
    LossyDrop {
        /// Short identifier for the construct (e.g. `"LISTEN"`).
        construct: String,
        /// Human-readable reason the construct was dropped.
        reason: String,
    },
    /// A table has row level security enabled but no policy grants read access,
    /// so its translated view denies every row.
    ///
    /// PostgreSQL behaves the same way, so the view is correct. This is
    /// reported because it is usually an unfinished migration, and because
    /// no runtime validation monitor is emitted for such a table: the
    /// monitor asks whether a backing row is visible through the view,
    /// which here is always no, so it would flag every write without
    /// distinguishing anything.
    RlsDeniesEveryRow {
        /// The table whose view denies every row.
        table: String,
    },
    /// A construct translated to something that keeps less than it did.
    ///
    /// Distinct from [`TranslationWarning::LossyDrop`], where the construct
    /// disappears. Here it is emitted, and only part of what it meant survives:
    /// `CHAR(3)` becomes `TEXT`, which stores what it is given rather than
    /// padding it to three characters.
    ///
    /// Fields are owned rather than `&'static str` because the location and
    /// the two type names are read from the input.
    LossyDowngrade {
        /// Short identifier for the construct, such as `"CHAR"`.
        construct: String,
        /// What the source declared, such as `"CHAR(3)"`.
        from: String,
        /// What was emitted in its place, such as `"TEXT"`.
        to: String,
        /// Where the downgrade happened.
        location: DowngradeLocation,
        /// What the emitted form no longer does.
        reason: String,
    },
    /// The acting role cannot read `column`, part of the key by which the
    /// replica's view finds the row an `UPDATE` or `DELETE` names, so the
    /// replica refuses every update and delete on `table` that PostgreSQL
    /// would let the role make. Reads and inserts are unaffected, and no
    /// row-level security monitor is emitted for the table, since the monitor
    /// finds a written row by the same key.
    UnreadableRowKey {
        /// The table whose updates and deletes are refused.
        table: String,
        /// The first key column the role cannot read.
        column: String,
    },
    /// A foreign key into a table whose rows a `SELECT` policy may hide from
    /// the acting role, which the replica checks only from the referenced
    /// side.
    ///
    /// PostgreSQL checks a foreign key without row security, so a write may
    /// reference a row the writer cannot read. The replica holds only the rows
    /// the role reads, so it leaves the check that a referencing row has its
    /// referenced row to the server. What deleting or rekeying a referenced
    /// row does to the referencing rows the replica holds stays, as triggers
    /// on the referenced table.
    HiddenReferencedRows {
        /// The foreign key's name, as PostgreSQL names it.
        name: String,
        /// The table the foreign key is declared on.
        table: String,
        /// The table the foreign key references.
        referenced: String,
        /// The `SELECT` policies on `referenced` that may hide its rows from
        /// the acting role, empty when none lets it read a row.
        policies: Vec<String>,
    },
    /// A trigger, a write statement of a trigger, or a foreign key left out
    /// of the replica because the server derives the rows it writes or, for
    /// a foreign key, the rows it references, under
    /// [`Pg2SqliteOptions::with_server_derived_writes_omitted`](crate::options::Pg2SqliteOptions::with_server_derived_writes_omitted),
    /// or any foreign key under
    /// [`Pg2SqliteOptions::with_foreign_keys_omitted`](crate::options::Pg2SqliteOptions::with_foreign_keys_omitted).
    ///
    /// The replica writes none of those rows, so a write the application makes
    /// on the replica that relies on one of them sees it only once the
    /// server's changes arrive. A foreign key left out is checked, and its
    /// actions run, only by the server.
    LeftToServer {
        /// `"TRIGGER"`, `"FOREIGN KEY"`, or the statement a kept trigger runs
        /// without, as `"INSERT in TRIGGER"`.
        construct: String,
        /// The trigger's name, or the foreign key's. A foreign key declared
        /// without a name is named by its columns and the table it
        /// references, as `(org, member) REFERENCES memberships`.
        name: String,
        /// The table the trigger fires on or the foreign key is declared on.
        table: String,
        /// The write the server derives, or what a trigger left out whole
        /// reads of it.
        reason: String,
    },
    /// A construct the replica keeps reads what a write it leaves to the
    /// server changes, under
    /// [`Pg2SqliteOptions::with_server_derived_writes_omitted`](crate::options::Pg2SqliteOptions::with_server_derived_writes_omitted).
    ///
    /// Until the server's rows arrive, the replica holds the rows the write
    /// changes as they were before it, so the construct may admit or refuse a
    /// later write, or show a row, where the server decides otherwise. A
    /// policy of any command, a `CHECK`, a trigger, a unique key or index and
    /// a foreign key each count, reading the table itself or through the
    /// functions it calls.
    ReadsLeftToServer {
        /// The kind of the construct that reads, as SQL spells it.
        construct: String,
        /// Its name, a foreign key named as in
        /// [`TranslationWarning::LeftToServer`].
        name: String,
        /// The table it is declared on, as written.
        table: String,
        /// The functions it reads through, the one it calls first, empty
        /// when it reads the table itself.
        through: Vec<String>,
        /// The table it reads, which the write changes, folded as
        /// PostgreSQL folds an unquoted name.
        read: String,
        /// The columns of `read` it reads that the write changes, folded,
        /// empty when it reads whole rows or which rows the table holds.
        columns: Vec<String>,
        /// The kind of what leaves the write to the server, as
        /// [`TranslationWarning::LeftToServer`] names it.
        writer: String,
        /// Its name.
        writer_name: String,
        /// The table it is declared on, as written.
        writer_table: String,
    },
}

/// Where a [`TranslationWarning::LossyDowngrade`] happened, each name being
/// the last identifier as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DowngradeLocation {
    /// A column whose declared type was downgraded.
    Column {
        /// The table declaring the column.
        table: String,
        /// The column.
        column: String,
    },
    /// A declared object that lost one of its clauses.
    Object(DeclaredObject),
    /// An expression translated into one that reads its operand differently.
    Expression {
        /// The declared object whose statement holds the expression, a
        /// function body counting as its caller's, the table or view a write
        /// changes, `None` for a query.
        object: Option<DeclaredObject>,
        /// The expression as written.
        text: String,
    },
    /// Every connection that writes to the replica, for a downgrade that
    /// connection state decides rather than any declared object.
    Script,
}

/// A declared object, as a [`DowngradeLocation`] names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredObject {
    /// A table.
    Table {
        /// The table.
        name: String,
    },
    /// A view.
    View {
        /// The view.
        name: String,
    },
    /// An index.
    Index {
        /// The indexed table.
        table: String,
        /// The index.
        name: String,
    },
    /// A row level security policy.
    Policy {
        /// The table the policy is declared on.
        table: String,
        /// The policy.
        name: String,
    },
    /// A trigger, which also holds the body of the function it executes.
    Trigger {
        /// The table or view the trigger fires on.
        table: String,
        /// The trigger.
        name: String,
    },
}

impl DeclaredObject {
    /// The object `statement` declares or alters, or the table or view it
    /// writes as `schema` resolves the name, `schema` being the one the
    /// statement was written against. `None` for a statement that names none.
    pub(crate) fn of_statement(statement: &Statement, schema: &ParserDB) -> Option<Self> {
        match statement {
            Statement::CreateTable(create_table) => {
                Some(Self::Table { name: last_ident_value_or_display(&create_table.name) })
            }
            Statement::AlterTable(alter_table) => {
                Some(Self::Table { name: last_ident_value_or_display(&alter_table.name) })
            }
            Statement::CreateView(create_view) => {
                Some(Self::View { name: last_ident_value_or_display(&create_view.name) })
            }
            Statement::CreateIndex(CreateIndex { name: Some(name), table_name, .. }) => {
                Some(Self::Index {
                    table: last_ident_value_or_display(table_name),
                    name: last_ident_value_or_display(name),
                })
            }
            Statement::CreateTrigger(create_trigger) => {
                Some(Self::Trigger {
                    table: last_ident_value_or_display(&create_trigger.table_name),
                    name: last_ident_value_or_display(&create_trigger.name),
                })
            }
            _ => Self::written(statement, schema),
        }
    }

    /// The table or view `statement` writes, a name `schema` holds no view
    /// under counting as a table.
    fn written(statement: &Statement, schema: &ParserDB) -> Option<Self> {
        let target = primary_write_target(statement)?;
        let name = last_ident_value_or_display(target);
        let view = lookup_target(target).is_some_and(|target| {
            schema
                .resolve_target_view(target, IdentifierCase::AsWritten)
                .is_ok_and(|view| view.is_some())
        });
        Some(if view { Self::View { name } } else { Self::Table { name } })
    }

    /// `policy`, by its table and name.
    pub(crate) fn of_policy(policy: &CreatePolicy) -> Self {
        Self::Policy {
            table: last_ident_value_or_display(&policy.table_name),
            name: policy.name.value.clone(),
        }
    }
}

impl core::fmt::Display for DowngradeLocation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Column { table, column } => write!(f, "{table}.{column}"),
            Self::Object(object) => write!(f, "{object}"),
            Self::Expression { object: Some(object), text } => write!(f, "{text} in {object}"),
            Self::Expression { object: None, text } => write!(f, "{text}"),
            Self::Script => write!(f, "every connection that writes"),
        }
    }
}

impl core::fmt::Display for DeclaredObject {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Table { name } => write!(f, "table {name}"),
            Self::View { name } => write!(f, "view {name}"),
            Self::Index { table, name } => write!(f, "index {name} on {table}"),
            Self::Policy { table, name } => write!(f, "policy {name} on {table}"),
            Self::Trigger { table, name } => write!(f, "trigger {name} on {table}"),
        }
    }
}

/// Where a translation reports the steps that lost something.
///
/// Named by [`TranslatorWithContext::translate_with_warnings`](crate::traits::TranslatorWithContext::translate_with_warnings),
/// so it is public for the same reason that trait is.
pub type WarningSink<'a> = &'a mut dyn FnMut(TranslationWarning);

impl core::fmt::Display for TranslationWarning {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LossyDrop { construct, reason } => {
                write!(f, "dropped {construct}: {reason}")
            }
            Self::RlsDeniesEveryRow { table } => {
                write!(
                    f,
                    "row level security on {table} has no read policy, so its view denies every row"
                )
            }
            Self::LossyDowngrade { construct, from, to, location, reason } => {
                write!(f, "downgraded {construct} at {location} from {from} to {to}: {reason}")
            }
            Self::UnreadableRowKey { table, column } => {
                write!(
                    f,
                    "the acting role cannot read key column {column} of {table}, so the replica \
                     refuses its updates and deletes"
                )
            }
            Self::HiddenReferencedRows { name, table, referenced, policies } => {
                write!(
                    f,
                    "the replica leaves the check of foreign key {name} on {table} to the server, \
                     since "
                )?;
                if policies.is_empty() {
                    write!(f, "no policy lets the acting role read {referenced}")
                } else {
                    write!(
                        f,
                        "{} may hide rows of {referenced} from the acting role",
                        policies.join(", ")
                    )
                }
            }
            Self::LeftToServer { construct, name, table, reason } => {
                write!(f, "left {construct} {name} on {table} to the server, since {reason}")
            }
            Self::ReadsLeftToServer {
                construct,
                name,
                table,
                through,
                read,
                columns,
                writer,
                writer_name,
                writer_table,
            } => {
                write!(f, "kept {construct} {name} on {table} reads ")?;
                if columns.is_empty() {
                    write!(f, "rows of {read}")?;
                } else {
                    write!(f, "{} of {read}", columns.join(", "))?;
                }
                for (depth, function) in through.iter().enumerate() {
                    let joint = if depth == 0 { " through" } else { " calling" };
                    write!(f, "{joint} {function}()")?;
                }
                write!(
                    f,
                    ", which {writer} {writer_name} on {writer_table} changes on the server alone, \
                     so the replica decides on them as they were before"
                )
            }
        }
    }
}

/// Combined output of [`Pg2Sqlite::translate_with_report`], the translated
/// script plus any warnings collected during translation.
///
/// [`Pg2Sqlite::translate_with_report`]: crate::pg2sqlite::Pg2Sqlite::translate_with_report
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationReport {
    /// The translated script, the one [`Pg2Sqlite::translate`] returns.
    ///
    /// [`Pg2Sqlite::translate`]: crate::pg2sqlite::Pg2Sqlite::translate
    pub script: Script,
    /// Warnings collected during translation. Empty when nothing was
    /// dropped or downgraded.
    pub warnings: Vec<TranslationWarning>,
}
