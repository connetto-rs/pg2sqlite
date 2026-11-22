//! PL/pgSQL bodies read with the labels PostgreSQL reads past dropped.
//!
//! PL/pgSQL labels the function's outermost level with the function's name
//! and each block with the label written before it, and `label.name` reaches
//! the variable `name` that level declares, so `refuse.old.id` reads the
//! trigger's `OLD`. Where the bare name reaches the same variable, the body
//! without the label runs the same, and every later reading of the body
//! meets only the bare names it follows.

use alloc::{borrow::Cow, collections::BTreeSet, format, string::String, vec::Vec};
use core::ops::ControlFlow;

use sql_traits::{structs::ParserDB, utils::identifier_resolution::stored_ident_name};
use sqlparser::{
    ast::{
        CreateFunction, CreateTrigger, Expr, Ident, ObjectNamePart, Select, SelectItem,
        SelectItemQualifiedWildcardKind, Visit, Visitor, visit_expressions_mut,
    },
    tokenizer::Location,
};
use sqlparser_plpgsql::{
    embedded::{self, Block, DiagnosticsItem, LoopControl, NodeKind, Target, embedded_sql},
    namespace::{FunctionKind, Namespace, Signature, Sql, Write},
};

use crate::{
    errors::Error,
    impls::{object_name::last_ident, translator_impls::definer::returns_trigger},
    traits::schema::{Schema, written_function},
};

/// `function`'s PL/pgSQL body `text`, parsed with every label dropped that
/// qualifies a reference the bare name reaches the same.
///
/// # Errors
///
/// Returns what `unparsed` makes of the error parsing `text`, and refuses a
/// label reaching past a variable an inner block declares under the bare
/// name, which no body without the label reads the same.
pub(crate) fn unlabelled_body(
    function: &CreateFunction,
    text: &str,
    unparsed: impl Fn(embedded::Error) -> Error,
) -> Result<Block, Error> {
    let signature = signature(function);
    let mut text = Cow::Borrowed(text);
    loop {
        let block = embedded_sql(&text).map_err(&unparsed)?;
        let labels = droppable_labels(&block, &signature)?;
        if labels.is_empty() {
            return Ok(block);
        }
        text = Cow::Owned(without(&text, &labels));
    }
}

/// The names every statement of the body of the function `trigger` runs in
/// `schema` reaches, when the body is one whose fields of `NEW` fold, which
/// declares nothing and nests no block or loop.
///
/// # Errors
///
/// Returns the errors reading the body returns.
pub(crate) fn folded_namespace(
    trigger: &CreateTrigger,
    schema: &ParserDB,
) -> Result<Option<Namespace>, Error> {
    let Some(body) = &trigger.exec_body else { return Ok(None) };
    let function = &body.func_desc.name;
    Ok(written_function(schema, function)?.zip(schema.function_body(function)?).and_then(
        |(written, block)| Some(block.sites(&signature(written), |_| true).ok()?.pop()?.namespace),
    ))
}

/// `value`, an expression a folded body reaching `namespace` evaluates, with
/// every label dropped that [`unlabelled_body`] drops. A folded body
/// declares nothing that could hide a name of the function's level.
pub(crate) fn unlabelled_value(mut value: Expr, namespace: Option<&Namespace>) -> Expr {
    let Some(namespace) = namespace else { return value };
    let _: ControlFlow<()> = visit_expressions_mut(&mut value, |expr| {
        if let Expr::CompoundIdentifier(parts) = expr {
            let drops =
                matches!(droppable(&parts.iter().collect::<Vec<_>>(), namespace), Ok(Some(_)));
            if drops {
                parts.remove(0);
                if parts.len() == 1 {
                    *expr = Expr::Identifier(parts.remove(0));
                }
            }
        }
        ControlFlow::Continue(())
    });
    value
}

/// What PL/pgSQL knows of `function` beyond its body.
pub(crate) fn signature(function: &CreateFunction) -> Signature {
    let stored = |ident: &Ident| stored_ident_name(ident).into_owned();
    Signature {
        name: last_ident(&function.name).map_or_else(String::new, stored),
        parameters: function
            .args
            .iter()
            .flatten()
            .map(|argument| argument.name.as_ref().map(stored))
            .collect(),
        kind: if returns_trigger(function) { FunctionKind::Trigger } else { FunctionKind::Plain },
    }
}

/// Where each label `block` can drop starts, and where the name it
/// qualifies starts.
///
/// A body whose names PL/pgSQL does not follow, through an `ALIAS FOR` or a
/// cursor naming nothing, drops none, and its readings refuse it.
fn droppable_labels(
    block: &Block,
    signature: &Signature,
) -> Result<BTreeSet<(Location, Location)>, Error> {
    let (Ok(sites), Ok(writes)) =
        (block.sites(signature, |_| true), block.write_sites(signature, |_| true))
    else {
        return Ok(BTreeSet::new());
    };
    let mut labels = BTreeSet::new();
    for site in &sites {
        let mut reach = Labels { namespace: &site.namespace, found: &mut labels };
        let flow = match site.sql {
            Sql::Statement(statement) => statement.visit(&mut reach),
            Sql::Expression(query) => query.visit(&mut reach),
            Sql::Dynamic(dynamic) => {
                core::iter::once(&*dynamic.command)
                    .chain(&dynamic.arguments)
                    .try_for_each(|query| query.visit(&mut reach))
            }
        };
        if let ControlFlow::Break(error) = flow {
            return Err(error);
        }
    }
    for write in &writes {
        let Write::Node(node) = write.write else { continue };
        for target in targets(&node.kind) {
            let parts: Vec<&Ident> =
                target.name.0.iter().filter_map(ObjectNamePart::as_ident).collect();
            if let Some([label, name]) = droppable(&parts, &write.namespace)? {
                labels.insert((label.span.start, name.span.start));
            }
        }
    }
    Ok(labels)
}

/// The label of the reference `parts` and the name after it, when
/// `namespace` reaches the same variable through the rest of the reference.
///
/// # Errors
///
/// Refuses a label reaching past a variable an inner block declares under
/// the bare name.
fn droppable<'p>(
    parts: &[&'p Ident],
    namespace: &Namespace,
) -> Result<Option<[&'p Ident; 2]>, Error> {
    let [label, name, ..] = parts else { return Ok(None) };
    let stored: Vec<Cow<'_, str>> = parts.iter().map(|part| stored_ident_name(part)).collect();
    let Some((labelled, 2)) = namespace.lookup(&stored, |_| true) else { return Ok(None) };
    if namespace.lookup(&stored[1..], |_| true).is_some_and(|(bare, _)| bare == labelled) {
        return Ok(Some([*label, *name]));
    }
    Err(Error::forward_refusal(format!(
        "`{label}.{name}` reaches the variable `{name}` of the level `{label}` labels, past a \
         `{name}` an inner block declares, so no body without the label reads the same. Rename \
         the inner `{name}`."
    )))
}

/// The variables `kind` assigns, as written.
fn targets(kind: &NodeKind) -> impl Iterator<Item = &Target> {
    let (targets, items): (&[Target], &[DiagnosticsItem]) = match kind {
        NodeKind::Assignment(assignment) => (core::slice::from_ref(&assignment.target), &[]),
        NodeKind::Statement { into: Some(into), .. }
        | NodeKind::Dynamic { into: Some(into), .. } => (&into.targets, &[]),
        NodeKind::Fetch(fetch) => (&fetch.targets, &[]),
        NodeKind::Diagnostics(diagnostics) => (&[], &diagnostics.items),
        NodeKind::Loop(running) => {
            match &running.control {
                LoopControl::Query { targets, .. }
                | LoopControl::Dynamic { targets, .. }
                | LoopControl::Array { targets, .. } => (targets, &[]),
                _ => (&[], &[]),
            }
        }
        _ => (&[], &[]),
    };
    targets.iter().chain(items.iter().map(|item| &item.target))
}

/// `text` without the characters from each start in `labels` to its end.
///
/// Lines and columns count from 1, as `sqlparser` counts them.
fn without(text: &str, labels: &BTreeSet<(Location, Location)>) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut at = Location::new(1, 1);
    for character in text.chars() {
        if !labels.iter().any(|(start, end)| *start <= at && at < *end) {
            kept.push(character);
        }
        if character == '\n' {
            at = Location::new(at.line + 1, 1);
        } else {
            at.column += 1;
        }
    }
    kept
}

/// The labels the SQL of one site can drop, added to `found`.
struct Labels<'n> {
    /// The names the SQL reaches.
    namespace: &'n Namespace,
    found: &'n mut BTreeSet<(Location, Location)>,
}

impl Labels<'_> {
    /// Adds the label of `parts` when the site can drop it.
    fn reference(&mut self, parts: &[&Ident]) -> ControlFlow<Error> {
        match droppable(parts, self.namespace) {
            Ok(Some([label, name])) => {
                self.found.insert((label.span.start, name.span.start));
                ControlFlow::Continue(())
            }
            Ok(None) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    }
}

impl Visitor for Labels<'_> {
    type Break = Error;

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Error> {
        match expr {
            Expr::CompoundIdentifier(parts) => self.reference(&parts.iter().collect::<Vec<_>>()),
            _ => ControlFlow::Continue(()),
        }
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Error> {
        for item in &select.projection {
            if let SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(record),
                _,
            ) = item
            {
                self.reference(
                    &record.0.iter().filter_map(ObjectNamePart::as_ident).collect::<Vec<_>>(),
                )?;
            }
        }
        ControlFlow::Continue(())
    }
}
