//! Values of a `CREATE TYPE ... AS ENUM` type.
//!
//! A column of an enum type holds its label as `TEXT`, so a read returns the
//! label and an equality, a key, a unique index or a foreign key compares
//! labels. PostgreSQL orders an enum by the position each label is declared
//! at, so every ordering reads the label's position instead, as the key
//! `CASE operand WHEN 'first' THEN 0 WHEN 'second' THEN 1 … END`. The rewrites
//! here answer PostgreSQL expressions over that key, which the translator then
//! translates like any other. A key holds no subquery, so it is legal in a
//! `CHECK`, an index expression, a partial-index predicate and a generated
//! column alike, and a simple `CASE` reads its operand once.

use sql_traits::{
    prelude::{
        ColumnLike, DataTypeLike, DatabaseLike, EnumerationKind, EnumerationMember, TableLike,
    },
    structs::ParserDB,
};
use sqlparser::ast::{
    AlterType, AlterTypeOperation, BinaryOperator, CaseWhen, DataType, Expr, FunctionArg,
    FunctionArgExpr, FunctionArguments, GroupByExpr, Ident, InTarget, ObjectName, OrderBy,
    OrderByKind, Query, SelectItem, SetExpr, Statement, TableAlias, TableFactor,
    UserDefinedTypeRepresentation, Value, ValueWithSpan, WildcardAdditionalOptions,
    helpers::attached_token::AttachedToken,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        expression_type::{UNKNOWN, parse_type_name, type_name},
        function_helpers::{integer_literal, string_literal, string_literal_text},
        object_name::last_ident,
        query_builder::{from_relation, make_query, make_simple_select},
        shared_helpers::strip_nesting,
    },
    options::TranslationContext,
};

/// The labels of the enum type `data_type` names, in declaration order, or
/// `None` for any other type.
pub(crate) fn labels<'a>(
    data_type: &'a DataType,
    schema: &'a ParserDB,
) -> Option<impl Iterator<Item = &'a str>> {
    if !matches!(data_type, DataType::Custom(..)) {
        return None;
    }
    let enumeration = data_type.enumeration(schema)?;
    (enumeration.kind() == EnumerationKind::Enum)
        .then(|| enumeration.into_members().map(EnumerationMember::label))
}

/// Whether `data_type` names an enum type.
pub(crate) fn is_enum(data_type: &DataType, schema: &ParserDB) -> bool {
    labels(data_type, schema).is_some()
}

/// The `CHECK` a column of an enum type needs, holding it to the labels, or
/// `None` for a column of any other type.
pub(crate) fn label_check(
    column: &sqlparser::ast::Ident,
    data_type: &DataType,
    schema: &ParserDB,
) -> Option<Expr> {
    Some(Expr::In {
        expr: Box::new(Expr::Identifier(column.clone())),
        target: InTarget::List(labels(data_type, schema)?.map(string_literal).collect()),
        negated: false,
        global: false,
    })
}

/// Whether the schema declares any enum type, so a statement over a schema
/// that declares none never reads an expression's type for one.
fn declares_enum(schema: &ParserDB) -> bool {
    schema.user_defined_types().any(|declared| {
        matches!(declared.representation(), Some(UserDefinedTypeRepresentation::Enum { .. }))
    })
}

/// The enum type `expr` settles on, or `None` when it settles on another type
/// or on none the statement writes.
fn enum_type_of(
    expr: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Option<DataType> {
    let name = type_name(expr, schema, options).ok().flatten()?;
    parse_type_name(&name).filter(|data_type| is_enum(data_type, schema))
}

/// Whether two spellings bind one enum type. Two bindings of one type read
/// the same stored labels, while two types declaring the same labels store
/// their own.
fn same_type(left: &DataType, right: &DataType, schema: &ParserDB) -> bool {
    left == right
        || labels(left, schema)
            .and_then(|mut labels| labels.next())
            .zip(labels(right, schema).and_then(|mut labels| labels.next()))
            .is_some_and(|(left, right)| core::ptr::eq(left, right))
}

/// The text `expr` holds when it is a string literal.
fn label_literal(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Value(ValueWithSpan { value, .. }) => string_literal_text(value),
        _ => None,
    }
}

/// The position `label` is declared at in `data_type`, or PostgreSQL's
/// refusal of a value that is no label of it.
fn position(label: &str, data_type: &DataType, schema: &ParserDB) -> Result<usize, Error> {
    labels(data_type, schema)
        .and_then(|mut declared| declared.position(|candidate| candidate == label))
        .ok_or_else(|| {
            Error::forward_refusal(format!(
                "invalid input value for enum {data_type}: \"{label}\". PostgreSQL rejects a \
                 value that is no label of the type."
            ))
        })
}

/// What one side of an ordering beside an enum operand reads.
#[derive(Clone, Copy)]
enum Side<'e> {
    /// A label, at its declared position.
    Position(usize),
    /// `NULL`, whose key is `NULL`.
    Null,
    /// A value of the type, keyed when the statement runs.
    Keyed(&'e Expr),
}

/// The side `expr` answers beside an operand of `data_type`, or the refusal
/// PostgreSQL gives a side of another type.
fn side<'e>(
    expr: &'e Expr,
    data_type: &DataType,
    operator: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Side<'e>, Error> {
    let expr = strip_nesting(expr);
    if let Some(label) = label_literal(expr) {
        return position(label, data_type, schema).map(Side::Position);
    }
    match expr {
        Expr::Value(ValueWithSpan { value: Value::Null, .. }) => return Ok(Side::Null),
        Expr::Value(ValueWithSpan { value: Value::Placeholder(_), .. }) => {
            return Ok(Side::Keyed(expr));
        }
        Expr::Cast { expr: inner, data_type: target, .. } if is_enum(target, schema) => {
            if !same_type(target, data_type, schema) {
                return Err(operator_refusal(data_type, operator, &target.to_string()));
            }
            let inner = strip_nesting(inner);
            if let Some(label) = label_literal(inner) {
                return position(label, data_type, schema).map(Side::Position);
            }
            if matches!(inner, Expr::Value(ValueWithSpan { value: Value::Null, .. })) {
                return Ok(Side::Null);
            }
            // A value that is no label keys to NULL, as the cast reads it.
            return Ok(Side::Keyed(inner));
        }
        _ => {}
    }
    let Some(name) = type_name(expr, schema, options).ok().flatten() else {
        return Ok(Side::Keyed(expr));
    };
    if name == UNKNOWN {
        return Ok(Side::Keyed(expr));
    }
    match parse_type_name(&name) {
        Some(other) if is_enum(&other, schema) && same_type(&other, data_type, schema) => {
            Ok(Side::Keyed(expr))
        }
        _ => Err(operator_refusal(data_type, operator, &name)),
    }
}

/// PostgreSQL's refusal of an operator between an enum and another type.
fn operator_refusal(data_type: &DataType, operator: &str, other: &str) -> Error {
    Error::forward_refusal(format!(
        "operator does not exist: {data_type} {operator} {other}. PostgreSQL compares an enum \
         only with a value of the same type."
    ))
}

/// `CASE operand WHEN <from> THEN <to> … END` over every label of
/// `data_type`, with no `ELSE`, so a value that is no label reads `NULL`.
fn label_case(
    operand: Expr,
    data_type: &DataType,
    schema: &ParserDB,
    to: impl Fn(usize, &str) -> Expr,
    from: impl Fn(usize, &str) -> Expr,
) -> Expr {
    Expr::Case {
        case_token: AttachedToken::empty(),
        end_token: AttachedToken::empty(),
        operand: Some(Box::new(operand)),
        conditions: labels(data_type, schema)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(position, label)| {
                CaseWhen { condition: from(position, label), result: to(position, label) }
            })
            .collect(),
        else_result: None,
    }
}

/// An `i64` literal for a label position.
fn position_literal(position: usize) -> Expr {
    integer_literal(i64::try_from(position).unwrap_or(i64::MAX))
}

/// The ordering key of a value of `data_type`, which is its label's position.
fn key(operand: Expr, data_type: &DataType, schema: &ParserDB) -> Expr {
    label_case(
        operand,
        data_type,
        schema,
        |position, _| position_literal(position),
        |_, label| string_literal(label),
    )
}

/// The label a key of `data_type` reads.
fn label_of_key(key: Expr, data_type: &DataType, schema: &ParserDB) -> Expr {
    label_case(
        key,
        data_type,
        schema,
        |_, label| string_literal(label),
        |position, _| position_literal(position),
    )
}

/// The key a side reads.
fn side_key(side: Side<'_>, data_type: &DataType, schema: &ParserDB) -> Expr {
    match side {
        Side::Position(position) => position_literal(position),
        Side::Null => Expr::Value(Value::Null.into()),
        Side::Keyed(expr) => key(expr.clone(), data_type, schema),
    }
}

/// The comparison operators, with the spelling PostgreSQL's refusal names.
fn comparison(op: &BinaryOperator) -> Option<(&'static str, bool)> {
    Some(match op {
        BinaryOperator::Eq => ("=", false),
        BinaryOperator::NotEq => ("<>", false),
        BinaryOperator::Lt => ("<", true),
        BinaryOperator::LtEq => ("<=", true),
        BinaryOperator::Gt => (">", true),
        BinaryOperator::GtEq => (">=", true),
        _ => return None,
    })
}

/// The PostgreSQL expression `expr` reads as once its enum values are keyed,
/// or `None` when `expr` keeps its ordinary translation. A comparison,
/// `BETWEEN` and `IN` list also refuse a label the type does not declare and a
/// value of another type, as PostgreSQL does.
///
/// # Errors
///
/// Returns the refusal PostgreSQL gives the expression.
pub(crate) fn rewrite(
    expr: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Expr>, Error> {
    match expr {
        Expr::BinaryOp { left, op, right } if declares_enum(schema) => {
            let Some((operator, ordered)) = comparison(op) else {
                return Ok(None);
            };
            if ordered {
                tuple_refusal(left, right, schema, options)?;
            }
            let Some(data_type) = enum_type_of(left, schema, options)
                .or_else(|| enum_type_of(right, schema, options))
            else {
                return Ok(None);
            };
            let left = side(left, &data_type, operator, schema, options)?;
            let right = side(right, &data_type, operator, schema, options)?;
            Ok(ordered.then(|| {
                Expr::BinaryOp {
                    left: Box::new(side_key(left, &data_type, schema)),
                    op: op.clone(),
                    right: Box::new(side_key(right, &data_type, schema)),
                }
            }))
        }
        Expr::Between { expr: operand, negated, low, high } if declares_enum(schema) => {
            let Some(data_type) = [operand, low, high]
                .into_iter()
                .find_map(|side| enum_type_of(side, schema, options))
            else {
                return Ok(None);
            };
            let key = |side_expr: &Expr| {
                side(side_expr, &data_type, "BETWEEN", schema, options)
                    .map(|read| Box::new(side_key(read, &data_type, schema)))
            };
            Ok(Some(Expr::Between {
                expr: key(operand)?,
                negated: *negated,
                low: key(low)?,
                high: key(high)?,
            }))
        }
        Expr::In { expr: operand, target: InTarget::List(list), .. } if declares_enum(schema) => {
            let Some(data_type) = enum_type_of(operand, schema, options) else {
                return Ok(None);
            };
            for item in list {
                side(item, &data_type, "=", schema, options)?;
            }
            Ok(None)
        }
        Expr::AnyOp { left, compare_op, .. } | Expr::AllOp { left, compare_op, .. }
            if declares_enum(schema) =>
        {
            match comparison(compare_op) {
                Some((operator, true)) if enum_type_of(left, schema, options).is_some() => {
                    Err(Error::forward_refusal(format!(
                        "{operator} ANY or ALL over an enum cannot be translated: the array's \
                         elements are compared as stored text, which does not follow the \
                         declaration order. Compare with each label instead."
                    )))
                }
                _ => Ok(None),
            }
        }
        Expr::Cast { expr: inner, data_type, .. } if declares_enum(schema) => {
            cast(inner, data_type, schema, options)
        }
        Expr::Function(call) if declares_enum(schema) => function(call, schema, options),
        _ => Ok(None),
    }
}

/// Refuses an ordering between row values holding an enum, which SQLite would
/// compare element by element on the stored text.
fn tuple_refusal(
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    let holds_enum = |side: &Expr| {
        matches!(strip_nesting(side), Expr::Tuple(elements)
            if elements.iter().any(|element| enum_type_of(element, schema, options).is_some()))
    };
    if holds_enum(left) || holds_enum(right) {
        return Err(Error::forward_refusal(
            "an ordering between row values holding an enum cannot be translated: SQLite \
             compares the row's elements as stored text, which does not follow the declaration \
             order. Compare the elements one by one."
                .to_string(),
        ));
    }
    Ok(())
}

/// The value a cast to an enum reads: the label a literal names, `NULL`, the
/// operand itself when it already has the type, and otherwise the operand
/// where it is a label and `NULL` where it is not.
fn cast(
    inner: &Expr,
    data_type: &DataType,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Expr>, Error> {
    if !is_enum(data_type, schema) {
        return Ok(None);
    }
    let inner = strip_nesting(inner);
    if let Some(label) = label_literal(inner) {
        position(label, data_type, schema)?;
        return Ok(Some(string_literal(label)));
    }
    if matches!(inner, Expr::Value(ValueWithSpan { value: Value::Null, .. })) {
        return Ok(Some(inner.clone()));
    }
    if let Some(source) = enum_type_of(inner, schema, options) {
        if same_type(&source, data_type, schema) {
            return Ok(Some(inner.clone()));
        }
        return Err(Error::forward_refusal(format!(
            "cannot cast type {source} to {data_type}. PostgreSQL has no cast between two enum \
             types."
        )));
    }
    Ok(Some(label_case(
        inner.clone(),
        data_type,
        schema,
        |_, label| string_literal(label),
        |_, label| string_literal(label),
    )))
}

/// The keyed form of `min`, `max`, `greatest` and `least` over an enum, and
/// the labels `enum_range` answers.
fn function(
    call: &sqlparser::ast::Function,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Expr>, Error> {
    let Some(name) = last_ident(&call.name).map(|ident| ident.value.to_ascii_lowercase()) else {
        return Ok(None);
    };
    let FunctionArguments::List(list) = &call.args else {
        return Ok(None);
    };
    let arguments: Vec<&Expr> = list
        .args
        .iter()
        .filter_map(|argument| {
            match argument {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
                _ => None,
            }
        })
        .collect();
    if arguments.len() != list.args.len() {
        return Ok(None);
    }
    match (name.as_str(), arguments.as_slice()) {
        ("min" | "max", [argument]) => {
            let Some(data_type) = enum_type_of(argument, schema, options) else {
                return Ok(None);
            };
            let keyed = keyed_call(call, list, |_| key((*argument).clone(), &data_type, schema));
            Ok(Some(label_of_key(keyed, &data_type, schema)))
        }
        ("greatest" | "least", [_, _, ..]) => {
            let Some(data_type) =
                arguments.iter().find_map(|argument| enum_type_of(argument, schema, options))
            else {
                return Ok(None);
            };
            let mut keys = Vec::with_capacity(arguments.len());
            for argument in &arguments {
                keys.push(side_key(
                    side(argument, &data_type, &name, schema, options)?,
                    &data_type,
                    schema,
                ));
            }
            let mut keys = keys.into_iter();
            let keyed =
                keyed_call(call, list, |_| keys.next().unwrap_or(Expr::Value(Value::Null.into())));
            Ok(Some(label_of_key(keyed, &data_type, schema)))
        }
        ("enum_range", [argument]) => {
            let Some(data_type) = enum_type_of(argument, schema, options) else {
                return Ok(None);
            };
            Ok(Some(Expr::Array(sqlparser::ast::Array {
                elem: labels(&data_type, schema)
                    .into_iter()
                    .flatten()
                    .map(|label| {
                        Expr::Cast {
                            kind: sqlparser::ast::CastKind::DoubleColon,
                            expr: Box::new(string_literal(label)),
                            data_type: data_type.clone(),
                            format: None,
                        }
                    })
                    .collect(),
                named: true,
            })))
        }
        ("enum_range", [lower, _]) if enum_type_of(lower, schema, options).is_some() => {
            Err(Error::forward_refusal(
                "enum_range with two bounds cannot be translated. Write the labels between them \
                 as an ARRAY literal."
                    .to_string(),
            ))
        }
        _ => Ok(None),
    }
}

/// `call` with each argument replaced by what `argument` answers for its
/// position, keeping its `OVER`, `FILTER`, `DISTINCT` and ordering.
fn keyed_call(
    call: &sqlparser::ast::Function,
    list: &sqlparser::ast::FunctionArgumentList,
    mut argument: impl FnMut(usize) -> Expr,
) -> Expr {
    Expr::Function(sqlparser::ast::Function {
        name: call.name.clone(),
        uses_odbc_syntax: call.uses_odbc_syntax,
        parameters: call.parameters.clone(),
        args: FunctionArguments::List(sqlparser::ast::FunctionArgumentList {
            duplicate_treatment: list.duplicate_treatment,
            args: (0..list.args.len())
                .map(|position| FunctionArg::Unnamed(FunctionArgExpr::Expr(argument(position))))
                .collect(),
            clauses: list.clauses.clone(),
        }),
        filter: call.filter.clone(),
        null_treatment: call.null_treatment,
        over: call.over.clone(),
        within_group: call.within_group.clone(),
    })
}

/// The ordering key of an `ORDER BY` or `PARTITION BY` item of an enum type,
/// or `None` for an item of any other type.
pub(crate) fn order_key(
    item: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Option<Expr> {
    if !declares_enum(schema) {
        return None;
    }
    let data_type = enum_type_of(item, schema, options)?;
    Some(key(item.clone(), &data_type, schema))
}

/// Whether SQLite applies `query`'s `ORDER BY` to a compound select, as it
/// does after a set operation and after the `UNION ALL` the grouping-set
/// rewrite builds, where only result columns can be sort keys.
fn sorts_a_compound(body: &SetExpr) -> bool {
    match body {
        SetExpr::SetOperation { .. } => true,
        SetExpr::Select(select) => {
            matches!(&select.group_by, GroupByExpr::Expressions(keys, _)
            if keys.iter().any(|key| {
                matches!(key, Expr::Rollup(_) | Expr::Cube(_) | Expr::GroupingSets(_))
            }))
        }
        _ => false,
    }
}

/// The position of the output an `ORDER BY` item names, by its position or by
/// the name PostgreSQL stores, or `None` for an item naming no output.
fn named_output(item: &Expr, outputs: &[(Option<String>, Option<DataType>)]) -> Option<usize> {
    match item {
        Expr::Value(ValueWithSpan { value: Value::Number(number, _), .. }) => {
            number
                .parse::<usize>()
                .ok()?
                .checked_sub(1)
                .filter(|position| *position < outputs.len())
        }
        Expr::Identifier(ident) => {
            let folded = if ident.quote_style.is_some() {
                ident.value.clone()
            } else {
                ident.value.to_ascii_lowercase()
            };
            outputs.iter().position(|(name, _)| name.as_deref() == Some(folded.as_str()))
        }
        _ => None,
    }
}

/// `query` ordered outside the compound select it sorts when an `ORDER BY`
/// item is an enum, so the key reads a column of the derived table, or `None`
/// when no item needs a key.
///
/// # Errors
///
/// Refuses an enum item that no output name can carry, the outputs of a `*`
/// that cannot be enumerated, and the refusal a scope that cannot be built
/// answers.
pub(crate) fn ordered_compound(
    query: &Query,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Query>, Error> {
    let Some(OrderBy { kind: OrderByKind::Expressions(items), interpolate }) = &query.order_by
    else {
        return Ok(None);
    };
    if !sorts_a_compound(&query.body) || !declares_enum(schema) {
        return Ok(None);
    }
    let Some(outputs) = crate::impls::shared_helpers::query_outputs(
        query,
        schema,
        options,
        |column| is_enum(&column.data_type, schema).then(|| column.data_type.clone()),
        |expr, schema, options| Ok(enum_type_of(expr, schema, options)),
    )?
    else {
        return Err(Error::forward_refusal(
            "ORDER BY over a compound select whose columns a * hides cannot be translated, since \
             SQLite sorts it by result columns and an enum among them needs its declaration \
             order. List the columns."
                .to_string(),
        ));
    };
    let mut keyed = false;
    let mut unnamed = false;
    let mut ordered = Vec::with_capacity(items.len());
    for item in items {
        let mut outer = item.clone();
        match named_output(&item.expr, &outputs).map(|position| &outputs[position]) {
            Some((name, Some(data_type))) => {
                keyed = true;
                let Some(name) = name else {
                    return Err(Error::forward_refusal(format!(
                        "ORDER BY {} names an enum column the compound select leaves unnamed, \
                         and SQLite sorts it by declaration order only through the column's \
                         name. Give the column an alias.",
                        item.expr
                    )));
                };
                outer.expr =
                    key(Expr::Identifier(Ident::with_quote('"', name.as_str())), data_type, schema);
            }
            Some(_) => {}
            None => unnamed = true,
        }
        ordered.push(outer);
    }
    if !keyed {
        return Ok(None);
    }
    if unnamed {
        return Err(Error::forward_refusal(
            "ORDER BY over a grouping set that orders an enum column and an expression no \
             output names cannot be translated, since SQLite sorts the rewrite by result columns \
             only. Project the expression and order by its output."
                .to_string(),
        ));
    }
    let mut body = query.body.as_ref().clone();
    name_compound_outputs(&mut body, &outputs)?;
    let derived = TableFactor::Derived {
        lateral: false,
        subquery: Box::new(make_query(None, body)),
        alias: Some(TableAlias {
            explicit: true,
            name: Ident::new(ORDERED_COMPOUND_ALIAS),
            columns: Vec::new(),
            at: None,
        }),
        sample: None,
    };
    let select = make_simple_select(
        vec![SelectItem::Wildcard(WildcardAdditionalOptions::default())],
        from_relation(derived),
        None,
    );
    Ok(Some(Query {
        with: query.with.clone(),
        body: Box::new(SetExpr::Select(Box::new(select))),
        order_by: Some(OrderBy {
            kind: OrderByKind::Expressions(ordered),
            interpolate: interpolate.clone(),
        }),
        limit_clause: query.limit_clause.clone(),
        fetch: query.fetch.clone(),
        locks: query.locks.clone(),
        for_clause: query.for_clause.clone(),
        settings: query.settings.clone(),
        format_clause: query.format_clause.clone(),
        pipe_operators: query.pipe_operators.clone(),
    }))
}

/// Gives SQLite's first compound arm the output names PostgreSQL exposes.
fn name_compound_outputs(
    mut body: &mut SetExpr,
    outputs: &[(Option<String>, Option<DataType>)],
) -> Result<(), Error> {
    let select = loop {
        match body {
            SetExpr::Select(select) => break select,
            SetExpr::SetOperation { left, .. } => body = left,
            SetExpr::Query(query) => body = &mut query.body,
            _ => return Ok(()),
        }
    };
    if select
        .projection
        .iter()
        .any(|item| matches!(item, SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..)))
    {
        if select.projection.iter().any(|item| {
            matches!(item, SelectItem::UnnamedExpr(expression)
                if !matches!(expression, Expr::Identifier(_) | Expr::CompoundIdentifier(_)))
        }) {
            return Err(Error::forward_refusal(
                "ORDER BY over a compound select mixing * with an unaliased expression \
                 cannot key an enum output. Alias the column or list the columns."
                    .to_string(),
            ));
        }
        return Ok(());
    }
    for (item, (name, _)) in select.projection.iter_mut().zip(outputs) {
        let Some(name) = name else { continue };
        match item {
            SelectItem::UnnamedExpr(expression) => {
                let expression = core::mem::replace(expression, Expr::Value(Value::Null.into()));
                *item = SelectItem::ExprWithAlias {
                    expr: expression,
                    alias: Ident::with_quote('"', name),
                };
            }
            SelectItem::ExprWithAlias { alias, .. } if alias.value != *name => {
                *alias = Ident::with_quote('"', name);
            }
            _ => {}
        }
    }
    Ok(())
}

/// The alias of the derived table [`ordered_compound`] sorts outside.
const ORDERED_COMPOUND_ALIAS: &str = "__pg2sqlite_ordered";

/// Whether a `DROP TYPE` of `names` drops enum types alone, which leaves
/// nothing to emit.
///
/// # Errors
///
/// Refuses `CASCADE` while a column of one of the types exists, since
/// PostgreSQL drops those columns.
pub(crate) fn drop_type(
    names: &[ObjectName],
    cascade: bool,
    schema: &ParserDB,
) -> Result<bool, Error> {
    let types: Vec<DataType> =
        names.iter().map(|name| DataType::Custom(name.clone(), Vec::new())).collect();
    if !types.iter().all(|data_type| is_enum(data_type, schema)) {
        return Ok(false);
    }
    if !cascade {
        return Ok(true);
    }
    for table in schema.tables() {
        for column in table.columns(schema)? {
            let declared = column.data_type(schema);
            if let Some(dropped) =
                types.iter().find(|data_type| same_type(data_type, declared, schema))
            {
                return Err(Error::forward_refusal(format!(
                    "DROP TYPE {dropped} CASCADE drops column {}.{}, which SQLite cannot drop \
                     alongside its CHECK. Drop the column first.",
                    table.table_name(),
                    column.column_name()
                )));
            }
        }
    }
    Ok(true)
}

/// Refuses an `ALTER TYPE ... ADD VALUE` in `statements` whose type the
/// schema the statements' epoch began from, which `started` snapshots,
/// already declares.
///
/// # Errors
///
/// Returns the refusal, since every statement translated before the epoch
/// checks and keys the labels the type had then.
pub(crate) fn reject_late_added_values(
    statements: &[Statement],
    started: impl FnOnce() -> ParserDB,
) -> Result<(), Error> {
    let mut added = statements.iter().filter_map(|statement| {
        match statement {
            Statement::AlterType(AlterType {
                name,
                operation: AlterTypeOperation::AddValue(_),
            }) => Some((statement, name)),
            _ => None,
        }
    });
    let Some(first) = added.next() else {
        return Ok(());
    };
    let started = started();
    for (statement, name) in core::iter::once(first).chain(added) {
        if is_enum(&DataType::Custom(name.clone(), Vec::new()), &started) {
            return Err(Error::forward_refusal(format!(
                "{statement} cannot be translated: {name} was declared before the last DROP \
                 TABLE, DROP VIEW or rename, or in the starting schema, so the columns already \
                 translated check its earlier labels, and SQLite changes a CHECK only by \
                 rebuilding the table. Declare the label in CREATE TYPE, or add it before that \
                 statement."
            )));
        }
    }
    Ok(())
}
