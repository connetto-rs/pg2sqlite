//! What the acting role may do with a table, column by column, and the shape
//! its translation takes as a result.
//!
//! A table the role reads and writes in full stays a plain table. A table the
//! role cannot read in full, or can insert into only in part, has a backing
//! table behind a view holding its logical name, as a table with row-level
//! security does. The view projects the readable columns, and its `INSTEAD OF`
//! triggers see which columns an insert supplied, which a `BEFORE` trigger on
//! a plain table cannot tell from a default.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{CreateTable, ObjectName};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{object_name::resolve_translation_table, session_variable},
    options::Pg2SqliteOptions,
};

/// The privileges the acting role holds on one column.
#[derive(Debug, Clone)]
struct ColumnGrant {
    name: String,
    select: bool,
    insert: bool,
    update: bool,
}

/// What the acting role may do with one table, as PostgreSQL's privilege
/// checks answer for each command and column.
#[derive(Debug, Clone)]
pub(crate) struct RoleTable {
    /// Every column in declaration order.
    columns: Vec<ColumnGrant>,
    delete: bool,
}

impl RoleTable {
    /// What `role` holds on `table`, or every privilege when no acting role is
    /// named, which is how a translation without a deployment runs.
    ///
    /// # Errors
    ///
    /// Propagates a lookup failure.
    pub(crate) fn of<'db>(
        table: &'db CreateTable,
        role: Option<&'db <ParserDB as DatabaseLike>::Role>,
        schema: &'db ParserDB,
    ) -> Result<Self, Error> {
        let Some(role) = role else {
            let columns = table
                .columns(schema)?
                .map(|column| {
                    ColumnGrant {
                        name: column.column_name().to_string(),
                        select: true,
                        insert: true,
                        update: true,
                    }
                })
                .collect();
            return Ok(Self { columns, delete: true });
        };
        let held = table.privileges(role, schema)?;
        let columns = table
            .columns(schema)?
            .map(|column| {
                ColumnGrant {
                    name: column.column_name().to_string(),
                    select: held.select.on_column(column),
                    insert: held.insert.on_column(column),
                    update: held.update.on_column(column),
                }
            })
            .collect();
        Ok(Self { columns, delete: held.delete })
    }

    /// What the acting role `options` names holds on `table`.
    ///
    /// # Errors
    ///
    /// Refuses an acting role the deployment or the schema does not declare,
    /// and propagates a lookup failure.
    pub(crate) fn acting(
        table: &CreateTable,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Self, Error> {
        Self::of(table, session_variable::acting_role(schema, options)?, schema)
    }

    fn column(&self, name: &str) -> Option<&ColumnGrant> {
        self.columns.iter().find(|column| column.name.eq_ignore_ascii_case(name))
    }

    /// Whether the role may `SELECT` some column, which is what brings the
    /// table into the replica.
    pub(crate) fn reads_any(&self) -> bool {
        self.columns.iter().any(|column| column.select)
    }

    /// Whether the role may `SELECT` `column`.
    pub(crate) fn reads(&self, column: &str) -> bool {
        self.column(column).is_some_and(|column| column.select)
    }

    /// Whether the role may `INSERT` into `column`.
    pub(crate) fn inserts(&self, column: &str) -> bool {
        self.column(column).is_some_and(|column| column.insert)
    }

    /// Whether the role may `UPDATE` `column`.
    pub(crate) fn updates(&self, column: &str) -> bool {
        self.column(column).is_some_and(|column| column.update)
    }

    /// Whether the role may `INSERT` into some column, which is what gives the
    /// table an insert path.
    pub(crate) fn inserts_any(&self) -> bool {
        self.columns.iter().any(|column| column.insert)
    }

    /// Whether the role may `UPDATE` some column.
    pub(crate) fn updates_any(&self) -> bool {
        self.columns.iter().any(|column| column.update)
    }

    /// Whether the role may `DELETE`.
    pub(crate) const fn deletes(&self) -> bool {
        self.delete
    }

    /// Whether the role holds no write privilege at all.
    pub(crate) fn is_read_only(&self) -> bool {
        !self.inserts_any() && !self.updates_any() && !self.delete
    }

    /// Whether the role holds every write privilege on every column.
    pub(crate) fn writes_everything(&self) -> bool {
        self.delete && self.columns.iter().all(|column| column.insert && column.update)
    }

    /// Whether a plain table cannot carry the role's grants, because a column
    /// has to be hidden or an insert has to be told apart from a default.
    pub(crate) fn needs_view(&self) -> bool {
        self.reads_any()
            && (!self.columns.iter().all(|column| column.select)
                || (self.inserts_any() && !self.columns.iter().all(|column| column.insert)))
    }

    /// The columns the role may `SELECT`, in declaration order.
    pub(crate) fn readable(&self) -> impl Iterator<Item = &str> {
        self.columns.iter().filter(|column| column.select).map(|column| column.name.as_str())
    }

    /// The columns the role may `SELECT` but not `UPDATE`, which a view's
    /// `UPDATE OF` gate names.
    pub(crate) fn readable_not_updatable(&self) -> impl Iterator<Item = &str> {
        self.columns
            .iter()
            .filter(|column| column.select && !column.update)
            .map(|column| column.name.as_str())
    }

    /// The columns the role may not `UPDATE`, which a plain table's
    /// `UPDATE OF` gate names.
    pub(crate) fn not_updatable(&self) -> impl Iterator<Item = &str> {
        self.columns.iter().filter(|column| !column.update).map(|column| column.name.as_str())
    }
}

/// Whether `table` has a backing table under the configured suffix, with a
/// view holding its logical name. Row-level security puts one there for every
/// role, and the acting role's grants do when a plain table cannot carry them.
///
/// # Errors
///
/// Refuses an acting role the deployment or the schema does not declare, and
/// propagates a lookup failure.
pub(crate) fn has_backing_table(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<bool, Error> {
    if table.has_row_level_security(schema)? {
        return Ok(true);
    }
    match session_variable::acting_role(schema, options)? {
        Some(role) => Ok(RoleTable::of(table, Some(role), schema)?.needs_view()),
        None => Ok(false),
    }
}

/// [`has_backing_table`] for the table `name` resolves to, `false` for a name
/// naming no table.
///
/// # Errors
///
/// As [`has_backing_table`].
pub(crate) fn names_table_with_backing(
    schema: &ParserDB,
    name: &ObjectName,
    options: &Pg2SqliteOptions,
) -> Result<bool, Error> {
    match resolve_translation_table(schema, name)? {
        Some(table) => has_backing_table(table, schema, options),
        None => Ok(false),
    }
}
