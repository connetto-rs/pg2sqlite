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

use core::ops::ControlFlow;

use sql_traits::{
    structs::{ParserDB, RowSecurity, RowStatement},
    traits::{DatabaseLike, FunctionLike, RoleLike, TableLike},
};
use sqlparser::ast::{
    BinaryOperator, CreateFunction, CreateTable, DataType, Expr, FunctionReturnType, Ident,
    InTarget, ObjectName, ObjectNamePart, Statement, visit_expressions, visit_relations,
    visit_relations_mut,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        function_helpers::{simple_function_expr, string_literal},
        object_name::{append_suffix, resolve_translation_table},
        privileges::has_backing_table,
        query_builder::{from_relation, plain_table_factor, single_expr_query},
    },
};

/// The owners of the definer bodies running on the connection, innermost
/// last.
pub(crate) const STACK: &str = "pg2sqlite_definer_stack";

type Role = <ParserDB as DatabaseLike>::Role;

/// `CREATE TABLE IF NOT EXISTS` for [`STACK`], which a translation needs
/// whenever one of its statements reads or writes it.
pub(crate) fn stack_table() -> Statement {
    ast_builder::create_table(
        stack_name(),
        vec![super::rls::generated_table_column("owner", DataType::Text, true)],
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
fn exempts(table: &CreateTable, owner: &Role, schema: &ParserDB) -> bool {
    matches!(table.row_security(RowStatement::Select, owner, schema), Ok(RowSecurity::Exempt(_)))
}

/// The owners of the schema's definer trigger functions that `table`'s
/// row-level security exempts, the ones whose bodies its guards let through.
///
/// A definer function whose owner is undeclared exempts nothing, and a trigger
/// running it is refused.
pub(crate) fn exempt_owners<'s>(table: &'s CreateTable, schema: &'s ParserDB) -> Vec<&'s str> {
    let mut owners = Vec::new();
    for function in schema.functions().filter(|function| returns_trigger(function)) {
        if let Ok(Some(owner)) = definer_owner(function, schema)
            && !owners.contains(&owner.name())
            && exempts(table, owner, schema)
        {
            owners.push(owner.name());
        }
    }
    owners.sort_unstable();
    owners
}

fn returns_trigger(function: &CreateFunction) -> bool {
    matches!(function.return_type, Some(FunctionReturnType::DataType(DataType::Trigger)))
}

/// The filter that selects the row of the innermost running definer body,
/// `rowid = (SELECT max(rowid) FROM <stack>)`.
fn innermost_stack_rowid() -> Expr {
    Expr::BinaryOp {
        left: Box::new(Expr::Identifier(Ident::new("rowid"))),
        op: BinaryOperator::Eq,
        right: Box::new(Expr::Subquery(Box::new(single_expr_query(
            simple_function_expr("max", vec![Expr::Identifier(Ident::new("rowid"))], None),
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
    let innermost = innermost_stack_rowid();
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

/// Rewrites a translated definer body to run as `owner`.
///
/// Every table with row-level security the body names is read and written
/// through its backing table, and the body runs with `owner` on [`STACK`]
/// when some table exempts it.
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
    let mut exempt_somewhere = false;
    for table in schema.tables() {
        if table.has_row_level_security(schema)? && exempts(table, owner, schema) {
            exempt_somewhere = true;
            break;
        }
    }
    for statement in &mut statements {
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

    if !exempt_somewhere {
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

/// The backing table `owner` reads and writes in place of `name`, `None` when
/// `name` is not a table with one. The owner reads past the acting role's
/// grants, and past row-level security where the policies exempt it.
fn backing_name(
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
            "trigger function '{}' is SECURITY DEFINER and reads or writes table '{}' as its \
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
    ast_builder::delete(stack_name(), Some(innermost_stack_rowid()))
}

fn stack_name() -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(Ident::new(STACK))])
}

fn is_stack(name: &ObjectName) -> bool {
    matches!(name.0.as_slice(), [ObjectNamePart::Identifier(ident)] if ident.value == STACK)
}
