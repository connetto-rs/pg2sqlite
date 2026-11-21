//! The statements and expressions of a trigger body that name a column
//! PostgreSQL does not find.
//!
//! PostgreSQL resolves the SQL of a PL/pgSQL body when a run first reaches
//! it, so the body loads and each run reaching such SQL fails there. SQLite
//! reads a double-quoted name no column in scope holds as a string, so each
//! is rewritten into a step that raises PostgreSQL's error where the run
//! reaches it.

use alloc::collections::BTreeSet;
use core::ops::ControlFlow;

use sql_traits::{structs::ParserDB, utils::identifier_resolution::stored_identifier};
use sqlparser::{
    ast::{
        CreateFunction, Expr, Ident, ObjectName, ObjectNamePart, SetExpr, Spanned, Statement,
        Value, visit_expressions, visit_expressions_mut, visit_relations,
    },
    tokenizer::Span,
};
use sqlparser_plpgsql::{
    embedded::{
        Assignment, Block, Branch, Condition, DeclarationKind, Node, NodeKind, Raise, RaiseLevel,
        RaiseOption, RaiseOptionKind, Target,
    },
    namespace::{FunctionKind, Signature, Sql, Write},
};
use sqlstate::SqlState;

use super::{PlPgSqlTranslator, TriggerFacts, raise::raise_statement};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        column_access::{Table, Variables, unresolved_in_body, unresolved_target},
        object_name::{last_ident, resolve_translation_table},
        query_builder::single_expr_query,
        translator_impls::{create_trigger::body_schema, trigger_slots::folded},
    },
    options::TranslationContext,
    traits::{schema::Schema, translator::TranslatorWithContext},
    warnings::WarningSink,
};

/// The field of `NEW` that marks where a maintenance body reaches the SQL of
/// one [`Unresolved`], followed by its index.
const REACHED: &str = "pg2sqlite_unresolved_";

/// SQL of a body naming a column PostgreSQL does not find, with the error
/// PostgreSQL raises where a run reaches it.
struct Unresolved {
    /// Where the body writes the SQL.
    span: Span,
    sqlstate: SqlState,
    message: String,
}

impl Unresolved {
    /// `RAISE` of PostgreSQL's error.
    fn raise(&self) -> Raise {
        let message =
            Expr::Value(Value::SingleQuotedString(self.message.clone()).with_empty_span());
        Raise {
            level: RaiseLevel::Exception,
            condition: Some(Condition::Sqlstate(self.sqlstate)),
            format: None,
            arguments: Vec::new(),
            options: vec![RaiseOption {
                kind: RaiseOptionKind::Message,
                value: single_expr_query(message, Vec::new(), None),
            }],
        }
    }
}

/// `block`, the body of the trigger function `function` that a trigger on
/// `table` runs, with each step holding SQL that names what PostgreSQL does
/// not find in `schema` rewritten into a `RAISE` of PostgreSQL's error, as
/// [`rewrite`] places it.
pub(crate) fn raising_unresolved(
    block: Block,
    function: &ObjectName,
    table: &ObjectName,
    schema: &ParserDB,
) -> Block {
    raising(block, &trigger_signature(function), rows(table, schema), schema)
}

/// `block`, the body of the helper `helper`, rewritten as
/// [`raising_unresolved`] rewrites a trigger function's.
pub(crate) fn helper_raising_unresolved(
    block: Block,
    helper: &CreateFunction,
    schema: &ParserDB,
) -> Block {
    let stored =
        |ident: &Ident| stored_identifier(&ident.value, ident.quote_style.is_some()).into_owned();
    let signature = Signature {
        name: last_ident(&helper.name).map_or_else(String::new, stored),
        parameters: helper
            .args
            .iter()
            .flatten()
            .map(|argument| argument.name.as_ref().map(stored))
            .collect(),
        kind: FunctionKind::Plain,
    };
    raising(block, &signature, None, schema)
}

/// `block` with each step naming what PostgreSQL does not find in `schema`
/// rewritten into a `RAISE` of PostgreSQL's error.
fn raising(
    mut block: Block,
    signature: &Signature,
    rows: Option<&Table>,
    schema: &ParserDB,
) -> Block {
    let found = unresolved(&block, signature, rows, schema);
    rewrite(&mut block, &found, &|_, unresolved| {
        Node { kind: NodeKind::Raise(unresolved.raise()), span: unresolved.span }
    });
    block
}

/// The table `table` names, whose row `NEW` and `OLD` hold.
fn rows<'s>(table: &ObjectName, schema: &'s ParserDB) -> Option<&'s Table> {
    resolve_translation_table(schema, table).ok().flatten()
}

/// The statements a maintenance trigger running `function` writes before
/// its `UPDATE`, one per statement or expression of the body that names a
/// column PostgreSQL does not find in `schema`, aborting with PostgreSQL's
/// error where the body reaches it.
///
/// The body only assigns fields of `NEW` under `IF`s, so where it reaches
/// such SQL is the value a field assigned true there and false at the start
/// holds when the body returns, folded as the assigned columns are.
///
/// # Errors
///
/// Returns the errors reading and translating the body returns, and refuses
/// a body that reads the whole row `NEW` after such SQL, which leaves no
/// condition to fold.
pub(crate) fn unresolved_aborts(
    function: &ObjectName,
    table: &ObjectName,
    facts: &TriggerFacts,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let body_schema = body_schema(schema, options);
    let Some(mut block) = body_schema.function_body(function)? else { return Ok(Vec::new()) };
    let signature = trigger_signature(function);
    let found = unresolved(&block, &signature, rows(table, body_schema), body_schema);
    if found.is_empty() {
        return Ok(Vec::new());
    }
    rewrite(&mut block, &found, &|index, unresolved| {
        let target =
            ObjectName::from(vec![Ident::new("NEW"), Ident::new(format!("{REACHED}{index}"))]);
        let reached = Expr::Value(Value::Boolean(true).with_empty_span());
        Node {
            kind: NodeKind::Assignment(Assignment {
                target: Target { name: target, indirection: Vec::new() },
                value: Box::new(single_expr_query(reached, Vec::new(), None)),
            }),
            span: unresolved.span,
        }
    });
    let fields = block.fields_of_new(&signature).map_err(|unfolded| {
        Error::forward_refusal(format!(
            "trigger function `{function}` names a column PostgreSQL does not find, and {unfolded}, \
             so the translation cannot tell where the body reaches that column"
        ))
    })?;
    let mut aborts = Vec::new();
    for field in fields {
        let Some(index) =
            field.field.value.strip_prefix(REACHED).and_then(|index| index.parse::<usize>().ok())
        else {
            continue;
        };
        let mut reached = field.value;
        let _: ControlFlow<()> = visit_expressions_mut(&mut reached, |expression| {
            if let Expr::CompoundIdentifier(parts) = expression
                && parts.last().is_some_and(|part| part.value == field.field.value)
            {
                *expression = Expr::Value(Value::Boolean(false).with_empty_span());
            }
            ControlFlow::Continue(())
        });
        let reached = PlPgSqlTranslator::fold_trigger_specials_in(reached, facts)?;
        let Some(Statement::Query(mut abort)) = raise_statement(&found[index].raise(), emit)?
        else {
            continue;
        };
        if let SetExpr::Select(select) = abort.body.as_mut() {
            select.selection = Some(reached);
        }
        aborts.extend(Statement::Query(abort).translate_with_warnings(schema, options, emit)?);
    }
    Ok(aborts)
}

/// What PL/pgSQL knows of the trigger function `function` beyond its body.
fn trigger_signature(function: &ObjectName) -> Signature {
    Signature {
        name: last_ident(function).map_or_else(String::new, |ident| {
            stored_identifier(&ident.value, ident.quote_style.is_some()).into_owned()
        }),
        parameters: Vec::new(),
        kind: FunctionKind::Trigger,
    }
}

/// The relations and functions a trigger body reaches.
pub(crate) struct BodyReach {
    /// The relations its SQL names, folded as PostgreSQL stores them.
    pub(crate) relations: BTreeSet<String>,
    /// The function itself and every function its SQL calls, folded.
    pub(crate) functions: BTreeSet<String>,
}

/// What `function`'s body in `schema` reaches, the bodies of the functions it
/// calls included, or `None` when its names are not all known before it runs,
/// as with SQL it builds at run time.
///
/// # Errors
///
/// Returns the error reading a body returns.
pub(crate) fn body_reach(
    function: &ObjectName,
    schema: &ParserDB,
) -> Result<Option<BodyReach>, Error> {
    let mut reach = BodyReach { relations: BTreeSet::new(), functions: BTreeSet::new() };
    let mut pending = vec![function.clone()];
    while let Some(function) = pending.pop() {
        if !reach.functions.insert(folded(&function)) {
            continue;
        }
        let block = match schema.function_body(&function) {
            Ok(Some(block)) => block,
            Ok(None) => continue,
            // A callee the schema cannot settle may reach anything.
            Err(_) if reach.functions.len() > 1 => return Ok(None),
            Err(error) => return Err(error),
        };
        let Ok(sites) = block.sites(&trigger_signature(&function), |_| true) else {
            return Ok(None);
        };
        for site in sites {
            let mut named = |relation: &ObjectName| {
                reach.relations.insert(folded(relation));
                ControlFlow::<()>::Continue(())
            };
            let mut called = |expr: &Expr| {
                if let Expr::Function(call) = expr {
                    pending.push(call.name.clone());
                }
                ControlFlow::<()>::Continue(())
            };
            let _: ControlFlow<()> = match site.sql {
                Sql::Statement(statement) => {
                    let _: ControlFlow<()> = visit_relations(statement, &mut named);
                    visit_expressions(statement, &mut called)
                }
                Sql::Expression(query) => {
                    let _: ControlFlow<()> = visit_relations(query, &mut named);
                    visit_expressions(query, &mut called)
                }
                Sql::Dynamic(_) => return Ok(None),
            };
        }
    }
    Ok(Some(reach))
}

/// Every statement and expression of `block`, the body of the trigger
/// function `signature` describes, that names what PostgreSQL does not find
/// in `schema`, and every assignment to a field of `NEW` or `OLD` that
/// `rows`, the trigger's table, lacks, in the order a run meets them.
///
/// A body whose names PL/pgSQL does not follow, through an `ALIAS FOR` or a
/// cursor naming nothing, has none, and its translation refuses it.
fn unresolved(
    block: &Block,
    signature: &Signature,
    rows: Option<&Table>,
    schema: &ParserDB,
) -> Vec<Unresolved> {
    let (Ok(sites), Ok(writes)) =
        (block.sites(signature, |_| true), block.write_sites(signature, |_| true))
    else {
        return Vec::new();
    };
    let read = sites.into_iter().filter_map(|site| {
        let span = match site.sql {
            Sql::Statement(statement) => statement.span(),
            Sql::Expression(query) => query.span(),
            Sql::Dynamic(_) => return None,
        };
        let variables = Variables { namespace: &site.namespace, rows };
        let (sqlstate, message) = unresolved_in_body(site.sql, &variables, schema)?;
        Some((span.start, Unresolved { span, sqlstate, message }))
    });
    // PostgreSQL resolves an assignment's target before its value.
    let assigned = writes.iter().filter_map(|write| {
        let Write::Node(node @ Node { kind: NodeKind::Assignment(assignment), .. }) = write.write
        else {
            return None;
        };
        let parts: Vec<Ident> =
            assignment.target.name.0.iter().filter_map(ObjectNamePart::as_ident).cloned().collect();
        let variables = Variables { namespace: &write.namespace, rows };
        let (sqlstate, message) = unresolved_target(&parts, &variables, schema)?;
        Some((node.span.start, Unresolved { span: node.span, sqlstate, message }))
    });
    let mut found: Vec<_> = read.chain(assigned).collect();
    found.sort_by_key(|(at, _)| (at.line, at.column));
    found.into_iter().map(|(_, unresolved)| unresolved).collect()
}

/// Rewrites `block` so that each step holding SQL of `found` is `step`, built
/// for that SQL, where a run of the body would evaluate it.
///
/// An `IF` or `CASE` whose condition names such a column keeps the branches
/// tested before it, and runs `step` where none of them is taken. A default
/// of the body's own declarations runs as the body is entered, so its `step`
/// opens the body. Any other step holding such SQL, a nested block whose
/// declarations hold it included, becomes `step`.
fn rewrite(block: &mut Block, found: &[Unresolved], step: &dyn Fn(usize, &Unresolved) -> Node) {
    if found.is_empty() {
        return;
    }
    rewrite_nodes(&mut block.statements, found, step);
    for handler in &mut block.handlers {
        rewrite_nodes(&mut handler.statements, found, step);
    }
    let defaults = block.declarations.iter_mut().filter_map(|declaration| {
        match &mut declaration.kind {
            DeclarationKind::Variable(variable) => Some(&mut variable.default),
            DeclarationKind::Alias(_) | DeclarationKind::Cursor(_) => None,
        }
    });
    for default in defaults {
        let unresolved = default.as_ref().and_then(|value| position_within(found, value.span()));
        if let Some(index) = unresolved {
            *default = None;
            block.statements.insert(0, step(index, &found[index]));
            break;
        }
    }
}

fn rewrite_nodes(
    nodes: &mut [Node],
    found: &[Unresolved],
    step: &dyn Fn(usize, &Unresolved) -> Node,
) {
    for node in nodes {
        if position_within(found, node.span).is_none() {
            continue;
        }
        let inner: Vec<Span> = children_mut(&mut node.kind)
            .into_iter()
            .flat_map(|statements| statements.iter().map(|child| child.span))
            .collect();
        let owned = found.iter().position(|unresolved| {
            encloses(node.span, unresolved.span)
                && !inner.iter().any(|child| encloses(*child, unresolved.span))
        });
        if let Some(index) = owned {
            let raising = step(index, &found[index]);
            let Some((branches, otherwise, first)) =
                later_condition(&mut node.kind, found[index].span)
            else {
                *node = raising;
                continue;
            };
            branches.truncate(first);
            *otherwise = vec![raising];
        }
        for statements in children_mut(&mut node.kind) {
            rewrite_nodes(statements, found, step);
        }
    }
}

/// The branches and the `ELSE` of `kind`, an `IF` or a `CASE`, with the
/// branch whose condition holds `span`, when that is not the first branch.
fn later_condition(
    kind: &mut NodeKind,
    span: Span,
) -> Option<(&mut Vec<Branch>, &mut Vec<Node>, usize)> {
    let (branches, otherwise) = match kind {
        NodeKind::If(conditional) => (&mut conditional.branches, &mut conditional.otherwise),
        NodeKind::Case(case) => (&mut case.branches, case.otherwise.get_or_insert_with(Vec::new)),
        _ => return None,
    };
    let tested = branches.iter().position(|branch| encloses(branch.condition.span(), span))?;
    (tested > 0).then_some((branches, otherwise, tested))
}

/// The index of the first of `found` written inside `span`.
fn position_within(found: &[Unresolved], span: Span) -> Option<usize> {
    found.iter().position(|unresolved| encloses(span, unresolved.span))
}

/// Whether `outer` holds all of `inner`, which the body writes somewhere.
fn encloses(outer: Span, inner: Span) -> bool {
    inner != Span::empty() && outer.start <= inner.start && inner.end <= outer.end
}

/// The lists of steps `kind` runs inside it.
fn children_mut(kind: &mut NodeKind) -> Vec<&mut Vec<Node>> {
    match kind {
        NodeKind::Block(block) => {
            core::iter::once(&mut block.statements)
                .chain(block.handlers.iter_mut().map(|handler| &mut handler.statements))
                .collect()
        }
        NodeKind::If(conditional) => {
            conditional
                .branches
                .iter_mut()
                .map(|branch| &mut branch.statements)
                .chain([&mut conditional.otherwise])
                .collect()
        }
        NodeKind::Case(case) => {
            case.branches
                .iter_mut()
                .map(|branch| &mut branch.statements)
                .chain(case.otherwise.as_mut())
                .collect()
        }
        NodeKind::Loop(looped) => vec![&mut looped.statements],
        _ => Vec::new(),
    }
}
