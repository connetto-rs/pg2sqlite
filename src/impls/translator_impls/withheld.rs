//! The columns of a backing table the acting role cannot read.
//!
//! A server holding to PostgreSQL's privileges sends a reader of part of a
//! table only the columns it may read, so the replica holds every other
//! column as NULL. The backing table therefore declares no `NOT NULL` on such
//! a column, no `CHECK` reading one and no `NULLS NOT DISTINCT` key over one.
//! The `BEFORE INSERT` trigger this module builds holds the rows the replica
//! writes itself to what the backing table no longer declares, judging the
//! default a withheld column takes. A `BEFORE UPDATE` trigger judges an
//! updated row against each such check only when the row holds every
//! withheld column the check reads, since a delivered row holds NULL there.
//! Both yield to the RLS exemption, which a replica storing the server's rows
//! raises.
//!
//! A key column stays as declared, since the replica finds a row by its key.

use alloc::borrow::Cow;

use sql_traits::{
    structs::ParserDB,
    traits::{CheckConstraintLike, ColumnLike, TableLike},
};
use sqlparser::ast::{
    BinaryOperator, ColumnDef, ColumnOption, CreateTable, Expr, Ident, NullsDistinctOption,
    ObjectName, Statement, TableConstraint, TriggerEvent, TriggerPeriod, UniqueConstraint,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        object_name::{
            declared_column_ident, quoted_ident, reads_column, resolve_translation_table,
            same_identifier,
        },
        privileges::{RoleTable, has_backing_table},
        session_variable,
        translator_impls::{
            clock_check::read_through_new, rls::write_guard_condition_expr, statement::negate,
        },
    },
    options::TranslationContext,
    traits::translator::TranslatorWithContext,
    warnings::WarningSink,
};

/// The columns of one table the acting role cannot read, other than its key
/// columns.
pub(crate) struct Withheld(Vec<Ident>);

impl Withheld {
    /// The columns of `table` `grants` withholds.
    ///
    /// # Errors
    ///
    /// Propagates a lookup failure.
    pub(crate) fn of(
        table: &CreateTable,
        schema: &ParserDB,
        grants: &RoleTable,
    ) -> Result<Self, Error> {
        let key: Vec<Ident> =
            table.primary_key_columns(schema)?.map(declared_column_ident).collect();
        Ok(Self(
            table
                .columns(schema)?
                .filter(|column| !grants.reads(column.column_name()))
                .map(declared_column_ident)
                .filter(|column| !key.iter().any(|key| same_identifier(key, column)))
                .collect(),
        ))
    }

    /// The columns the acting role cannot read of the table `name` names,
    /// empty unless the table has a backing table.
    ///
    /// # Errors
    ///
    /// Refuses an acting role the deployment or the schema does not declare,
    /// and propagates a lookup failure.
    pub(crate) fn of_named(
        name: &ObjectName,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<Self, Error> {
        let Some(table) = resolve_translation_table(schema, name)? else {
            return Ok(Self(Vec::new()));
        };
        if session_variable::acting_role(schema, options)?.is_none()
            || !has_backing_table(table, schema, options)?
        {
            return Ok(Self(Vec::new()));
        }
        Self::of(table, schema, &RoleTable::acting(table, schema, options)?)
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `column` is withheld.
    pub(crate) fn holds(&self, column: &Ident) -> bool {
        self.0.iter().any(|withheld| same_identifier(withheld, column))
    }

    /// Whether `expr` reads a withheld column.
    pub(crate) fn read_by(&self, expr: &Expr) -> bool {
        self.0.iter().any(|column| reads_column(expr, column))
    }

    /// `create_table` without the `NOT NULL`s of the withheld columns, the
    /// checks reading one and the `NULLS NOT DISTINCT` of a key over one,
    /// which stays a plain key. A `NOT ENFORCED` check stays for the table
    /// translation to refuse.
    pub(crate) fn release<'t>(&self, create_table: &'t CreateTable) -> Cow<'t, CreateTable> {
        let columns: Vec<Cow<'_, ColumnDef>> =
            create_table.columns.iter().map(|column| self.release_column(column)).collect();
        let drops = |constraint: &TableConstraint| {
            match constraint {
                TableConstraint::NotNull(not_null) => self.holds(&not_null.column),
                TableConstraint::Check(check) => {
                    check.enforced != Some(false) && self.read_by(&check.expr)
                }
                _ => false,
            }
        };
        let relaxes = |unique: &UniqueConstraint| {
            unique.nulls_distinct == NullsDistinctOption::NotDistinct
                && unique.columns.iter().any(|key| self.read_by(&key.column.expr))
        };
        if columns.iter().all(|column| matches!(column, Cow::Borrowed(_)))
            && !create_table.constraints.iter().any(|constraint| {
                drops(constraint)
                    || matches!(constraint, TableConstraint::Unique(unique) if relaxes(unique))
            })
        {
            return Cow::Borrowed(create_table);
        }
        let mut released = create_table.clone();
        for (column, kept) in released.columns.iter_mut().zip(columns) {
            if let Cow::Owned(kept) = kept {
                *column = kept;
            }
        }
        released.constraints.retain(|constraint| !drops(constraint));
        for constraint in &mut released.constraints {
            if let TableConstraint::Unique(unique) = constraint
                && relaxes(unique)
            {
                unique.nulls_distinct = NullsDistinctOption::None;
            }
        }
        Cow::Owned(released)
    }

    /// `column` without its `NOT NULL` when it is withheld, and without the
    /// checks reading a withheld column.
    pub(crate) fn release_column<'c>(&self, column: &'c ColumnDef) -> Cow<'c, ColumnDef> {
        let withheld = self.holds(&column.name);
        let released = |option: &ColumnOption| {
            match option {
                ColumnOption::NotNull => withheld,
                ColumnOption::Check(check) => {
                    check.enforced != Some(false) && self.read_by(&check.expr)
                }
                _ => false,
            }
        };
        if !column.options.iter().any(|option| released(&option.option)) {
            return Cow::Borrowed(column);
        }
        let mut kept = column.clone();
        kept.options.retain(|option| !released(&option.option));
        Cow::Owned(kept)
    }

    /// The triggers on the backing table `backing` of `table` holding the
    /// rows the replica writes itself to what [`Self::release`] takes out of
    /// it, none when it takes nothing out.
    ///
    /// # Errors
    ///
    /// Refuses a check that does not translate, unless the options drop
    /// unsupported checks, and propagates a lookup failure.
    pub(crate) fn guards(
        &self,
        table: &CreateTable,
        backing: &str,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        if self.is_empty() {
            return Ok(Vec::new());
        }
        let relation = table.stored_table_name();
        let columns: Vec<Ident> = table.columns(schema)?.map(declared_column_ident).collect();
        let column_refs: Vec<&Ident> = columns.iter().collect();
        let mut inserted = Vec::new();
        for column in table.columns(schema)? {
            let ident = declared_column_ident(column);
            let computed = column.attribute().options.iter().any(|option| {
                matches!(option.option, ColumnOption::Generated { generation_expr: Some(_), .. })
            });
            if self.holds(&ident) && !column.is_nullable(schema)? && !computed {
                inserted.push(ast_builder::raise_statement(
                    "ABORT",
                    Some(&crate::write_guard::not_null_violation(column.column_name(), &relation)),
                    Some(Expr::IsNull(Box::new(new_column(ident)))),
                ));
            }
        }

        let scope = crate::options::table_column_scope(Some(table), schema);
        let scoped = scope.as_ref().map(|scope| options.with_scope(scope));
        let check_options = scoped.as_ref().unwrap_or(options).without_subqueries();
        let mut checks = Vec::new();
        for check in table.check_constraints(schema)? {
            let declared = check.attribute();
            if declared.enforced == Some(false) || !self.read_by(&declared.expr) {
                continue;
            }
            let condition =
                match declared.expr.translate_with_warnings(schema, &check_options, emit) {
                    Ok(condition) => condition,
                    Err(_) if options.is_remove_unsupported_check_constraints_enabled() => continue,
                    Err(error) => return Err(error),
                };
            let name = check.constraint_name(schema)?.map(Cow::into_owned).unwrap_or_default();
            let held: Vec<Ident> = self
                .0
                .iter()
                .filter(|column| reads_column(&declared.expr, column))
                .cloned()
                .collect();
            checks.push((name, condition, held));
        }
        // PostgreSQL tests the checks in the order of their names.
        checks.sort_by(|left, right| left.0.cmp(&right.0));

        let mut updated = Vec::new();
        for (name, mut condition, held) in checks {
            read_through_new(&mut condition, &column_refs);
            let message = crate::write_guard::check_violation(&relation, &name);
            let violation = negate(condition);
            inserted.push(ast_builder::raise_statement(
                "ABORT",
                Some(&message),
                Some(violation.clone()),
            ));
            // A delivered row holds NULL in the withheld columns, so only a
            // row holding each one the check reads is judged on update.
            let judged = held.into_iter().rev().fold(violation, |judged, column| {
                Expr::BinaryOp {
                    left: Box::new(Expr::IsNotNull(Box::new(new_column(column)))),
                    op: BinaryOperator::And,
                    right: Box::new(judged),
                }
            });
            updated.push(ast_builder::raise_statement("ABORT", Some(&message), Some(judged)));
        }

        let trigger = |verb: &str, event: TriggerEvent, body: Vec<Statement>| {
            (!body.is_empty()).then(|| {
                ast_builder::trigger(
                    ObjectName::from(vec![quoted_ident(&format!("{backing}_withheld_{verb}"))]),
                    ObjectName::from(vec![quoted_ident(backing)]),
                    TriggerPeriod::Before,
                    event,
                    true,
                    write_guard_condition_expr(None, options),
                    body,
                )
            })
        };
        Ok(trigger("insert", TriggerEvent::Insert, inserted)
            .into_iter()
            .chain(trigger("update", TriggerEvent::Update(Vec::new()), updated))
            .collect())
    }
}

fn new_column(column: Ident) -> Expr {
    Expr::CompoundIdentifier(vec![Ident::new("NEW"), column])
}
