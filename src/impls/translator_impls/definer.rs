//! `SECURITY DEFINER` trigger functions, whose bodies read and write as the
//! function's owner.
//!
//! Where row-level security exempts that owner from a table, the body reads
//! and writes the table's backing storage, and the guards on that storage let
//! its writes through. SQLite cannot tell which trigger is running, so the
//! body pushes its owner onto [`STACK`] for its duration, and a backing-table
//! guard stands aside while the innermost owner on the stack is one the table
//! exempts. Nested definer bodies stack, as `current_user` does in PostgreSQL.
//!
//! An aborted statement takes the push back with it. `RAISE(IGNORE)` keeps
//! what the trigger already wrote, so the body pops its owner before a
//! statement that can raise one and pushes it again before the next. A caller
//! statement written `OR FAIL` keeps the rows before a failing one too, so a
//! failure inside a definer body would leave its owner on the stack.
//! PostgreSQL has no `OR FAIL`, and no translated statement carries one.

use alloc::collections::BTreeSet;
use core::ops::ControlFlow;

use sql_traits::{
    structs::{ParserDB, RowSecurity, RowStatement},
    traits::{ColumnLike, DatabaseLike, FunctionLike, RoleLike, TableLike},
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, BinaryOperator, CreateFunction, CreateTable, DataType, Expr,
    FunctionReturnType, Ident, InTarget, Insert, ObjectName, ObjectNamePart, Statement,
    visit_expressions, visit_relations, visit_relations_mut,
};

use super::trigger_slots::folded;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        function_helpers::{simple_function_expr, string_literal},
        object_name::{append_suffix, last_ident_value_or_display, resolve_translation_table},
        privileges::{RoleTable, has_backing_table},
        query_builder::{from_relation, plain_table_factor, single_expr_query},
        translator_impls::{
            insert::row_width,
            off_replica::{Write, row_write},
        },
    },
    write_guard::permission_denied,
};

/// The frames of the definer bodies running on the connection, each holding
/// its body's owner, innermost last.
pub(crate) const STACK: &str = "pg2sqlite_definer_frames";

type Role = <ParserDB as DatabaseLike>::Role;
type Table = <ParserDB as DatabaseLike>::Table;

/// `CREATE TABLE IF NOT EXISTS` for [`STACK`], which a translation needs
/// whenever one of its statements reads or writes it.
pub(crate) fn stack_table() -> Statement {
    ast_builder::create_table(
        stack_name(),
        vec![
            super::rls::generated_key_column(),
            super::rls::generated_text_column(
                "owner",
                super::rls::non_empty_at_most("owner", super::rls::NAME_LENGTH),
            ),
        ],
    )
}

/// Whether `statement` reads or writes [`STACK`].
pub(crate) fn uses_stack(statement: &Statement) -> bool {
    visit_relations(statement, |name: &ObjectName| {
        if is_stack(name) { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    })
    .is_break()
}

/// The role a trigger function's body runs as, `None` for a `SECURITY
/// INVOKER` function.
///
/// # Errors
///
/// Refuses a definer function whose owner the schema never declares.
pub(crate) fn definer_owner<'s>(
    function: &'s CreateFunction,
    schema: &'s ParserDB,
) -> Result<Option<&'s Role>, Error> {
    if function.security_mode() != sqlparser::ast::FunctionSecurity::Definer {
        return Ok(None);
    }
    match function.owner(schema)?.and_then(|name| schema.role(name)) {
        Some(owner) => Ok(Some(owner)),
        None => {
            Err(Error::forward_refusal(format!(
                "trigger function '{}' is SECURITY DEFINER, so its body reads and writes as its \
                 owner, and the schema never declares that owner. Name it with ALTER FUNCTION \
                 ... OWNER TO a role the schema creates, or call with_schema_owner_role with the \
                 role that runs the schema.",
                function.name
            )))
        }
    }
}

/// Whether `owner` reads and writes `table` past its row-level security.
pub(crate) fn exempts(table: &CreateTable, owner: &Role, schema: &ParserDB) -> bool {
    matches!(table.row_security(RowStatement::Select, owner, schema), Ok(RowSecurity::Exempt(_)))
}

/// The owners of the schema's definer trigger functions and PL/pgSQL helpers,
/// whose bodies a trigger runs, each named once.
fn running_owners(schema: &ParserDB) -> Vec<&Role> {
    let mut owners: Vec<&Role> = Vec::new();
    let runs_in_triggers = |function: &CreateFunction| {
        returns_trigger(function) || function.stored_language().as_deref() == Some("plpgsql")
    };
    for function in schema.functions().filter(|function| runs_in_triggers(function)) {
        if let Ok(Some(owner)) = definer_owner(function, schema)
            && !owners.iter().any(|listed| listed.name() == owner.name())
        {
            owners.push(owner);
        }
    }
    owners
}

/// The owners of the schema's definer trigger functions and PL/pgSQL helpers,
/// whose bodies a trigger runs, that `table`'s row-level security exempts,
/// the ones whose bodies its guards let through.
///
/// A definer function whose owner is undeclared exempts nothing, and a trigger
/// running it is refused.
pub(crate) fn exempt_owners<'s>(table: &'s CreateTable, schema: &'s ParserDB) -> Vec<&'s str> {
    let mut owners: Vec<&str> = running_owners(schema)
        .into_iter()
        .filter(|owner| exempts(table, owner, schema))
        .map(RoleLike::name)
        .collect();
    owners.sort_unstable();
    owners
}

/// The owners of the schema's definer trigger functions and PL/pgSQL helpers,
/// whose bodies a trigger runs, the ones the guards holding the acting role to
/// its privileges let through. [`run_as_owner`] holds each body's writes to its
/// owner's privileges instead.
pub(crate) fn definer_owners(schema: &ParserDB) -> Vec<&str> {
    let mut owners: Vec<&str> = running_owners(schema).into_iter().map(RoleLike::name).collect();
    owners.sort_unstable();
    owners
}

/// Whether `function` is a trigger function.
pub(crate) fn returns_trigger(function: &CreateFunction) -> bool {
    matches!(function.return_type, Some(FunctionReturnType::DataType(DataType::Trigger)))
}

/// The filter that selects the frame of the innermost running definer body,
/// `id = (SELECT max(id) FROM <stack>)`.
fn innermost_frame() -> Expr {
    Expr::BinaryOp {
        left: Box::new(Expr::Identifier(Ident::new("id"))),
        op: BinaryOperator::Eq,
        right: Box::new(Expr::Subquery(Box::new(single_expr_query(
            simple_function_expr("max", vec![Expr::Identifier(Ident::new("id"))], None),
            from_relation(plain_table_factor(stack_name())),
            None,
        )))),
    }
}

/// The guard clause that holds unless the innermost running definer body
/// belongs to one of `owners`, `None` when there are none.
pub(crate) fn outside_owners(owners: &[&str]) -> Option<Expr> {
    if owners.is_empty() {
        return None;
    }
    let innermost = innermost_frame();
    let listed = Expr::In {
        expr: Box::new(Expr::Identifier(Ident::new("owner"))),
        target: InTarget::List(owners.iter().map(|owner| string_literal(owner)).collect()),
        negated: false,
        global: false,
    };
    Some(Expr::Exists {
        subquery: Box::new(single_expr_query(
            Expr::Value(sqlparser::ast::Value::Number("1".to_string(), false).into()),
            from_relation(plain_table_factor(stack_name())),
            Some(Expr::BinaryOp {
                left: Box::new(innermost),
                op: BinaryOperator::And,
                right: Box::new(listed),
            }),
        )),
        negated: true,
    })
}

/// `condition`, narrowed to the writes made outside the bodies of `owners`.
pub(crate) fn unless_running(condition: Option<Expr>, owners: &[&str]) -> Option<Expr> {
    match (condition, outside_owners(owners)) {
        (Some(condition), Some(outside)) => {
            Some(Expr::BinaryOp {
                left: Box::new(Expr::Nested(Box::new(condition))),
                op: BinaryOperator::And,
                right: Box::new(outside),
            })
        }
        (condition, outside) => condition.or(outside),
    }
}

/// Rewrites a translated definer body to run as `owner`.
///
/// Every table with row-level security the body names is read and written
/// through its backing table. A write needing a privilege `owner` lacks on a
/// column it names raises PostgreSQL's refusal in its place, so the body runs
/// with `owner` on [`STACK`] when some table exempts it or a write reaches a
/// table whose guards hold the acting role to its own privileges.
///
/// # Errors
///
/// Refuses a body naming a table whose policies bind `owner`, since SQLite
/// has no way to apply another role's policies.
pub(crate) fn run_as_owner(
    function: &CreateFunction,
    owner: &Role,
    mut statements: Vec<Statement>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let mut guarded = exempt_somewhere(owner, schema)?;
    // Privileges are modelled only for a translation naming an acting role.
    let privileged = crate::impls::session_variable::acting_role(schema, options)?.is_some();
    for statement in &mut statements {
        let mut denied = None;
        if privileged
            && let Some((name, write)) = row_write(statement)
            && let Some(table) = resolve_translation_table(schema, name)?
        {
            if owner_may(statement, table, &write, owner, schema)? {
                guarded |= !RoleTable::acting(table, schema, options)?.writes_everything();
            } else {
                denied = Some(permission_denied(table.table_name()));
            }
        }
        if let Some(message) = denied {
            *statement = ast_builder::raise_statement("ABORT", Some(&message), None);
            continue;
        }
        let outcome = visit_relations_mut(statement, |name: &mut ObjectName| {
            match backing_name(function, owner, name, schema, options) {
                Ok(Some(backing)) => {
                    *name = backing;
                    ControlFlow::Continue(())
                }
                Ok(None) => ControlFlow::Continue(()),
                Err(error) => ControlFlow::Break(error),
            }
        });
        if let ControlFlow::Break(error) = outcome {
            return Err(error);
        }
    }

    if !guarded {
        return Ok(statements);
    }
    let mut wrapped = Vec::with_capacity(statements.len() + 2);
    let mut pushed = false;
    for statement in statements {
        let ignores = ignores_the_write(&statement);
        if ignores && pushed {
            wrapped.push(pop());
        } else if !ignores && !pushed {
            wrapped.push(push(owner));
        }
        pushed = !ignores;
        wrapped.push(statement);
    }
    if pushed {
        wrapped.push(pop());
    }
    Ok(wrapped)
}

/// The tables whose row-level security exempts `owner`, each failed lookup
/// of a table's row-level security as an error in its place.
fn exempting<'s>(
    owner: &'s Role,
    schema: &'s ParserDB,
) -> impl Iterator<Item = Result<&'s CreateTable, Error>> + 's {
    schema.tables().filter_map(move |table| {
        match table.has_row_level_security(schema) {
            Ok(secured) => (secured && exempts(table, owner, schema)).then_some(Ok(table)),
            Err(error) => Some(Err(error.into())),
        }
    })
}

/// Whether row-level security on some table exempts `owner`, so a body running
/// as `owner` stands on [`STACK`] for the guards that let it through.
///
/// # Errors
///
/// Returns the error looking up a table's row-level security returns.
pub(crate) fn exempt_somewhere(owner: &Role, schema: &ParserDB) -> Result<bool, Error> {
    Ok(exempting(owner, schema).next().transpose()?.is_some())
}

/// The tables, folded, whose row-level security exempts `owner`.
///
/// # Errors
///
/// As [`exempt_somewhere`].
pub(crate) fn exempting_tables(owner: &Role, schema: &ParserDB) -> Result<BTreeSet<String>, Error> {
    exempting(owner, schema).map(|table| Ok(folded(&table?.name))).collect()
}

/// Whether `owner` holds what PostgreSQL checks before `statement` makes
/// `write` on `table`, the command on each column it names.
///
/// # Errors
///
/// Propagates a lookup failure.
fn owner_may(
    statement: &Statement,
    table: &Table,
    write: &Write<'_>,
    owner: &Role,
    schema: &ParserDB,
) -> Result<bool, Error> {
    let held = RoleTable::of(table, Some(owner), schema)?;
    let updates = |assignments: &[Assignment]| {
        assignments
            .iter()
            .flat_map(|assignment| {
                match &assignment.target {
                    AssignmentTarget::ColumnName(name) => core::slice::from_ref(name),
                    AssignmentTarget::Tuple(names) => names.as_slice(),
                }
            })
            .all(|name| held.updates(&last_ident_value_or_display(name)))
    };
    Ok(match write {
        Write::Insert { upsert } => {
            let Statement::Insert(insert) = statement else { return Ok(true) };
            inserts(insert, table, &held, schema)? && upsert.is_none_or(updates)
        }
        Write::Update(assignments) => updates(assignments),
        Write::Delete | Write::Truncate => held.deletes(),
    })
}

/// Whether `held` covers the columns `insert` fills, the ones it lists or as
/// many leading columns as its rows hold, and some column for a row of
/// defaults.
///
/// # Errors
///
/// Propagates a lookup failure.
fn inserts(
    insert: &Insert,
    table: &Table,
    held: &RoleTable,
    schema: &ParserDB,
) -> Result<bool, Error> {
    if !insert.columns.is_empty() {
        return Ok(insert
            .columns
            .iter()
            .all(|name| held.inserts(&last_ident_value_or_display(name))));
    }
    let width = insert.source.as_deref().map_or(Some(0), |source| row_width(&source.body));
    let columns: Vec<String> =
        table.columns(schema)?.map(|column| column.column_name().to_string()).collect();
    let filled = &columns[..width.map_or(columns.len(), |width| width.min(columns.len()))];
    Ok(if filled.is_empty() {
        held.inserts_any()
    } else {
        filled.iter().all(|column| held.inserts(column))
    })
}

/// The backing table `owner` reads and writes in place of `name`, `None` when
/// `name` is not a table with one. The owner reads past the acting role's
/// grants, and past row-level security where the policies exempt it.
pub(crate) fn backing_name(
    function: &CreateFunction,
    owner: &Role,
    name: &ObjectName,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<ObjectName>, Error> {
    let Some(table) = resolve_translation_table(schema, name)? else {
        return Ok(None);
    };
    if !has_backing_table(table, schema, options)? {
        return Ok(None);
    }
    if table.has_row_level_security(schema)? && !exempts(table, owner, schema) {
        return Err(Error::forward_refusal(format!(
            "function '{}' is SECURITY DEFINER and reads or writes table '{}' as its \
             owner '{}', whom the table's row-level security policies bind. The translation \
             cannot apply another role's policies, only the caller's or none. Make the owner \
             the table's owner or a BYPASSRLS role, or declare the function SECURITY INVOKER.",
            function.name,
            table.table_name(),
            owner.name()
        )));
    }
    Ok(Some(append_suffix(name, options.get_rls_table_suffix())))
}

/// Whether `statement` can raise `RAISE(IGNORE)`, which ends the trigger and
/// keeps what it wrote.
fn ignores_the_write(statement: &Statement) -> bool {
    visit_expressions(statement, |expr| {
        if let Expr::Function(function) = expr
            && function
                .name
                .0
                .last()
                .and_then(ObjectNamePart::as_ident)
                .is_some_and(|ident| ident.value.eq_ignore_ascii_case("RAISE"))
            && let sqlparser::ast::FunctionArguments::List(list) = &function.args
            && matches!(
                list.args.first(),
                Some(sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(
                    Expr::Identifier(action)
                ))) if action.value.eq_ignore_ascii_case("IGNORE")
            )
        {
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    })
    .is_break()
}

fn push(owner: &Role) -> Statement {
    ast_builder::insert(
        stack_name(),
        vec![ObjectName(vec![ObjectNamePart::Identifier(Ident::new("owner"))])],
        ast_builder::values(vec![vec![string_literal(owner.name())]]),
    )
}

fn pop() -> Statement {
    ast_builder::delete(stack_name(), Some(innermost_frame()))
}

fn stack_name() -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(Ident::new(STACK))])
}

fn is_stack(name: &ObjectName) -> bool {
    matches!(name.0.as_slice(), [ObjectNamePart::Identifier(ident)] if ident.value == STACK)
}
