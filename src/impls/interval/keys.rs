//! The comparison key of an interval value, and the sides a key can read.
//!
//! An interval column holds the text PostgreSQL prints. The key is the
//! fixed-width text whose byte order is PostgreSQL's interval order, so a
//! comparison, sort, grouping or extremum runs on the key rather than on
//! the stored text.
//!
//! Two forms are emitted. Where a subquery is allowed, the key binds the
//! operand, its tokens and its fields once through nested scalar subqueries.
//! In an index expression, a CHECK constraint, a partial-index predicate or
//! a generated column SQLite forbids subqueries, so the inline form repeats
//! the operand at every read, and is only legal over an operand that
//! replays.

use alloc::borrow::Cow;
#[cfg(not(feature = "std"))]
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::ops::ControlFlow;

use sql_traits::{errors::LookupError, structs::ParserDB};
use sqlparser::{
    ast::{
        BinaryOperator, Distinct, DuplicateTreatment, Expr, Function, FunctionArg, FunctionArgExpr,
        FunctionArgumentList, FunctionArguments, GroupByExpr, Ident, ObjectName, ObjectNamePart,
        Select, SelectItem, SetExpr, TableFactor, Value, ValueWithSpan, VisitMut, VisitorMut,
    },
    dialect::SQLiteDialect,
    parser::Parser,
    tokenizer::Span,
};

use super::{
    Typmod, interval_data_type_typmod, is_interval_data_type,
    keys_templates::{self, PLACEHOLDER},
    literal_comparison_key, literal_text, qualifier_typmod,
};
use crate::{
    errors::Error,
    impls::{
        expression_type::{UNKNOWN, is_interval_typed, type_name},
        function_helpers::{integer_literal, simple_function_expr, string_literal},
        object_name::last_ident,
        replay::{is_replayable, reject_duplicated_operand},
        shared_helpers::{
            function_argument_exprs, inspect_declared_in_scope, query_output_carries,
            scope_declines_column, set_branch_output_carries_interval,
        },
    },
    options::TranslationContext,
    traits::translator::TranslatorWithContext,
    warnings::WarningSink,
};

/// The string variants a literal beside an interval may be written as.
fn string_literal_text(value: &Value) -> Option<String> {
    match value {
        Value::SingleQuotedString(text)
        | Value::NationalStringLiteral(text)
        | Value::EscapedStringLiteral(text)
        | Value::UnicodeStringLiteral(text) => Some(text.clone()),
        Value::DollarQuotedString(quoted) => Some(quoted.value.clone()),
        _ => None,
    }
}

/// The literal a value expression holds, through one level of parentheses.
fn value_of(expr: &Expr) -> Option<&Value> {
    match expr {
        Expr::Value(ValueWithSpan { value, .. }) => Some(value),
        Expr::Nested(inner) => value_of(inner),
        _ => None,
    }
}

/// What a side beside an interval read answers.
enum IntervalSide {
    /// A string literal the statement does not type. Beside an `IN` or a
    /// `BETWEEN` PostgreSQL types it as a plain interval; inside an array
    /// literal it resolves as text.
    BareLiteral(String),
    /// A literal the statement types as an interval, a cast or an
    /// `INTERVAL '…'` qualifier setting the typmod it keeps.
    TypedLiteral { text: String, typmod: Option<Typmod> },
    /// The side holds the canonical text the key reads, a column or a bound
    /// parameter: key it at run time.
    Keyed(Box<Expr>),
    /// The side is NULL, which keys to NULL.
    Null,
    /// The side answers no interval the statement types.
    Other,
}

/// How a string literal the statement does not type is read beside an
/// interval.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BareLiterals {
    /// `IN` and `BETWEEN` type the element as a plain interval.
    AsInterval,
    /// `= ANY` and `<> ALL` resolve the array literal as text, and
    /// PostgreSQL refuses the operator.
    AsText,
}

/// The interval side of `side`, or the refusal it answers. A side whose type
/// the statement cannot settle answers `Other`, so a read beside a settled
/// interval refuses it rather than run on the stored text.
///
/// # Errors
///
/// Propagates the refusal an `INTERVAL` over a non-literal operand answers,
/// and the refusal a cast to `interval` over a non-literal answers.
fn interval_side(
    side: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<IntervalSide, Error> {
    let side = match side {
        Expr::Nested(inner) => inner.as_ref(),
        _ => side,
    };
    match side {
        Expr::Value(ValueWithSpan { value, .. }) => {
            Ok(match value {
                Value::Null => IntervalSide::Null,
                // A bound parameter holds canonical text, so the key reads it
                // as it reads a column.
                Value::Placeholder(_) => IntervalSide::Keyed(Box::new(side.clone())),
                other => {
                    match string_literal_text(other) {
                        Some(text) => IntervalSide::BareLiteral(text),
                        None => IntervalSide::Other,
                    }
                }
            })
        }
        Expr::Interval(interval) => {
            let Some(text) = value_of(interval.value.as_ref()).and_then(string_literal_text) else {
                return Err(Error::forward_refusal(
                    "an INTERVAL over a non-literal operand cannot be keyed: the key reads the \
                     text PostgreSQL prints, and the operand's text is not known at \
                     translation time. Read the operand into an interval column first."
                        .to_string(),
                ));
            };
            Ok(IntervalSide::TypedLiteral { text, typmod: qualifier_typmod(interval)? })
        }
        Expr::Cast { expr, data_type, .. } if is_interval_data_type(data_type) => {
            interval_side_cast(expr, data_type)
        }
        _ => {
            Ok(if is_interval_typed(side, schema, options) {
                IntervalSide::Keyed(Box::new(side.clone()))
            } else {
                IntervalSide::Other
            })
        }
    }
}

/// The interval side of a cast's inner expression. `NULL` cast to interval
/// is a `NULL` side, and a bound parameter cast to interval is keyed, since
/// the parameter holds the interval's canonical text the way a column does.
/// A string literal is keyed through its text, and any other operand answers
/// the refusal the cast spells.
fn interval_side_cast(
    expr: &Expr,
    data_type: &sqlparser::ast::DataType,
) -> Result<IntervalSide, Error> {
    match value_of(expr) {
        Some(Value::Null) => Ok(IntervalSide::Null),
        Some(Value::Placeholder(_)) => Ok(IntervalSide::Keyed(Box::new(expr.clone()))),
        _ => {
            let Some(text) = value_of(expr).and_then(string_literal_text) else {
                return Err(cast_spelling_refusal());
            };
            Ok(IntervalSide::TypedLiteral { text, typmod: interval_data_type_typmod(data_type)? })
        }
    }
}

/// The refusal a cast to `interval` over a non-literal answers.
fn cast_spelling_refusal() -> Error {
    Error::forward_refusal(
        "a cast to interval over a non-literal cannot be keyed: any of PostgreSQL's spellings \
         may be stored in the text it reads, and the key reads only the canonical text the \
         replica writes. Store the cast in an interval column first."
            .to_string(),
    )
}

/// The PostgreSQL type name a refusal message spells.
pub(crate) fn message_type_name(normalized: &str) -> String {
    let lower = normalized.to_ascii_lowercase();
    if lower == "int" { "integer".to_string() } else { lower }
}

/// The PostgreSQL type name of `expr` spelled for a refusal message, or
/// `unknown` when nothing the statement writes decides it.
pub(crate) fn message_type_name_of(
    expr: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> String {
    match type_name(expr, schema, options).ok().flatten() {
        Some(name) => message_type_name(&name),
        None => UNKNOWN.to_string(),
    }
}

/// The refusal PostgreSQL answers for a side its `interval {op}` operator has
/// no form for.
fn operator_does_not_exist(
    side: &Expr,
    operator: &str,
    other: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Error {
    Error::forward_refusal(format!(
        "operator does not exist: {} {} {}.",
        message_type_name_of(side, schema, options),
        operator,
        message_type_name_of(other, schema, options),
    ))
}

/// Replaces every `PLACEHOLDER` identifier in the parsed `template` with a
/// clone of `operand`, one owned copy per occurrence.
fn substitute(operand: &Expr, template: &str) -> Result<Expr, Error> {
    let mut expr = Parser::new(&SQLiteDialect {})
        .try_with_sql(template)
        .and_then(|mut parser| parser.parse_expr())
        .map_err(|error| {
            Error::forward_refusal(format!(
                "the interval key template fails to parse as an expression: {error}"
            ))
        })?;
    let mut replacer = ReplacePlaceholder { operand: operand.clone() };
    let _: ControlFlow<()> = expr.visit(&mut replacer);
    Ok(expr)
}

/// Replaces the key template's operand identifier with the caller's operand.
struct ReplacePlaceholder {
    operand: Expr,
}

impl VisitorMut for ReplacePlaceholder {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        if matches!(
            expr,
            Expr::Identifier(ident) if ident.value == PLACEHOLDER
        ) {
            *expr = self.operand.clone();
        }
        ControlFlow::Continue(())
    }
}

/// The comparison key of the translated `operand`, in the bound form where a
/// subquery is allowed and in the inline form where one is not.
///
/// # Errors
///
/// Answers the refusal the key template fails to parse, and the refusal an
/// inline key over a non-replayable operand answers.
pub(crate) fn key_expr(operand: &Expr, options: &TranslationContext<'_>) -> Result<Expr, Error> {
    if options.allows_subqueries() {
        return substitute(operand, keys_templates::BOUND_KEY);
    }
    if !is_replayable(operand, options) {
        return Err(reject_duplicated_operand("an interval comparison key", operand));
    }
    substitute(operand, keys_templates::INLINE_KEY)
}

/// The comparison key of the translated `operand` with the operand's own
/// text kept behind it, as `min` and `max` pick the extreme's text off.
///
/// # Errors
///
/// Answers the refusal the key template fails to parse, and the refusal a
/// position with no subquery answers.
pub(crate) fn keyed_text_expr(
    operand: &Expr,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    if !options.allows_subqueries() {
        return Err(Error::forward_refusal(
            "min and max over an interval read the comparison key, which is a scalar subquery \
             SQLite does not allow in this position. Move the aggregate to a query body."
                .to_string(),
        ));
    }
    substitute(operand, keys_templates::BOUND_KEYED_TEXT)
}

/// The comparison key of the interval literal `text` under `typmod`, as a
/// string literal, or the refusal PostgreSQL answers.
pub(crate) fn literal_key(text: &str, typmod: Option<Typmod>) -> Result<Expr, Error> {
    Ok(string_literal(&literal_comparison_key(text, typmod)?))
}

/// The keyed text of the interval literal `text` under `typmod`: the
/// comparison key with the canonical text kept behind it, as `min` and
/// `max` pick the text off.
pub(crate) fn literal_keyed_text(text: &str, typmod: Option<Typmod>) -> Result<Expr, Error> {
    Ok(Expr::BinaryOp {
        left: Box::new(literal_key(text, typmod)?),
        op: BinaryOperator::StringConcat,
        right: Box::new(string_literal(&literal_text(text, typmod)?)),
    })
}

/// The key of one side of an interval read, or the refusal.
///
/// `operator` and `other` name the read the refusal answers for, in
/// PostgreSQL's `operator does not exist` spelling, and `bare_literals`
/// decides how a string literal the statement does not type is read.
///
/// # Errors
///
/// Propagates the refusal the side answers, and the refusal a side its
/// `interval {op}` operator has no form for answers.
pub(crate) fn operand_key(
    side: &Expr,
    operator: &str,
    other: &Expr,
    bare_literals: BareLiterals,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    match interval_side(side, schema, options)? {
        IntervalSide::Null => {
            Ok(Expr::Value(ValueWithSpan { value: Value::Null, span: Span::empty() }))
        }
        IntervalSide::BareLiteral(text) => {
            match bare_literals {
                BareLiterals::AsInterval => literal_key(&text, None),
                BareLiterals::AsText => {
                    Err(Error::forward_refusal(format!(
                        "operator does not exist: interval {operator} text. An array literal beside an \
                 interval resolves as text, which PostgreSQL refuses the operator for. Type the \
                 elements, as '1 day'::interval."
                    )))
                }
            }
        }
        IntervalSide::TypedLiteral { text, typmod } => literal_key(&text, typmod),
        IntervalSide::Keyed(expr) => {
            let translated = expr.translate_with_warnings(schema, options, emit)?;
            key_expr(&translated, options)
        }
        IntervalSide::Other => Err(operator_does_not_exist(side, operator, other, schema, options)),
    }
}

/// The comparison key of an `ORDER BY`, `GROUP BY` or `PARTITION BY` item, or
/// `None` when the item keeps its ordinary translation. An item that settles
/// no interval resolves against the select's own output items, `projection`,
/// by output name and then by ordinal position. An item that settles an
/// interval resolves against the output names first, since PostgreSQL reads
/// an `ORDER BY` name as an output column before it reads it as an input
/// expression, and the resolved expression is keyed only when it is an
/// interval. A `group_by` name that resolves to a column in the current scope
/// names the input column and keeps its ordinary translation.
///
/// # Errors
///
/// Propagates the refusal the item answers.
pub(crate) fn position_key(
    side: &Expr,
    projection: Option<&[SelectItem]>,
    group_by: bool,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Expr>, Error> {
    let resolved = match interval_side(side, schema, options)? {
        IntervalSide::BareLiteral(text) => return Ok(Some(literal_key(&text, None)?)),
        IntervalSide::TypedLiteral { text, typmod } => {
            return Ok(Some(literal_key(&text, typmod)?));
        }
        IntervalSide::Null | IntervalSide::Other | IntervalSide::Keyed(_) => side,
    };
    let resolved = projection
        .map(|projection| select_list_item(resolved, projection, group_by, schema, options))
        .transpose()?
        .flatten()
        .unwrap_or(resolved);
    let translated = resolved.translate_with_warnings(schema, options, emit)?;
    if is_interval_typed(resolved, schema, options) {
        return Ok(Some(key_expr(&translated, options)?));
    }
    Ok(Some(translated))
}

/// The projection item an `ORDER BY` or `GROUP BY` item names, or the
/// refusal the name's ambiguities answer. `None` when the item keeps its
/// ordinary translation. A `GROUP BY` name that resolves to a column of a
/// relation in the current scope names that input column, and a name two of
/// those relations expose answers PostgreSQL's `column reference` ambiguity.
/// A name that settles no input column matches the select's own output
/// columns by the name PostgreSQL compares them under, an unquoted spelling
/// folded to lower case and a quoted one as written, taking the first of
/// equal matches.
///
/// # Errors
///
/// Propagates the refusal a name PostgreSQL refuses answers.
fn select_list_item<'a>(
    item: &Expr,
    projection: &'a [SelectItem],
    group_by: bool,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<&'a Expr>, Error> {
    let Expr::Identifier(ident) = item else {
        return Ok(ordinal_projection_item(item, projection));
    };
    let name = folded_name(ident);
    if group_by && !scope_declines_column(item, &ident.value, options) {
        if scope_names_two_relations(item, options) {
            return Err(column_reference_ambiguous(&name));
        }
        if inspect_declared_in_scope(item, schema, options, |_| Some(()), |_, _, _| Ok(Some(())))
            .is_ok_and(|found| found.is_some())
        {
            // The name resolves to a column in the current scope, so
            // PostgreSQL's GROUP BY names the input column and the item
            // keeps its ordinary translation.
            return Ok(None);
        }
    }
    let named: Vec<&Expr> = projection
        .iter()
        .filter(|candidate| {
            projection_item_name(candidate).is_some_and(|output| output.as_ref() == name.as_ref())
        })
        .filter_map(projection_item_expr)
        .collect();
    match named.as_slice() {
        [] => Ok(None),
        [named] => Ok(Some(*named)),
        _ if named.iter().all(|expression| *expression == named[0]) => Ok(Some(named[0])),
        _ => Err(ambiguous_output_refusal(&name, group_by)),
    }
}

/// The name PostgreSQL compares an identifier under: an unquoted spelling
/// folded to lower case, a quoted one as written.
fn folded_name(ident: &Ident) -> Cow<'_, str> {
    if ident.quote_style.is_none() {
        Cow::Owned(ident.value.to_ascii_lowercase())
    } else {
        Cow::Borrowed(&ident.value)
    }
}

/// Whether the relations in scope expose the name in more than one of them,
/// the ambiguity PostgreSQL's `colNameToVar` answers for a `GROUP BY` name.
fn scope_names_two_relations(item: &Expr, options: &TranslationContext<'_>) -> bool {
    for definition in options.column_definitions(item, None) {
        match definition {
            Err(error) => return matches!(error, LookupError::AmbiguousTableLookup { .. }),
            Ok(Some(_)) => return false,
            Ok(None) => {}
        }
    }
    false
}

/// The refusal PostgreSQL answers for a `GROUP BY` name two relations in
/// scope expose.
fn column_reference_ambiguous(name: &str) -> Error {
    Error::forward_refusal(format!(
        "column reference \"{name}\" is ambiguous: more than one relation in scope exposes the \
         name, and PostgreSQL refuses the clause. Qualify the name to name one of the relations."
    ))
}

/// The refusal a name that names more than one unequal output column of an
/// `ORDER BY` or `GROUP BY` answers.
fn ambiguous_output_refusal(name: &str, group_by: bool) -> Error {
    Error::forward_refusal(format!(
        "{} \"{name}\" is ambiguous: more than one of the select's output columns carries \
         that name, and PostgreSQL refuses the clause. Name one of the columns explicitly, \
         or qualify the input column.",
        if group_by { "GROUP BY" } else { "ORDER BY" }
    ))
}

/// The projection item the item's ordinal position names.
fn ordinal_projection_item<'a>(item: &Expr, projection: &'a [SelectItem]) -> Option<&'a Expr> {
    let Expr::Value(ValueWithSpan { value: Value::Number(digits, ..), .. }) = item else {
        return None;
    };
    let position = digits.parse::<usize>().ok()?;
    projection.get(position.checked_sub(1)?).and_then(projection_item_expr)
}

/// The output column name a projection item answers: the alias it carries,
/// first of the spellings it carries, or the name its column reference
/// spells, each folded the way PostgreSQL folds the identifier.
fn projection_item_name(item: &SelectItem) -> Option<Cow<'_, str>> {
    match item {
        SelectItem::ExprWithAlias { alias, .. } => Some(folded_name(alias)),
        SelectItem::UnnamedExpr(expr) => {
            match expr {
                Expr::Identifier(ident) => Some(folded_name(ident)),
                Expr::CompoundIdentifier(parts) => parts.last().map(folded_name),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The `GROUP BY` rewrite of a forward `SELECT DISTINCT` over named
/// interval items, or the refusal the query answers. `None` when the query
/// keeps its `DISTINCT`.
///
/// # Errors
///
/// Propagates the refusal the query answers, and the refusal an inline key
/// over a non-replayable operand answers.
pub(crate) fn distinct_select(
    source: &Select,
    translated: &Select,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Select>, Error> {
    let Some(Distinct::Distinct) = translated.distinct.as_ref() else {
        return Ok(None);
    };
    let mut translated_exprs = Vec::with_capacity(translated.projection.len());
    for translated_item in &translated.projection {
        match translated_item {
            SelectItem::UnnamedExpr(expr)
            | SelectItem::ExprWithAlias { expr, .. }
            | SelectItem::ExprWithAliases { expr, .. } => translated_exprs.push(expr),
            wildcard @ (SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _)) => {
                let query = crate::impls::query_builder::make_query(
                    None,
                    SetExpr::Select(Box::new(source.clone())),
                );
                let mut interval_output = query_output_carries(
                    &query,
                    schema,
                    options,
                    |column| is_interval_data_type(&column.data_type).then_some(()),
                    |expr, schema, o| Ok(is_interval_typed(expr, schema, o).then_some(())),
                )?;
                // A derived relation's outputs do not always expand under a
                // wildcard, VALUES and set operations among them, so the
                // check settles each derived body itself.
                if !interval_output {
                    let derived = source
                        .from
                        .iter()
                        .flat_map(|entry| {
                            core::iter::once(&entry.relation)
                                .chain(entry.joins.iter().map(|join| &join.relation))
                        })
                        .filter_map(|relation| {
                            match relation {
                                TableFactor::Derived { subquery, .. } => Some(subquery),
                                _ => None,
                            }
                        });
                    for subquery in derived {
                        if set_branch_output_carries_interval(&subquery.body, schema, options)? {
                            interval_output = true;
                            break;
                        }
                    }
                }
                if interval_output {
                    return Err(Error::forward_refusal(format!(
                        "SELECT DISTINCT {wildcard} over a relation with an interval column \
                         cannot be translated: the distinct would run on the stored text, \
                         and the key that settles interval equality is a scalar subquery \
                         SQLite does not allow in a wildcard projection. Name the interval \
                         columns."
                    )));
                }
                return Ok(None);
            }
        }
    }
    let interval_items: Vec<bool> = source
        .projection
        .iter()
        .map(|item| {
            projection_item_expr(item).is_some_and(|expr| is_interval_typed(expr, schema, options))
        })
        .collect();
    if interval_items.iter().any(|typed| *typed) {
        if groups_rows(&source.group_by) {
            return Err(Error::forward_refusal(
                "SELECT DISTINCT with an interval item alongside a GROUP BY clause cannot be \
                 translated: the distinct would run on the stored text, and the member a \
                 group answers is a plan choice the key does not settle. Drop the DISTINCT, \
                 or run the distinct in the application."
                    .to_string(),
            ));
        }
        if source.projection.iter().any(|item| {
            projection_item_expr(item).is_some_and(|expr| {
                crate::impls::translator_impls::query::is_aggregate_or_window_expression(expr)
            })
        }) {
            return Err(Error::forward_refusal(
                "SELECT DISTINCT with an interval item alongside an aggregate or a window \
                 function cannot be translated: the distinct would run on the stored text, \
                 and the aggregate's or the window's row grouping is not the distinct's. \
                 Drop the DISTINCT, or run the distinct in the application."
                    .to_string(),
            ));
        }
        let mut group_by = Vec::with_capacity(translated.projection.len());
        for (position, translated_expr) in translated_exprs.iter().copied().enumerate() {
            if interval_items[position] {
                group_by.push(key_expr(translated_expr, options)?);
            } else {
                group_by.push(translated_expr.clone());
            }
        }
        let mut rewritten = translated.clone();
        rewritten.distinct = None;
        rewritten.group_by = GroupByExpr::Expressions(group_by, Vec::new());
        return Ok(Some(rewritten));
    }
    Ok(None)
}

/// The expression a projection item carries, `None` for a wildcard.
fn projection_item_expr(item: &SelectItem) -> Option<&Expr> {
    match item {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => Some(expr),
        _ => None,
    }
}

/// Whether `group_by` names a grouping, in any of its spellings.
fn groups_rows(group_by: &GroupByExpr) -> bool {
    match group_by {
        GroupByExpr::Expressions(exprs, _) => !exprs.is_empty(),
        GroupByExpr::All(_) => true,
    }
}

/// The NULL-ignoring extremum over `arguments`, one `coalesce` rotation per
/// argument, so a slot is NULL only when every argument is.
fn null_ignoring_extremum(arguments: &[Expr], greatest: bool) -> Expr {
    simple_function_expr(
        if greatest { "MAX" } else { "MIN" },
        (0..arguments.len())
            .map(|start| {
                let rotated =
                    arguments.iter().cycle().skip(start).take(arguments.len()).cloned().collect();
                simple_function_expr("coalesce", rotated, None)
            })
            .collect(),
        None,
    )
}

/// The keyed text of one extremum argument: the comparison key with the
/// argument's text kept behind it.
///
/// # Errors
///
/// Propagates the refusal the argument answers, and the refusal a non-
/// replayable argument answers.
fn extremum_argument_key(
    arg: &Expr,
    construct: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    match interval_side(arg, schema, options)? {
        IntervalSide::BareLiteral(text) => literal_keyed_text(&text, None),
        IntervalSide::TypedLiteral { text, typmod } => literal_keyed_text(&text, typmod),
        IntervalSide::Null => {
            Ok(Expr::Value(ValueWithSpan { value: Value::Null, span: Span::empty() }))
        }
        IntervalSide::Other => {
            let other_type = message_type_name_of(arg, schema, options);
            Err(Error::forward_refusal(format!(
                "{} types interval and {other_type} cannot be matched",
                construct.to_ascii_uppercase()
            )))
        }
        IntervalSide::Keyed(expr) => {
            let translated = expr.translate_with_warnings(schema, options, emit)?;
            if !is_replayable(&translated, options) {
                return Err(reject_duplicated_operand(construct, arg));
            }
            keyed_text_expr(&translated, options)
        }
    }
}

/// The interval translation of a call, or `None` when the call is not over
/// an interval this position owns.
///
/// # Errors
///
/// Propagates the refusal the call answers.
pub(crate) fn function(
    func: &Function,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Expr>, Error> {
    let name =
        last_ident(&func.name).map(|ident| ident.value.to_ascii_lowercase()).unwrap_or_default();
    let args = function_argument_exprs(&func.args);
    match name.as_str() {
        "count" => count(func, &args, schema, options, emit),
        "min" | "max" => extremum(name.as_str(), func, &args, schema, options, emit),
        "sum" | "avg" => {
            if any_interval_typed(&args, schema, options) {
                Err(Error::forward_refusal(format!(
                    "{name} over an interval cannot be translated: the answer is an \
                     interval, and SQLite has no interval type to hold one. Aggregate in the \
                     application."
                )))
            } else {
                Ok(None)
            }
        }
        "date_part" => {
            if args.get(1).is_some_and(|arg| is_interval_typed(arg, schema, options)) {
                Err(Error::forward_refusal(
                    "date_part over an interval cannot be translated: reading a field out of an \
                     interval is a value read the replica does not translate. Read the stored \
                     text in the application."
                        .to_string(),
                ))
            } else {
                Ok(None)
            }
        }
        "to_char" => {
            if args.first().is_some_and(|arg| is_interval_typed(arg, schema, options)) {
                Err(Error::forward_refusal(
                    "to_char over an interval cannot be translated: rendering an interval from \
                     its fields is a value read the replica does not translate. Read the \
                     stored text in the application."
                        .to_string(),
                ))
            } else {
                Ok(None)
            }
        }
        "abs" => {
            if any_interval_typed(&args, schema, options) {
                return Err(Error::forward_refusal(
                    "function abs(interval) does not exist. SQLite's abs has no interval form, \
                     and the replica refuses rather than run it on the stored text."
                        .to_string(),
                ));
            }
            Ok(None)
        }
        "greatest" | "least" => greatest_least(name.as_str(), func, &args, schema, options, emit),
        _ => Ok(None),
    }
}

/// Whether any of `args` carries the `interval` type, `false` when nothing
/// settles the type of an argument.
fn any_interval_typed(args: &[&Expr], schema: &ParserDB, options: &TranslationContext<'_>) -> bool {
    args.iter().any(|arg| is_interval_typed(arg, schema, options))
}

/// The interval translation of a `count` call.
fn count(
    func: &Function,
    args: &[&Expr],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Expr>, Error> {
    let FunctionArguments::List(list) = &func.args else {
        return Ok(None);
    };
    if func.over.is_some() {
        // A window count agrees on text and value: it counts non-NULL rows.
        return Ok(None);
    }
    if list.duplicate_treatment != Some(DuplicateTreatment::Distinct) {
        return Ok(None);
    }
    if !any_interval_typed(args, schema, options) {
        return Ok(None);
    }
    if func.filter.is_some() || !func.within_group.is_empty() || func.null_treatment.is_some() {
        return Err(Error::forward_refusal(
            "count(DISTINCT) over an interval with a FILTER or WITHIN GROUP clause cannot be \
             translated: the distinct count runs on the comparison key, and this clause does \
             not carry through it. Count in the application."
                .to_string(),
        ));
    }
    if args.len() > 1 {
        let types = args
            .iter()
            .map(|arg| message_type_name_of(arg, schema, options))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::forward_refusal(format!(
            "function count({types}) does not exist. PostgreSQL has no count over a pair of \
             values, so this is not a statement the server runs."
        )));
    }
    let arg = args[0];
    let translated = arg.translate_with_warnings(schema, options, emit)?;
    let key = key_expr(&translated, options)?;
    Ok(Some(Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("count"))]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: Some(DuplicateTreatment::Distinct),
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(key))],
            clauses: Vec::new(),
        }),
        filter: None,
        null_treatment: None,
        within_group: Vec::new(),
        over: None,
    })))
}

/// The interval translation of a `min` or `max` call.
fn extremum(
    name: &str,
    func: &Function,
    args: &[&Expr],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Expr>, Error> {
    if args.len() > 1 {
        if any_interval_typed(args, schema, options) {
            return Err(Error::forward_refusal(format!(
                "function {name}(interval, …) does not exist. PostgreSQL has no two-argument \
                 {name} over intervals, so this is not a statement the server runs."
            )));
        }
        return Ok(None);
    }
    let Some(arg) = args.first() else {
        return Ok(None);
    };
    if !is_interval_typed(arg, schema, options) {
        return Ok(None);
    }
    if func.over.is_some() {
        return Err(Error::forward_refusal(format!(
            "{name} over an interval with an OVER clause cannot be translated: the key that \
             orders the rows is a scalar subquery, and SQLite does not allow one inside a \
             window function."
        )));
    }
    let keyed_text = extremum_argument_key(arg, name, schema, options, emit)?;
    let aggregate = simple_function_expr(name, vec![keyed_text], None);
    Ok(Some(simple_function_expr("substr", vec![aggregate, integer_literal(24)], None)))
}

/// The interval translation of a `greatest` or `least` call. A call whose
/// arguments never settle the `interval` type is text, and a bare literal
/// beside a settled argument keeps its key, since PostgreSQL types it as an
/// interval.
///
/// # Errors
///
/// Propagates the refusal the call answers.
fn greatest_least(
    name: &str,
    func: &Function,
    args: &[&Expr],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Expr>, Error> {
    if args.len() < 2 {
        // A single argument is its own extremum, the value itself.
        return Ok(None);
    }
    let any_interval = {
        let mut found = false;
        for arg in args {
            if matches!(
                interval_side(arg, schema, options)?,
                IntervalSide::TypedLiteral { .. } | IntervalSide::Keyed(_)
            ) {
                found = true;
                break;
            }
        }
        found
    };
    if !any_interval {
        return Ok(None);
    }
    if func.over.is_some() {
        return Err(Error::forward_refusal(format!(
            "{name} over an interval with an OVER clause cannot be translated: the key that \
             orders the values is a scalar subquery, and SQLite does not allow one inside a \
             window function."
        )));
    }
    if !options.allows_subqueries() {
        return Err(Error::forward_refusal(format!(
            "{name} over an interval reads the comparison key, which is a scalar subquery \
             SQLite does not allow in this position. Move the call to a query body."
        )));
    }
    let greatest = name == "greatest";
    let width = (args.len() - 1).to_string().len();
    let mut ranked = Vec::new();
    for (position, arg) in args.iter().enumerate() {
        let keyed_text = extremum_argument_key(arg, name, schema, options, emit)?;
        let rank = if greatest { args.len() - 1 - position } else { position };
        ranked.push(with_rank(keyed_text, &format!("{rank:0width$}")));
    }
    let extremum = null_ignoring_extremum(&ranked, greatest);
    let text_start = i64::try_from(24 + width).unwrap_or(i64::MAX);
    Ok(Some(simple_function_expr("substr", vec![extremum, integer_literal(text_start)], None)))
}

/// `keyed_text` with `rank` between the 23-character key and the text, so
/// equal keys order by argument position. PostgreSQL's `GREATEST` and `LEAST`
/// replace the running answer only on a strict win, keeping the earlier
/// argument among equal values.
fn with_rank(keyed_text: Expr, rank: &str) -> Expr {
    let key = simple_function_expr(
        "substr",
        vec![keyed_text.clone(), integer_literal(1), integer_literal(23)],
        None,
    );
    let text = simple_function_expr("substr", vec![keyed_text, integer_literal(24)], None);
    Expr::BinaryOp {
        left: Box::new(Expr::BinaryOp {
            left: Box::new(key),
            op: BinaryOperator::StringConcat,
            right: Box::new(string_literal(rank)),
        }),
        op: BinaryOperator::StringConcat,
        right: Box::new(text),
    }
}

#[cfg(test)]
mod tests {
    use core::ops::ControlFlow;

    use sql_traits::prelude::ParserDB;
    use sqlparser::{
        ast::{Expr, Ident, SelectItem, Visit, Visitor},
        dialect::{PostgreSqlDialect, SQLiteDialect},
        parser::Parser,
    };

    use super::{keys_templates, select_list_item, substitute};

    fn parse_expr(sql: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {}).try_with_sql(sql).unwrap().parse_expr().unwrap()
    }

    #[test]
    fn a_malformed_key_template_answers_the_parse_refusal() {
        let error = substitute(&parse_expr("iv"), "(").unwrap_err();
        assert!(
            error.to_string().contains("fails to parse as an expression"),
            "refusal {error} should name the parse failure"
        );
    }

    /// The value of the first quoted identifier of `expr`, in visit order.
    fn first_quoted_identifier(expr: &Expr) -> Option<String> {
        struct Finder(Option<String>);

        impl Visitor for Finder {
            type Break = ();

            fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
                if let Expr::Identifier(ident) = expr
                    && ident.quote_style.is_some()
                {
                    self.0 = Some(ident.value.clone());
                    return ControlFlow::Break(());
                }
                ControlFlow::Continue(())
            }
        }

        let mut finder = Finder(None);
        let _ = expr.visit(&mut finder);
        finder.0
    }

    #[test]
    fn the_key_templates_parse_and_substitute_their_placeholder() {
        assert_eq!(first_quoted_identifier(&parse_expr("\"x\"")), Some(String::from("x")));
        for template in [
            keys_templates::BOUND_KEY,
            keys_templates::INLINE_KEY,
            keys_templates::BOUND_KEYED_TEXT,
        ] {
            let parsed = Parser::new(&SQLiteDialect {})
                .try_with_sql(template)
                .unwrap()
                .parse_expr()
                .unwrap();
            assert!(
                first_quoted_identifier(&parsed).is_none(),
                "the template {template} carries only unquoted identifiers"
            );
            let substituted = substitute(&parse_expr("iv"), template).unwrap();
            assert!(
                !substituted.to_string().contains("pg2sqlite_interval_operand"),
                "the substitution replaces every placeholder: {substituted}"
            );
        }
    }

    #[test]
    fn select_list_item_skips_the_group_by_column_check_for_a_variable() {
        let schema =
            ParserDB::parse::<PostgreSqlDialect>("CREATE TABLE t (x int, iv interval)").unwrap();
        let options = crate::options::TranslationContext::from_owned(
            crate::prelude::Pg2SqliteOptions::default(),
        );
        let query = Parser::new(&PostgreSqlDialect {})
            .try_with_sql("SELECT x, iv FROM t")
            .unwrap()
            .parse_query()
            .unwrap();
        let scope = sql_traits::structs::ColumnScope::from_query(
            &query,
            &schema,
            sql_traits::structs::IdentifierCase::AsWritten,
        )
        .unwrap();
        let scoped = options.with_scope(&scope);
        let projection = vec![
            SelectItem::UnnamedExpr(Expr::Identifier(Ident::new("x"))),
            SelectItem::UnnamedExpr(Expr::Identifier(Ident::new("iv"))),
        ];
        let item = Expr::Identifier(Ident::new("x"));
        let plain = select_list_item(&item, &projection, true, &schema, &scoped)
            .expect("a group by name settles");
        assert!(plain.is_none(), "a declared column keeps its ordinary translation under GROUP BY");
        let variables = vec![String::from("x")];
        let scoped = scoped.with_variables(&variables);
        let keyed = select_list_item(&item, &projection, true, &schema, &scoped)
            .expect("a group by name settles");
        assert!(keyed.is_some(), "a variable name skips the column check and reads its output");
    }
}
