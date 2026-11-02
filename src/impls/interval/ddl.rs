//! The DDL side of the interval comparison key: the uniqueness and the
//! checks a table of interval columns earns, and the references it cannot.

#[cfg(not(feature = "std"))]
use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::ops::ControlFlow;

use sql_traits::{structs::ParserDB, traits::TableLike};
use sqlparser::ast::{
    BinaryOperator, ColumnOption, CreateTable, Expr, ForeignKeyConstraint, Ident, InTarget,
    IndexColumn, ObjectName, ObjectNamePart, OrderByExpr, OrderByOptions, Statement,
    TableConstraint, Visit, Visitor,
};

use super::{is_interval_data_type, keys};
use crate::{
    errors::Error,
    impls::{
        object_name::{COLUMN_LOOKUP_CASE, last_ident, quoted_ident, resolve_translation_table},
        translator_impls::insert::postgres_constraint_name,
    },
    options::TranslationContext,
};

/// The table's interval columns as a check, an index or a conflict names
/// them, and the table's name, which companion indexes are named after.
struct IntervalRefs {
    columns: Vec<String>,
    table: String,
}

impl IntervalRefs {
    /// The interval columns a table declares and the table's own name.
    fn of(columns: Vec<String>, table: &CreateTable) -> Self {
        let table = last_ident(&table.name)
            .map_or_else(|| table.name.to_string(), |ident| ident.value.clone());
        Self { columns, table }
    }

    /// Whether `expr` names one of the interval columns, bare or qualified,
    /// in the spellings a constraint expression may carry.
    ///
    /// A qualified name's last part is the column. Its qualifiers need no
    /// check here, since sql-traits refuses a constraint expression whose
    /// reference names a relation other than the table being created.
    fn is_ref(&self, expr: &Expr) -> bool {
        let expr = match expr {
            Expr::Nested(inner) => inner.as_ref(),
            _ => expr,
        };
        match expr {
            Expr::Identifier(ident) => self.column_named(&ident.value, ident.quote_style.is_some()),
            Expr::CompoundIdentifier(parts) => {
                parts.last().is_some_and(|column| {
                    self.column_named(&column.value, column.quote_style.is_some())
                })
            }
            _ => false,
        }
    }

    /// Whether `expr` is a bare identifier naming one of the interval
    /// columns, the only spelling a UNIQUE or PRIMARY KEY column list may
    /// carry.
    fn is_bare_ref(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Identifier(ident)
            if self.column_named(&ident.value, ident.quote_style.is_some()))
    }

    /// Whether `value` names one of the interval columns, case rules
    /// included.
    fn column_named(&self, value: &str, quoted: bool) -> bool {
        if quoted {
            self.columns.iter().any(|column| column == value)
        } else {
            self.columns.iter().any(|column| column.eq_ignore_ascii_case(value))
        }
    }
}

/// A UNIQUE or PRIMARY KEY constraint SQLite enforces with a companion
/// unique index over the comparison key.
pub(crate) struct CompanionIndex {
    name: String,
    columns: Vec<IndexColumn>,
}

/// The companion unique indexes, in declaration order, over the emitted
/// `table_name`.
pub(crate) fn companion_statements(
    companions: &[CompanionIndex],
    table_name: &ObjectName,
) -> Vec<Statement> {
    companions
        .iter()
        .map(|companion| {
            Statement::CreateIndex(sqlparser::ast::CreateIndex {
                name: Some(ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(
                    &companion.name,
                ))])),
                table_name: table_name.clone(),
                using: None,
                columns: companion.columns.clone(),
                unique: true,
                concurrently: false,
                r#async: false,
                if_not_exists: true,
                include: Vec::new(),
                nulls_distinct: None,
                with: Vec::new(),
                predicate: None,
                index_options: Vec::new(),
                alter_options: Vec::new(),
            })
        })
        .collect()
}

/// Refuses the table's foreign keys over interval columns, checks its
/// constraint expressions name intervals only where the key settles them,
/// and answers the companion index each UNIQUE or PRIMARY KEY over an
/// interval column earns.
///
/// # Errors
///
/// Propagates the refusal a foreign key over an interval column answers, the
/// refusal a key template fails to parse, and the refusal a constraint
/// expression naming an interval outside a keyed position answers.
pub(crate) fn companions(
    create_table: &CreateTable,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<CompanionIndex>, Error> {
    let interval_columns: Vec<String> = create_table
        .columns
        .iter()
        .filter(|column| is_interval_data_type(&column.data_type))
        .map(|column| column.name.value.clone())
        .collect();
    reject_foreign_keys_over_intervals(create_table, schema, &interval_columns)?;
    if interval_columns.is_empty() {
        return Ok(Vec::new());
    }
    let refs = IntervalRefs::of(interval_columns, create_table);
    let mut companions = Vec::new();
    add_column_key_companions(create_table, &refs, &mut companions, options)?;
    add_table_key_companions(create_table, &refs, &mut companions, options)?;
    validate_constraint_expressions(create_table, &refs)?;
    Ok(companions)
}

/// Refuses a foreign key whose local or referenced side is an interval
/// column, which SQLite would settle on the stored text.
fn reject_foreign_keys_over_intervals(
    create_table: &CreateTable,
    schema: &ParserDB,
    interval_columns: &[String],
) -> Result<(), Error> {
    for constraint in &create_table.constraints {
        if let TableConstraint::ForeignKey(fk) = constraint {
            reject_foreign_key(fk, schema, interval_columns)?;
        }
    }
    for column in &create_table.columns {
        for option in &column.options {
            if let ColumnOption::ForeignKey(fk) = &option.option {
                reject_foreign_key(fk, schema, interval_columns)?;
            }
        }
    }
    Ok(())
}

/// Refuses one foreign key whose local or referenced side names an interval
/// column.
fn reject_foreign_key(
    fk: &ForeignKeyConstraint,
    schema: &ParserDB,
    interval_columns: &[String],
) -> Result<(), Error> {
    if fk.columns.iter().any(|column| column_named(interval_columns, &column.value)) {
        return Err(foreign_key_refusal());
    }
    if let Ok(Some(foreign)) = resolve_translation_table(schema, &fk.foreign_table) {
        for referenced in &fk.referred_columns {
            // sql-traits refuses a reference to a missing column before this
            // runs, so only the referenced column's type is in question.
            let referenced = foreign.column(&referenced.value, schema, COLUMN_LOOKUP_CASE)?;
            if referenced.is_some_and(|column| is_interval_data_type(&column.attribute().data_type))
            {
                return Err(foreign_key_refusal());
            }
        }
    }
    Ok(())
}

/// The refusal a foreign key over interval columns answers.
fn foreign_key_refusal() -> Error {
    Error::forward_refusal(
        "a foreign key over interval columns cannot be translated: SQLite compares the stored \
         values, so a reference PostgreSQL accepts, as '24:00:00' to '1 day', would be rejected. \
         Drop the foreign key and enforce the reference in the application."
            .to_string(),
    )
}

/// Whether `value` names one of `interval_columns`, folded as PostgreSQL
/// folds an unquoted name.
fn column_named(interval_columns: &[String], value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    interval_columns.iter().any(|column| column.to_ascii_lowercase() == lowered)
}

/// Names the companion index each UNIQUE or PRIMARY KEY option on an
/// interval column earns, keeping the option itself on the stored text.
///
/// The declared key stays because a SQLite session records changes only to a
/// table with a declared primary key and applies a changeset only onto one.
/// Equal texts are equal intervals, so it rejects nothing PostgreSQL accepts,
/// and the companion over the comparison key settles PostgreSQL's equality.
fn add_column_key_companions(
    table: &CreateTable,
    refs: &IntervalRefs,
    companions: &mut Vec<CompanionIndex>,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    for column in &table.columns {
        if !is_interval_data_type(&column.data_type) {
            continue;
        }
        for option in &column.options {
            let (declared, primary_key) = match &option.option {
                ColumnOption::Unique(unique) => {
                    (declared_name(unique.name.as_ref(), unique.index_name.as_ref()), false)
                }
                ColumnOption::PrimaryKey(primary) => {
                    (declared_name(primary.name.as_ref(), primary.index_name.as_ref()), true)
                }
                _ => continue,
            };
            companions.push(declared_companion(
                declared,
                &refs.table,
                core::slice::from_ref(&column.name.value),
                primary_key,
                [keyed_index_column(&Expr::Identifier(column.name.clone()), options)?],
            ));
        }
    }
    Ok(())
}

/// Names the companion index each table UNIQUE or PRIMARY KEY carrying an
/// interval column earns, keeping the constraint itself on the stored text
/// for the reason [`add_column_key_companions`] gives.
fn add_table_key_companions(
    table: &CreateTable,
    refs: &IntervalRefs,
    companions: &mut Vec<CompanionIndex>,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    for constraint in &table.constraints {
        let (declared, columns, primary_key) = match constraint {
            TableConstraint::Unique(unique) if has_interval_column(&unique.columns, refs) => {
                (
                    declared_name(unique.name.as_ref(), unique.index_name.as_ref()),
                    &unique.columns,
                    false,
                )
            }
            TableConstraint::PrimaryKey(primary) if has_interval_column(&primary.columns, refs) => {
                (
                    declared_name(primary.name.as_ref(), primary.index_name.as_ref()),
                    &primary.columns,
                    true,
                )
            }
            _ => continue,
        };
        companions.push(companion_for_columns(
            declared,
            &refs.table,
            columns,
            primary_key,
            refs,
            options,
        )?);
    }
    Ok(())
}

/// Whether the constraint's columns name an interval column.
fn has_interval_column(columns: &[IndexColumn], refs: &IntervalRefs) -> bool {
    columns.iter().any(|column| refs.is_bare_ref(&column.column.expr))
}

/// The companion of a UNIQUE or PRIMARY KEY over `columns`, keyed
/// where a column is an interval. A column list may name its columns only
/// bare, so a qualified entry, which PostgreSQL refuses, keeps its ordinary
/// translation.
fn companion_for_columns(
    declared: Option<String>,
    table: &str,
    columns: &[IndexColumn],
    primary_key: bool,
    refs: &IntervalRefs,
    options: &TranslationContext<'_>,
) -> Result<CompanionIndex, Error> {
    let names: Vec<String> = columns
        .iter()
        .filter_map(|column| {
            match &column.column.expr {
                Expr::Identifier(ident) => Some(ident.value.clone()),
                _ => None,
            }
        })
        .collect();
    let declared_columns = columns;
    let mut columns: Vec<IndexColumn> = Vec::with_capacity(declared_columns.len());
    for column in declared_columns {
        let operand = &column.column.expr;
        let expr = if refs.is_bare_ref(operand) {
            keys::key_expr(operand, &options.without_subqueries())?
        } else {
            operand.clone()
        };
        columns.push(IndexColumn {
            column: OrderByExpr { expr, options: column.column.options.clone(), with_fill: None },
            operator_class: None,
        });
    }
    let name = declared.unwrap_or_else(|| postgres_constraint_name(table, &names, primary_key));
    Ok(CompanionIndex { name, columns })
}

/// The companion of a UNIQUE or PRIMARY KEY option, named after the
/// constraint.
fn declared_companion(
    declared: Option<String>,
    table: &str,
    names: &[String],
    primary_key: bool,
    index_columns: impl IntoIterator<Item = IndexColumn>,
) -> CompanionIndex {
    let columns = index_columns.into_iter().collect::<Vec<_>>();
    let name = declared.unwrap_or_else(|| postgres_constraint_name(table, names, primary_key));
    CompanionIndex { name, columns }
}

/// The constraint's declared name, or `None` when it is anonymous.
fn declared_name(name: Option<&Ident>, index_name: Option<&Ident>) -> Option<String> {
    name.or(index_name).map(|ident| ident.value.clone())
}

/// The index column SQLite needs where the constraint's column is an
/// interval, keeping the constraint's sort options.
fn keyed_index_column(
    operand: &Expr,
    options: &TranslationContext<'_>,
) -> Result<IndexColumn, Error> {
    let expr = keys::key_expr(operand, &options.without_subqueries())?;
    Ok(IndexColumn {
        column: OrderByExpr { expr, options: OrderByOptions::default(), with_fill: None },
        operator_class: None,
    })
}

/// The table's CHECK constraints, the CHECK options of its columns and its
/// generated expressions, refusing the one that names an interval the
/// comparison keying cannot settle.
///
/// # Errors
///
/// Propagates the refusal an interval reference outside a keyed position
/// answers.
fn validate_constraint_expressions(table: &CreateTable, refs: &IntervalRefs) -> Result<(), Error> {
    for constraint in &table.constraints {
        if let TableConstraint::Check(check) = constraint {
            validate_constraint_expr(&check.expr, refs)?;
        }
    }
    for column in &table.columns {
        for option in &column.options {
            match &option.option {
                ColumnOption::Check(check) => validate_constraint_expr(&check.expr, refs)?,
                ColumnOption::Generated { generation_expr: Some(expr), .. } => {
                    validate_constraint_expr(expr, refs)?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Runs the keyed-position validator over one CHECK or generated expression.
fn validate_constraint_expr(expr: &Expr, refs: &IntervalRefs) -> Result<(), Error> {
    let mut validator = RefValidator { refs, depth: 0 };
    match expr.visit(&mut validator) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

/// The node a keyed position settles on, so the validator skips its sides.
fn is_keyed_position(expr: &Expr, refs: &IntervalRefs) -> bool {
    match expr {
        Expr::BinaryOp { left, op, right } => {
            is_comparison(op) && (refs.is_ref(left) || refs.is_ref(right))
        }
        Expr::In { expr: operand, target, .. } => {
            refs.is_ref(operand) && matches!(target, InTarget::List(_))
        }
        Expr::Between { expr: operand, .. } => refs.is_ref(operand),
        Expr::IsNull(inner) | Expr::IsNotNull(inner) => refs.is_ref(inner),
        _ => false,
    }
}

/// Whether `op` settles the value of an interval.
fn is_comparison(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
    )
}

/// Refuses the reference to an interval the keyed positions do not settle.
fn unprotected_refusal(expr: &Expr) -> Error {
    Error::forward_refusal(format!(
        "{expr} names an interval column outside a comparison, an IN list, a BETWEEN, or an \
         IS NULL test, and the comparison key the replica settles such a check on is not legal \
         in that position. Write the constraint as a comparison over the column."
    ))
}

struct RefValidator<'r> {
    refs: &'r IntervalRefs,
    depth: usize,
}

impl Visitor for RefValidator<'_> {
    type Break = Error;

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Error> {
        if is_keyed_position(expr, self.refs) {
            self.depth += 1;
        } else if self.depth == 0 && self.refs.is_ref(expr) {
            return ControlFlow::Break(unprotected_refusal(expr));
        }
        ControlFlow::Continue(())
    }

    fn post_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Error> {
        if is_keyed_position(expr, self.refs) {
            self.depth -= 1;
        }
        ControlFlow::Continue(())
    }
}
