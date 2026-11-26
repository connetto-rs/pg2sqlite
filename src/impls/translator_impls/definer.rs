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
//!
//! A foreign key action the replica runs as a trigger pushes a frame owned by
//! [`KEY_ACTION`], since PostgreSQL runs the action as the referencing
//! table's owner and past its row-level security, and the guards of a table
//! receiving such an action stand aside while that frame is the innermost.
//!
//! A `TRUNCATE` empties its tables under a frame owned by [`TRUNCATION`],
//! since PostgreSQL checks only the `TRUNCATE` privilege and fires no row
//! trigger. The guards of a table a role may truncate stand aside while that
//! frame is the innermost, and a row trigger a truncation could fire holds
//! only outside one.

use alloc::collections::BTreeSet;
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

use super::{
    create_trigger::{TriggerRelation, trigger_relation},
    plpgsql::{ReadBodies, body_reach},
    trigger_slots::folded,
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        function_helpers::{simple_function_expr, string_literal},
        object_name::{append_suffix, resolve_translation_table},
        privileges::{RoleTable, has_backing_table},
        query_builder::{from_relation, plain_table_factor, single_expr_query},
        translator_impls::off_replica::row_write,
    },
    traits::schema::trigger_function,
};

/// The frames of the definer bodies running on the connection, each holding
/// its body's owner, innermost last.
pub(crate) const STACK: &str = "pg2sqlite_definer_frames";

/// The owner of the frame a running foreign key action pushes. PostgreSQL
/// reserves the role names starting `pg_`, so no body's owner is this one.
pub(crate) const KEY_ACTION: &str = "pg_key_action";

/// The owner of the frame a running `TRUNCATE` pushes, reserved as
/// [`KEY_ACTION`] is.
pub(crate) const TRUNCATION: &str = "pg_truncate";

type Role = <ParserDB as DatabaseLike>::Role;

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
/// its privileges let through. The translator holds each body, and each
/// trigger it fires, to its owner's privileges instead.
pub(crate) fn definer_owners(schema: &ParserDB) -> Vec<&str> {
    let mut owners: Vec<&str> = running_owners(schema).into_iter().map(RoleLike::name).collect();
    owners.sort_unstable();
    owners
}

/// `owners`, with [`KEY_ACTION`] when a foreign key action writes `table`
/// past its guards, and with [`TRUNCATION`] when the acting role or the owner
/// of a definer body a trigger runs may truncate `table`.
///
/// # Errors
///
/// Fails as [`receives_key_actions`](super::hidden_referenced_rows::receives_key_actions)
/// does, and refuses an acting role the schema does not declare.
pub(crate) fn and_system_frames<'s>(
    mut owners: Vec<&'s str>,
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Vec<&'s str>, Error> {
    if super::hidden_referenced_rows::receives_key_actions(table, schema, options)? {
        owners.push(KEY_ACTION);
    }
    let mut truncates = RoleTable::acting(table, schema, options)?.truncates();
    for owner in running_owners(schema) {
        truncates = truncates || RoleTable::of(table, Some(owner), schema)?.truncates();
    }
    if truncates {
        owners.push(TRUNCATION);
    }
    Ok(owners)
}

/// `statements`, the body of a foreign key action, run under a frame owned by
/// [`KEY_ACTION`].
pub(crate) fn as_key_action(statements: Vec<Statement>) -> Vec<Statement> {
    let mut framed = Vec::with_capacity(statements.len() + 2);
    framed.push(push(KEY_ACTION, None));
    framed.extend(statements);
    framed.push(pop(None));
    framed
}

/// `statements`, the deletes emptying the tables of a `TRUNCATE`, run under a
/// frame owned by [`TRUNCATION`]. The push reads its owner from a `SELECT`,
/// which takes the condition of the `IF` a trigger body runs it in.
pub(crate) fn as_truncation(statements: Vec<Statement>) -> Vec<Statement> {
    let mut framed = Vec::with_capacity(statements.len() + 2);
    framed.push(ast_builder::insert(
        stack_name(),
        vec![ObjectName(vec![ObjectNamePart::Identifier(Ident::new("owner"))])],
        single_expr_query(string_literal(TRUNCATION), Vec::new(), None),
    ));
    framed.extend(statements);
    framed.push(pop(None));
    framed
}

/// The condition of a row trigger that holds outside every running
/// `TRUNCATE`, wherever its frame is on the stack, since PostgreSQL fires no
/// row trigger for the rows a `TRUNCATE` removes nor for the rows a key action
/// it sets off writes.
pub(crate) fn outside_truncation() -> Expr {
    Expr::Exists {
        subquery: Box::new(single_expr_query(
            Expr::Value(sqlparser::ast::Value::Number("1".to_string(), false).into()),
            from_relation(plain_table_factor(stack_name())),
            Some(Expr::BinaryOp {
                left: Box::new(Expr::Identifier(Ident::new("owner"))),
                op: BinaryOperator::Eq,
                right: Box::new(string_literal(TRUNCATION)),
            }),
        )),
        negated: true,
    }
}

/// The owners of the schema's definer functions whose writes can fire a
/// trigger on `table`, folded, directly or through the invoker triggers those
/// writes fire, each named once in name order. A body whose writes cannot be
/// read may write any table.
///
/// # Errors
///
/// Propagates a lookup failure.
pub(crate) fn owners_reaching<'s>(
    table: &str,
    schema: &'s ParserDB,
) -> Result<Vec<&'s Role>, Error> {
    let mut bodies = ReadBodies::default();
    // The tables each invoker trigger's table leads its writes to, `None`
    // for a body whose writes cannot be read.
    let mut fired: Vec<(String, Option<BTreeSet<String>>)> = Vec::new();
    for trigger in schema.triggers() {
        let Ok(TriggerRelation::Table(_)) = trigger_relation(trigger, schema) else { continue };
        let Some(exec_body) = &trigger.exec_body else { continue };
        let function = &exec_body.func_desc.name;
        let declared = trigger_function(schema, function)?;
        if let Some(declared) = declared
            && definer_owner(declared, schema)?.is_some()
        {
            continue;
        }
        let written =
            body_reach(function, declared, schema, &mut bodies).map(|reach| reach.written);
        fired.push((folded(&trigger.table_name), written));
    }
    let leads_to_table = |start: BTreeSet<String>| {
        let mut reached = start;
        let mut pending: Vec<String> = reached.iter().cloned().collect();
        while let Some(written) = pending.pop() {
            for (on, leads) in &fired {
                if *on != written {
                    continue;
                }
                let Some(leads) = leads else { return true };
                for next in leads {
                    if reached.insert(next.clone()) {
                        pending.push(next.clone());
                    }
                }
            }
        }
        reached.contains(table)
    };
    let mut owners: Vec<&Role> = Vec::new();
    for function in schema.functions() {
        let Some(owner) = definer_owner(function, schema)? else { continue };
        if owners.iter().any(|listed| listed.name() == owner.name()) {
            continue;
        }
        let reaches = match body_reach(&function.name, Some(function), schema, &mut bodies) {
            Some(reach) => leads_to_table(reach.written),
            None => true,
        };
        if reaches {
            owners.push(owner);
        }
    }
    owners.sort_unstable_by(|left, right| left.name().cmp(right.name()));
    Ok(owners)
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
    (!owners.is_empty()).then(|| innermost_among(owners, true))
}

/// The guard clause that holds while the innermost running definer body
/// belongs to `owner`.
pub(crate) fn running_owner(owner: &str) -> Expr {
    innermost_among(&[owner], false)
}

/// Whether the innermost running definer body belongs to one of `owners`,
/// or, `negated`, does not.
fn innermost_among(owners: &[&str], negated: bool) -> Expr {
    let listed = Expr::In {
        expr: Box::new(Expr::Identifier(Ident::new("owner"))),
        target: InTarget::List(owners.iter().map(|owner| string_literal(owner)).collect()),
        negated: false,
        global: false,
    };
    Expr::Exists {
        subquery: Box::new(single_expr_query(
            Expr::Value(sqlparser::ast::Value::Number("1".to_string(), false).into()),
            from_relation(plain_table_factor(stack_name())),
            Some(Expr::BinaryOp {
                left: Box::new(innermost_frame()),
                op: BinaryOperator::And,
                right: Box::new(listed),
            }),
        )),
        negated,
    }
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
/// through its backing table. The body runs with `owner` on [`STACK`] when
/// some table exempts it or it writes, so the guards let its writes through
/// and a trigger they fire runs as `owner` too. The translator has already
/// held each statement to `owner`'s privileges.
///
/// # Errors
///
/// Refuses a body naming a table whose policies bind `owner`, since SQLite
/// has no way to apply another role's policies.
pub(crate) fn run_as_owner(
    function: &CreateFunction,
    owner: &Role,
    statements: Vec<Statement>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let refusal = |table: &CreateTable| binding_refusal(function, table, owner);
    let (statements, guarded) = backed(owner, statements, &refusal, schema, options)?;
    Ok(framed(owner, statements, guarded, None))
}

/// `statements` reading and writing the backing tables `owner` reaches, and
/// whether they run with `owner` on [`STACK`], which they do when some table
/// exempts `owner` or they write.
///
/// # Errors
///
/// Refuses with `refusal` of a table whose policies bind `owner`.
fn backed(
    owner: &Role,
    mut statements: Vec<Statement>,
    refusal: &dyn Fn(&CreateTable) -> String,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(Vec<Statement>, bool), Error> {
    let guarded = exempt_somewhere(owner, schema)?
        || statements.iter().any(|statement| row_write(statement).is_some());
    for statement in &mut statements {
        let outcome = visit_relations_mut(statement, |name: &mut ObjectName| {
            match backing_or_refusal(owner, name, refusal, schema, options) {
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
    Ok((statements, guarded))
}

/// Rewrites the translated body of `function`, an invoker trigger function,
/// to run as `owner`, whose definer body fired it, every statement and frame
/// under `guard`, the condition that the trigger fired inside a body of
/// `owner`.
///
/// A body naming a table whose policies bind `owner` raises a refusal under
/// `guard` in place of the body, since SQLite has no way to apply another
/// role's policies, and the trigger may never fire that way.
///
/// # Errors
///
/// Propagates a lookup failure.
pub(crate) fn run_as_firing_owner(
    function: &ObjectName,
    owner: &Role,
    statements: Vec<Statement>,
    guard: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let refusal = |table: &CreateTable| {
        format!(
            "trigger function '{function}' runs as '{}', the owner of a SECURITY DEFINER \
             function whose write fires it, and reads or writes table '{}', whose row-level \
             security policies bind that owner. The translation cannot apply another role's \
             policies, only the caller's or none. Make the owner the table's owner or a \
             BYPASSRLS role, or declare the definer function SECURITY INVOKER.",
            owner.name(),
            table.table_name()
        )
    };
    for statement in &statements {
        let outcome = visit_relations(statement, |name: &ObjectName| {
            match binding_table(owner, name, schema, options) {
                Ok(Some(table)) => ControlFlow::Break(Ok(table)),
                Ok(None) => ControlFlow::Continue(()),
                Err(error) => ControlFlow::Break(Err(error)),
            }
        });
        if let ControlFlow::Break(bound) = outcome {
            let mut raised = ast_builder::raise_statement("ABORT", Some(&refusal(bound?)), None);
            crate::impls::translator_impls::condition_injection::inject_condition_into_dml_statement(
                &mut raised,
                guard.clone(),
            )?;
            return Ok(vec![raised]);
        }
    }
    let (statements, guarded) = backed(owner, statements, &refusal, schema, options)?;
    Ok(framed(owner, statements, guarded, Some(guard)))
}

/// `statements` with `owner` pushed on [`STACK`] around those that write
/// when `guarded`, each push and pop under `guard` when given.
fn framed(
    owner: &Role,
    statements: Vec<Statement>,
    guarded: bool,
    guard: Option<&Expr>,
) -> Vec<Statement> {
    if !guarded {
        return statements;
    }
    let mut wrapped = Vec::with_capacity(statements.len() + 2);
    let mut pushed = false;
    for statement in statements {
        let ignores = ignores_the_write(&statement);
        if ignores && pushed {
            wrapped.push(pop(guard));
        } else if !ignores && !pushed {
            wrapped.push(push(owner.name(), guard));
        }
        pushed = !ignores;
        wrapped.push(statement);
    }
    if pushed {
        wrapped.push(pop(guard));
    }
    wrapped
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
    let refusal = |table: &CreateTable| binding_refusal(function, table, owner);
    backing_or_refusal(owner, name, &refusal, schema, options)
}

/// As [`backing_name`], refusing with `refusal` of a table whose policies
/// bind `owner`.
fn backing_or_refusal(
    owner: &Role,
    name: &ObjectName,
    refusal: &dyn Fn(&CreateTable) -> String,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<ObjectName>, Error> {
    if let Some(table) = binding_table(owner, name, schema, options)? {
        return Err(Error::forward_refusal(refusal(table)));
    }
    let Some(table) = resolve_translation_table(schema, name)? else {
        return Ok(None);
    };
    if !has_backing_table(table, schema, options)? {
        return Ok(None);
    }
    Ok(Some(append_suffix(name, options.get_rls_table_suffix())))
}

/// The table `name` names when its row-level security policies bind
/// `owner`, which SQLite cannot apply.
fn binding_table<'s>(
    owner: &Role,
    name: &ObjectName,
    schema: &'s ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<&'s CreateTable>, Error> {
    let Some(table) = resolve_translation_table(schema, name)? else {
        return Ok(None);
    };
    Ok((has_backing_table(table, schema, options)?
        && table.has_row_level_security(schema)?
        && !exempts(table, owner, schema))
    .then_some(table))
}

fn binding_refusal(function: &CreateFunction, table: &CreateTable, owner: &Role) -> String {
    format!(
        "function '{}' is SECURITY DEFINER and reads or writes table '{}' as its owner '{}', \
         whom the table's row-level security policies bind. The translation cannot apply \
         another role's policies, only the caller's or none. Make the owner the table's owner \
         or a BYPASSRLS role, or declare the function SECURITY INVOKER.",
        function.name,
        table.table_name(),
        owner.name()
    )
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

fn push(owner: &str, guard: Option<&Expr>) -> Statement {
    let source = match guard {
        Some(guard) => single_expr_query(string_literal(owner), Vec::new(), Some(guard.clone())),
        None => ast_builder::values(vec![vec![string_literal(owner)]]),
    };
    ast_builder::insert(
        stack_name(),
        vec![ObjectName(vec![ObjectNamePart::Identifier(Ident::new("owner"))])],
        source,
    )
}

fn pop(guard: Option<&Expr>) -> Statement {
    let innermost = innermost_frame();
    let selection = match guard {
        Some(guard) => {
            Expr::BinaryOp {
                left: Box::new(innermost),
                op: BinaryOperator::And,
                right: Box::new(Expr::Nested(Box::new(guard.clone()))),
            }
        }
        None => innermost,
    };
    ast_builder::delete(stack_name(), Some(selection))
}

fn stack_name() -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(Ident::new(STACK))])
}

fn is_stack(name: &ObjectName) -> bool {
    matches!(name.0.as_slice(), [ObjectNamePart::Identifier(ident)] if ident.value == STACK)
}
