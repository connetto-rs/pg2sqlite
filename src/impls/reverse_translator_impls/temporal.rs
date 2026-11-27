use pg2sqlite_runtime::{
    TemporalFunction,
    TemporalFunction::{
        DateDifference, DateMinusDays, DateMinusInterval, DatePart, DatePlusDays, DatePlusInterval,
        DatePlusTime, DatePlusTimeTz, DateToTimestamp, DateToTimestampTz, EpochTextToTimestampTz,
        EpochToTimestampTz, Extract, Float8Out, Input, IntervalAverage, IntervalDivide,
        IntervalJustifyDays, IntervalJustifyHours, IntervalJustifyInterval, IntervalMinus,
        IntervalMultiply, IntervalNegate, IntervalPlus, IntervalScale, IntervalSum, IntervalToTime,
        IntervalTrunc, MakeDate, MakeTime, MakeTimestamp, Round, TimeDifference, TimeMinusInterval,
        TimePlusInterval, TimeToInterval, TimeToTimeTz, TimeTzAtIntervalZone, TimeTzAtZone,
        TimeTzCompare, TimeTzMinusInterval, TimeTzOut, TimeTzPlusInterval, TimeTzToTime,
        TimestampAtIntervalZone, TimestampAtZone, TimestampDifference, TimestampMinusInterval,
        TimestampPlusInterval, TimestampToDate, TimestampToTime, TimestampToTimestampTz,
        TimestampTrunc, TimestampTzAtIntervalZone, TimestampTzAtZone, TimestampTzDifference,
        TimestampTzMinusInterval, TimestampTzOut, TimestampTzPlusInterval, TimestampTzToDate,
        TimestampTzToTime, TimestampTzToTimeTz, TimestampTzToTimestamp, TimestampTzTrunc, ToChar,
    },
    TemporalFunctions, TemporalKind,
    TemporalKind::{Date, Interval, Time, TimeTz, Timestamp, TimestampTz},
};
use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, CastKind, DataType, DateTimeField, Expr, ExtractSyntax, Function,
    FunctionArguments, Ident, ObjectName, TimezoneInfo, UnaryOperator,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::function_helpers::{
        extract_exactly_reverse, simple_function_expr, single_quoted_literal,
    },
    options::TranslationContext,
    traits::ReverseTranslator,
};

pub(crate) fn classify(
    name: &ObjectName,
    options: &TranslationContext<'_>,
) -> Option<TemporalFunction> {
    let [part] = name.0.as_slice() else { return None };
    classify_name(&part.as_ident()?.value, options.get_temporal_functions()?)
}

pub(crate) fn classify_name(name: &str, functions: &TemporalFunctions) -> Option<TemporalFunction> {
    if !name
        .get(..functions.prefix.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&functions.prefix))
    {
        return None;
    }
    let fixed = [
        DatePlusDays,
        DateMinusDays,
        DateDifference,
        DatePlusInterval,
        DateMinusInterval,
        DatePlusTime,
        DatePlusTimeTz,
        TimestampPlusInterval,
        TimestampMinusInterval,
        TimestampDifference,
        TimestampTzPlusInterval,
        TimestampTzMinusInterval,
        TimestampTzDifference,
        TimePlusInterval,
        TimeMinusInterval,
        TimeDifference,
        TimeTzPlusInterval,
        TimeTzMinusInterval,
        IntervalPlus,
        IntervalMinus,
        IntervalNegate,
        IntervalMultiply,
        IntervalDivide,
        IntervalSum,
        IntervalAverage,
        IntervalScale,
        IntervalJustifyHours,
        IntervalJustifyDays,
        IntervalJustifyInterval,
        TimeToInterval,
        IntervalToTime,
        TimestampTzToTimestamp,
        TimestampToTimestampTz,
        TimestampTzToDate,
        DateToTimestampTz,
        TimestampTzToTime,
        TimestampTzToTimeTz,
        TimeToTimeTz,
        TimestampToDate,
        TimestampToTime,
        DateToTimestamp,
        TimeTzToTime,
        TimestampTzOut,
        TimeTzOut,
        Float8Out,
        TimeTzCompare,
        TimestampTzAtZone,
        TimestampAtZone,
        TimeTzAtZone,
        TimestampAtIntervalZone,
        TimestampTzAtIntervalZone,
        TimeTzAtIntervalZone,
        TimestampTrunc,
        TimestampTzTrunc,
        IntervalTrunc,
        EpochToTimestampTz,
        EpochTextToTimestampTz,
        MakeDate,
        MakeTime,
        MakeTimestamp,
    ];
    fixed
        .into_iter()
        .chain(TemporalKind::ALL.into_iter().flat_map(|kind| {
            [Input(kind), Extract(kind), DatePart(kind), ToChar(kind), Round(kind)]
        }))
        .find(|function| functions.name(*function).eq_ignore_ascii_case(name))
}

fn data_type(kind: TemporalKind, precision: Option<u64>) -> DataType {
    match kind {
        TemporalKind::Date => DataType::Date,
        TemporalKind::Time => DataType::Time(precision, TimezoneInfo::None),
        TemporalKind::TimeTz => DataType::Time(precision, TimezoneInfo::Tz),
        TemporalKind::Timestamp => DataType::Timestamp(precision, TimezoneInfo::None),
        TemporalKind::TimestampTz => DataType::Timestamp(precision, TimezoneInfo::Tz),
        TemporalKind::Interval => DataType::Interval { fields: None, precision },
    }
}

fn cast(value: Expr, data_type: DataType) -> Expr {
    let value = if matches!(value, Expr::AtTimeZone { .. } | Expr::BinaryOp { .. }) {
        Expr::Nested(Box::new(value))
    } else {
        value
    };
    Expr::Cast { kind: CastKind::DoubleColon, expr: Box::new(value), data_type, format: None }
}

pub(super) fn reverse_timetz_comparison(
    left: &Expr,
    operator: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Expr>, Error> {
    use sqlparser::ast::Value;
    if !matches!(
        operator,
        BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
    ) || !matches!(right, Expr::Value(value) if matches!(&value.value, Value::Number(number, _) if number == "0"))
    {
        return Ok(None);
    }
    let Expr::Function(function) = left else { return Ok(None) };
    if classify(&function.name, options) != Some(TemporalFunction::TimeTzCompare) {
        return Ok(None);
    }
    let FunctionArguments::List(list) = &function.args else {
        return Err(Error::reverse_refusal("timetz comparison requires positional arguments"));
    };
    if !matches!(function.parameters, FunctionArguments::None)
        || function.null_treatment.is_some()
        || function.filter.is_some()
        || function.over.is_some()
        || !function.within_group.is_empty()
        || list.duplicate_treatment.is_some()
        || !list.clauses.is_empty()
        || list.args.iter().any(|argument| {
            !matches!(
                argument,
                sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(_))
            )
        })
    {
        return Err(Error::reverse_refusal("unsupported timetz comparison call shape"));
    }
    let arguments = extract_exactly_reverse(&function.args, 2, &function.name.to_string())?;
    Ok(Some(Expr::BinaryOp {
        left: Box::new(cast(
            arguments[0].reverse_translate(schema, options)?,
            data_type(TemporalKind::TimeTz, None),
        )),
        op: operator.clone(),
        right: Box::new(cast(
            arguments[1].reverse_translate(schema, options)?,
            data_type(TemporalKind::TimeTz, None),
        )),
    }))
}

fn interval_key_operand(expression: &Expr, options: &TranslationContext<'_>) -> Option<Expr> {
    use sqlparser::{dialect::SQLiteDialect, parser::Parser};
    if !matches!(expression, Expr::Nested(_) | Expr::Subquery(_) | Expr::Case { .. }) {
        return None;
    }
    let expression = expression.to_string();
    if !expression.contains("json_extract(") {
        return None;
    }
    let placeholder = "pg2sqlite_reverse_interval_operand";
    let operand = Expr::Identifier(Ident::new(placeholder));
    let inline_options = options.without_subqueries();
    for context in [options, &inline_options] {
        let template = crate::impls::interval::keys::key_expr(&operand, context).ok()?.to_string();
        let (prefix, remaining) = template.split_once(placeholder)?;
        let suffix = remaining.split(placeholder).next()?;
        let Some(remainder) = expression.strip_prefix(prefix) else { continue };
        for (position, _) in remainder.match_indices(suffix) {
            let candidate = &remainder[..position];
            if template.replace(placeholder, candidate) != expression {
                continue;
            }
            let parsed = Parser::new(&SQLiteDialect {})
                .try_with_sql(candidate)
                .and_then(|mut parser| parser.parse_expr())
                .ok()?;
            if crate::impls::interval::keys::key_expr(&parsed, context).ok()?.to_string()
                == expression
            {
                return Some(parsed);
            }
        }
    }
    None
}

fn interval_key_literal(expression: &Expr) -> Option<Expr> {
    use sqlparser::ast::Value;
    if matches!(expression, Expr::Value(value) if value.value == Value::Null) {
        return Some(expression.clone());
    }
    let key = single_quoted_literal(expression)?;
    if key.len() != 23 || !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let text = if key == "00000000000000000000000" {
        "-infinity".to_string()
    } else if key == "99999999999999999999999" {
        "infinity".to_string()
    } else {
        let days = key[..12].parse::<i64>().ok()? - 100_000_000_000;
        let micros = key[12..].parse::<i64>().ok()?;
        if micros >= 86_400_000_000 {
            return None;
        }
        format!("{days} days {micros} microseconds")
    };
    Some(crate::impls::function_helpers::string_literal(&text))
}

pub(super) fn reverse_interval_comparison(
    left: &Expr,
    operator: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Expr>, Error> {
    if !matches!(
        operator,
        BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
    ) {
        return Ok(None);
    }
    let left_operand = interval_key_operand(left, options);
    let right_operand = interval_key_operand(right, options);
    if left_operand.is_none() && right_operand.is_none() {
        return Ok(None);
    }
    let left = left_operand.or_else(|| interval_key_literal(left));
    let right = right_operand.or_else(|| interval_key_literal(right));
    let (Some(left), Some(right)) = (left, right) else {
        return Err(Error::reverse_refusal("interval comparison requires two generated keys"));
    };
    Ok(Some(Expr::BinaryOp {
        left: Box::new(cast(
            left.reverse_translate(schema, options)?,
            data_type(TemporalKind::Interval, None),
        )),
        op: operator.clone(),
        right: Box::new(cast(
            right.reverse_translate(schema, options)?,
            data_type(TemporalKind::Interval, None),
        )),
    }))
}

fn reverse_at_zone(
    operation: TemporalFunction,
    arguments: &[&Expr],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    let kind = match operation {
        TimestampTzAtZone | TimestampTzAtIntervalZone => TimestampTz,
        TimestampAtZone | TimestampAtIntervalZone => Timestamp,
        _ => TimeTz,
    };
    let zone = arguments[0].reverse_translate(schema, options)?;
    let zone = if matches!(
        operation,
        TimestampAtIntervalZone | TimestampTzAtIntervalZone | TimeTzAtIntervalZone
    ) {
        cast(zone, data_type(Interval, None))
    } else {
        zone
    };
    Ok(Expr::AtTimeZone {
        timestamp: Box::new(cast(
            arguments[1].reverse_translate(schema, options)?,
            data_type(kind, None),
        )),
        time_zone: Box::new(zone),
    })
}

pub(super) fn reverse_clock(function: &Function) -> Result<Option<Expr>, Error> {
    if !matches!(function.name.0.as_slice(), [part] if part.as_ident()
        .is_some_and(|ident| ident.value.eq_ignore_ascii_case("strftime")))
    {
        return Ok(None);
    }
    let Ok(arguments) = extract_exactly_reverse(&function.args, 2, "strftime") else {
        return Ok(None);
    };
    if single_quoted_literal(arguments[0]) != Some("%Y-%m-%d %H:%M:%f000+00:00")
        || single_quoted_literal(arguments[1]) != Some("now")
    {
        return Ok(None);
    }
    validate_call(function, TimestampTzOut)?;
    Ok(Some(simple_function_expr("NOW", vec![], None)))
}

fn validate_call(function: &Function, operation: TemporalFunction) -> Result<(), Error> {
    let aggregate = matches!(operation, IntervalSum | IntervalAverage);
    let FunctionArguments::List(list) = &function.args else {
        return Err(Error::reverse_refusal("temporal functions require positional arguments"));
    };
    if !matches!(function.parameters, FunctionArguments::None)
        || function.null_treatment.is_some()
        || list.args.iter().any(|argument| {
            !matches!(
                argument,
                sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(_))
            )
        })
        || (!aggregate
            && (function.filter.is_some()
                || function.over.is_some()
                || !function.within_group.is_empty()
                || list.duplicate_treatment.is_some()
                || !list.clauses.is_empty()))
    {
        return Err(Error::reverse_refusal("unsupported temporal function call shape"));
    }
    if matches!(operation, Round(Date | Interval) | ToChar(TimeTz)) {
        return Err(Error::reverse_refusal("unsupported temporal function kind"));
    }
    Ok(())
}

fn reverse_binary(
    function: &Function,
    operation: TemporalFunction,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Expr>, Error> {
    let binary = match operation {
        DatePlusDays => Some((BinaryOperator::Plus, Date, None, 2)),
        DateMinusDays => Some((BinaryOperator::Minus, Date, None, 2)),
        DateDifference => Some((BinaryOperator::Minus, Date, Some(Date), 2)),
        DatePlusInterval => Some((BinaryOperator::Plus, Date, Some(Interval), 2)),
        DateMinusInterval => Some((BinaryOperator::Minus, Date, Some(Interval), 2)),
        DatePlusTime => Some((BinaryOperator::Plus, Date, Some(Time), 2)),
        DatePlusTimeTz => Some((BinaryOperator::Plus, Date, Some(TimeTz), 2)),
        TimestampPlusInterval => Some((BinaryOperator::Plus, Timestamp, Some(Interval), 2)),
        TimestampMinusInterval => Some((BinaryOperator::Minus, Timestamp, Some(Interval), 2)),
        TimestampDifference => Some((BinaryOperator::Minus, Timestamp, Some(Timestamp), 2)),
        TimestampTzPlusInterval => Some((BinaryOperator::Plus, TimestampTz, Some(Interval), 3)),
        TimestampTzMinusInterval => Some((BinaryOperator::Minus, TimestampTz, Some(Interval), 3)),
        TimestampTzDifference => Some((BinaryOperator::Minus, TimestampTz, Some(TimestampTz), 2)),
        TimePlusInterval => Some((BinaryOperator::Plus, Time, Some(Interval), 2)),
        TimeMinusInterval => Some((BinaryOperator::Minus, Time, Some(Interval), 2)),
        TimeDifference => Some((BinaryOperator::Minus, Time, Some(Time), 2)),
        TimeTzPlusInterval => Some((BinaryOperator::Plus, TimeTz, Some(Interval), 2)),
        TimeTzMinusInterval => Some((BinaryOperator::Minus, TimeTz, Some(Interval), 2)),
        IntervalPlus => Some((BinaryOperator::Plus, Interval, Some(Interval), 2)),
        IntervalMinus => Some((BinaryOperator::Minus, Interval, Some(Interval), 2)),
        IntervalMultiply => Some((BinaryOperator::Multiply, Interval, None, 2)),
        IntervalDivide => Some((BinaryOperator::Divide, Interval, None, 2)),
        _ => None,
    };
    if let Some((operator, left_kind, right_kind, arity)) = binary {
        let arguments = extract_exactly_reverse(&function.args, arity, &function.name.to_string())?;
        let left =
            cast(arguments[0].reverse_translate(schema, options)?, data_type(left_kind, None));
        let right = arguments[1].reverse_translate(schema, options)?;
        let right = cast(
            right,
            right_kind.map_or_else(
                || {
                    if matches!(operation, DatePlusDays | DateMinusDays) {
                        DataType::Integer(None)
                    } else {
                        DataType::DoublePrecision
                    }
                },
                |kind| data_type(kind, None),
            ),
        );
        return Ok(Some(Expr::Nested(Box::new(Expr::BinaryOp {
            left: Box::new(left),
            op: operator,
            right: Box::new(right),
        }))));
    }
    Ok(None)
}

fn reverse_cast(
    operation: TemporalFunction,
    arguments: &[&Expr],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Expr>, Error> {
    let target = match operation {
        TimestampTzToTimestamp | DateToTimestamp => Some(Timestamp),
        TimestampToTimestampTz | DateToTimestampTz => Some(TimestampTz),
        TimestampTzToDate | TimestampToDate => Some(Date),
        TimestampTzToTime | TimestampToTime | TimeTzToTime | IntervalToTime => Some(Time),
        TimestampTzToTimeTz | TimeToTimeTz => Some(TimeTz),
        TimeToInterval => Some(Interval),
        Input(kind) | Round(kind) => Some(kind),
        _ => None,
    };
    if let Some(target) = target {
        let precision = if matches!(operation, Round(_)) {
            let Expr::Value(value) = arguments[1] else {
                return Err(Error::reverse_refusal(
                    "temporal precision must be an integer literal",
                ));
            };
            let sqlparser::ast::Value::Number(number, _) = &value.value else {
                return Err(Error::reverse_refusal(
                    "temporal precision must be an integer literal",
                ));
            };
            Some(number.parse::<u64>().ok().filter(|precision| *precision <= 6).ok_or_else(
                || Error::reverse_refusal("temporal precision must be between zero and six"),
            )?)
        } else {
            None
        };
        let value = arguments[0].reverse_translate(schema, options)?;
        let source = match operation {
            TimestampTzToTimestamp
            | TimestampTzToDate
            | TimestampTzToTime
            | TimestampTzToTimeTz => Some(TimestampTz),
            TimestampToTimestampTz | TimestampToDate | TimestampToTime => Some(Timestamp),
            DateToTimestamp | DateToTimestampTz => Some(Date),
            TimeToTimeTz | TimeToInterval => Some(Time),
            TimeTzToTime => Some(TimeTz),
            IntervalToTime => Some(Interval),
            _ => None,
        };
        let value =
            if let Some(source) = source { cast(value, data_type(source, None)) } else { value };
        return Ok(Some(cast(value, data_type(target, precision))));
    }
    Ok(None)
}

fn arity(operation: TemporalFunction) -> usize {
    match operation {
        TimeToTimeTz | Input(_) | TimeTzAtZone | TimestampTzTrunc | IntervalScale | MakeDate
        | MakeTime => 3,
        TimestampTzToTimestamp
        | TimestampToTimestampTz
        | TimestampTzToDate
        | DateToTimestampTz
        | TimestampTzToTime
        | TimestampTzToTimeTz
        | TimestampTzOut
        | TimestampTzAtZone
        | TimestampAtZone
        | TimestampAtIntervalZone
        | TimestampTzAtIntervalZone
        | TimeTzAtIntervalZone
        | TimestampTrunc
        | IntervalTrunc
        | TimeTzCompare
        | Round(_) => 2,
        Extract(kind) | DatePart(kind) => {
            if kind == TimestampTz {
                3
            } else {
                2
            }
        }
        ToChar(kind) => {
            if matches!(kind, Date | TimestampTz) {
                3
            } else {
                2
            }
        }
        MakeTimestamp => 6,
        _ => 1,
    }
}

fn reverse_interval_scale(
    arguments: &[&Expr],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    use sqlparser::ast::{IntervalFields, Value};
    let fields = match arguments[1] {
        Expr::Value(value) if value.value == Value::Null => None,
        value => {
            let name = single_quoted_literal(value)
                .ok_or_else(|| Error::reverse_refusal("interval fields must be a literal"))?;
            Some(match name.to_ascii_uppercase().as_str() {
                "YEAR" => IntervalFields::Year,
                "MONTH" => IntervalFields::Month,
                "DAY" => IntervalFields::Day,
                "HOUR" => IntervalFields::Hour,
                "MINUTE" => IntervalFields::Minute,
                "SECOND" => IntervalFields::Second,
                "YEAR TO MONTH" => IntervalFields::YearToMonth,
                "DAY TO HOUR" => IntervalFields::DayToHour,
                "DAY TO MINUTE" => IntervalFields::DayToMinute,
                "DAY TO SECOND" => IntervalFields::DayToSecond,
                "HOUR TO MINUTE" => IntervalFields::HourToMinute,
                "HOUR TO SECOND" => IntervalFields::HourToSecond,
                "MINUTE TO SECOND" => IntervalFields::MinuteToSecond,
                _ => return Err(Error::reverse_refusal("unsupported interval fields")),
            })
        }
    };
    let precision = match arguments[2] {
        Expr::Value(value) if value.value == Value::Null => None,
        Expr::Value(value) => {
            let Value::Number(number, _) = &value.value else {
                return Err(Error::reverse_refusal("interval precision must be an integer"));
            };
            Some(number.parse::<u64>().ok().filter(|precision| *precision <= 6).ok_or_else(
                || Error::reverse_refusal("interval precision must be between zero and six"),
            )?)
        }
        _ => return Err(Error::reverse_refusal("interval precision must be a literal")),
    };
    Ok(cast(
        arguments[0].reverse_translate(schema, options)?,
        DataType::Interval { fields, precision },
    ))
}

fn reverse_aggregate(
    function: &Function,
    operation: TemporalFunction,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    let name = if operation == IntervalSum { "sum" } else { "avg" };
    let reversed = super::function::build_reverse_function(
        ObjectName::from(vec![Ident::new(name)]),
        function,
        &function.args,
        schema,
        options,
    )?;
    let Expr::Function(mut reversed) = reversed else { unreachable!() };
    let FunctionArguments::List(list) = &mut reversed.args else { unreachable!() };
    let argument = list.args.remove(0);
    let sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(value)) =
        argument
    else {
        unreachable!()
    };
    list.args.push(sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(
        cast(value, data_type(Interval, None)),
    )));
    Ok(Expr::Function(reversed))
}

fn reverse_extract(
    kind: TemporalKind,
    arguments: &[&Expr],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    let field = single_quoted_literal(arguments[0])
        .ok_or_else(|| Error::reverse_refusal("extract requires a literal field"))?;
    let field = sql_values::postgres::ExtractField::parse(field)
        .map_or_else(|| DateTimeField::Custom(Ident::with_quote('\'', field)), extract_field);
    Ok(Expr::Extract {
        field,
        syntax: ExtractSyntax::From,
        expr: Box::new(cast(
            arguments[1].reverse_translate(schema, options)?,
            data_type(kind, None),
        )),
    })
}

fn reverse_named_function(
    operation: TemporalFunction,
    arguments: &[&Expr],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    match operation {
        DatePart(kind) | ToChar(kind) => {
            let (name, value_index) =
                if matches!(operation, DatePart(_)) { ("date_part", 1) } else { ("to_char", 0) };
            let values = arguments[..2]
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let value = value.reverse_translate(schema, options)?;
                    Ok(if index == value_index {
                        cast(value, data_type(kind, None))
                    } else {
                        value
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            Ok(simple_function_expr(name, values, None))
        }
        TimestampTrunc | TimestampTzTrunc | IntervalTrunc => {
            let kind = match operation {
                TimestampTrunc => Timestamp,
                TimestampTzTrunc => TimestampTz,
                _ => Interval,
            };
            let mut values = vec![
                arguments[0].reverse_translate(schema, options)?,
                cast(arguments[1].reverse_translate(schema, options)?, data_type(kind, None)),
            ];
            if operation == TimestampTzTrunc {
                values.push(arguments[2].reverse_translate(schema, options)?);
            }
            Ok(simple_function_expr("date_trunc", values, None))
        }
        EpochTextToTimestampTz => {
            Ok(simple_function_expr(
                "to_timestamp",
                vec![cast(
                    arguments[0].reverse_translate(schema, options)?,
                    DataType::DoublePrecision,
                )],
                None,
            ))
        }
        EpochToTimestampTz | MakeDate | MakeTime | MakeTimestamp => {
            let name = match operation {
                EpochToTimestampTz => "to_timestamp",
                MakeDate => "make_date",
                MakeTime => "make_time",
                _ => "make_timestamp",
            };
            let values = arguments
                .iter()
                .map(|value| value.reverse_translate(schema, options))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(simple_function_expr(name, values, None))
        }
        _ => unreachable!("named temporal function dispatch"),
    }
}

pub(super) fn reverse(
    function: &Function,
    operation: TemporalFunction,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    validate_call(function, operation)?;
    if let Some(expression) = reverse_binary(function, operation, schema, options)? {
        return Ok(expression);
    }
    let arguments =
        extract_exactly_reverse(&function.args, arity(operation), &function.name.to_string())?;
    if let Some(expression) = reverse_cast(operation, &arguments, schema, options)? {
        return Ok(expression);
    }
    match operation {
        TimeTzCompare => {
            Err(Error::reverse_refusal("timetz comparison must be compared directly with zero"))
        }
        Float8Out => {
            Ok(cast(
                cast(arguments[0].reverse_translate(schema, options)?, DataType::DoublePrecision),
                DataType::Text,
            ))
        }
        TimestampTzOut | TimeTzOut => {
            let kind = if operation == TimestampTzOut { TimestampTz } else { TimeTz };
            Ok(cast(
                cast(arguments[0].reverse_translate(schema, options)?, data_type(kind, None)),
                DataType::Text,
            ))
        }
        TimestampTzAtZone
        | TimestampAtZone
        | TimeTzAtZone
        | TimestampAtIntervalZone
        | TimestampTzAtIntervalZone
        | TimeTzAtIntervalZone => reverse_at_zone(operation, &arguments, schema, options),
        IntervalScale => reverse_interval_scale(&arguments, schema, options),
        IntervalJustifyHours | IntervalJustifyDays | IntervalJustifyInterval => {
            let name = match operation {
                IntervalJustifyHours => "justify_hours",
                IntervalJustifyDays => "justify_days",
                _ => "justify_interval",
            };
            Ok(simple_function_expr(
                name,
                vec![cast(
                    arguments[0].reverse_translate(schema, options)?,
                    data_type(Interval, None),
                )],
                None,
            ))
        }
        IntervalNegate => {
            Ok(Expr::UnaryOp {
                op: UnaryOperator::Minus,
                expr: Box::new(cast(
                    arguments[0].reverse_translate(schema, options)?,
                    data_type(Interval, None),
                )),
            })
        }
        IntervalSum | IntervalAverage => reverse_aggregate(function, operation, schema, options),
        Extract(kind) => reverse_extract(kind, &arguments, schema, options),
        DatePart(_)
        | ToChar(_)
        | TimestampTrunc
        | TimestampTzTrunc
        | IntervalTrunc
        | EpochToTimestampTz
        | EpochTextToTimestampTz
        | MakeDate
        | MakeTime
        | MakeTimestamp => reverse_named_function(operation, &arguments, schema, options),
        _ => unreachable!("binary and cast operations return before dispatch"),
    }
}

fn extract_field(field: sql_values::postgres::ExtractField) -> DateTimeField {
    use sql_values::postgres::ExtractField;
    match field {
        ExtractField::Microseconds => DateTimeField::Microseconds,
        ExtractField::Milliseconds => DateTimeField::Milliseconds,
        ExtractField::Second => DateTimeField::Second,
        ExtractField::Minute => DateTimeField::Minute,
        ExtractField::Hour => DateTimeField::Hour,
        ExtractField::Day => DateTimeField::Day,
        ExtractField::Week => DateTimeField::Week(None),
        ExtractField::Month => DateTimeField::Month,
        ExtractField::Quarter => DateTimeField::Quarter,
        ExtractField::Year => DateTimeField::Year,
        ExtractField::Decade => DateTimeField::Decade,
        ExtractField::Century => DateTimeField::Century,
        ExtractField::Millennium => DateTimeField::Millennium,
        ExtractField::DayOfWeek => DateTimeField::Dow,
        ExtractField::IsoDayOfWeek => DateTimeField::Isodow,
        ExtractField::DayOfYear => DateTimeField::Doy,
        ExtractField::IsoYear => DateTimeField::Isoyear,
        ExtractField::Epoch => DateTimeField::Epoch,
        ExtractField::Julian => DateTimeField::Julian,
        ExtractField::Timezone => DateTimeField::Timezone,
        ExtractField::TimezoneHour => DateTimeField::TimezoneHour,
        ExtractField::TimezoneMinute => DateTimeField::TimezoneMinute,
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    #[test]
    fn extract_reversal_defers_field_validation_for_strict_nulls() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(TemporalFunctions::default()),
        );
        for (kind, field, expected) in [
            ("date", "hour", "EXTRACT(HOUR FROM NULL::DATE)"),
            ("time", "day", "EXTRACT(DAY FROM NULL::TIME)"),
            ("interval", "dow", "EXTRACT(DOW FROM NULL::INTERVAL)"),
            ("timestamp", "timezone", "EXTRACT(TIMEZONE FROM NULL::TIMESTAMP)"),
            ("timestamp", "fortnight", "EXTRACT('fortnight' FROM NULL::TIMESTAMP)"),
        ] {
            let expression = parse_expr(&format!("pg2sqlite_extract_{kind}('{field}', NULL)"));
            assert_eq!(
                expression.reverse_translate(&schema, &options).unwrap().to_string(),
                expected
            );
        }
    }

    #[test]
    fn generated_numeric_rounding_restores_unscaled_round() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(TemporalFunctions::default()),
        );
        let value = parse_expr("pg2sqlite_extract_timestamp('epoch', value) + 1");
        let expression = crate::impls::shared_helpers::rescale_minor_units(value, 6, 0);
        for expression in [expression.clone(), parse_expr(&expression.to_string())] {
            let reversed = expression.reverse_translate(&schema, &options).unwrap().to_string();
            assert_eq!(reversed, "round(EXTRACT(EPOCH FROM value::TIMESTAMP) + 0.000001)");
        }
        let expression =
            parse_expr("pg2sqlite_timestamptz_out(pg2sqlite_timestamp_zone('UTC', value), 'UTC')");
        assert_eq!(
            expression.reverse_translate(&schema, &options).unwrap().to_string(),
            "(value::TIMESTAMP AT TIME ZONE 'UTC')::TIMESTAMPTZ::TEXT",
        );
    }

    #[test]
    fn scaled_extract_arithmetic_and_float_casts_restore_units() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(TemporalFunctions::default()),
        );
        for (sqlite, expected) in [
            (
                "pg2sqlite_extract_timestamp('epoch', value) * 1000000",
                "EXTRACT(EPOCH FROM value::TIMESTAMP) * 1000000",
            ),
            (
                "1000000 * pg2sqlite_extract_timestamp('epoch', value)",
                "1000000 * EXTRACT(EPOCH FROM value::TIMESTAMP)",
            ),
            (
                "pg2sqlite_extract_timestamp('epoch', value) / 1000000.0",
                "EXTRACT(EPOCH FROM value::TIMESTAMP)::DOUBLE PRECISION",
            ),
            (
                "CAST(round(pg2sqlite_date_part_timestamp('epoch', value) * 1000000) AS INTEGER)",
                "CAST(round(date_part('epoch', value::TIMESTAMP) * 1000000) AS BIGINT)",
            ),
        ] {
            let reversed = parse_expr(sqlite).reverse_translate(&schema, &options).unwrap();
            assert_eq!(reversed.to_string(), expected);
        }
        assert!(
            parse_expr("pg2sqlite_extract_timestamp('epoch', value) / 1000000")
                .reverse_translate(&schema, &options)
                .is_err()
        );
    }

    #[test]
    fn generated_numeric_text_rendering_restores_extract_text() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(TemporalFunctions::default()),
        );
        let operand = parse_expr("pg2sqlite_extract_timestamp('epoch', value)");
        let rendered = crate::impls::shared_helpers::render_minor_units_as_text(&operand, 6);
        let text = rendered.to_string();
        for expression in [rendered, parse_expr(&text)] {
            let reversed = expression.reverse_translate(&schema, &options).unwrap().to_string();
            assert_eq!(reversed, "EXTRACT(EPOCH FROM value::TIMESTAMP)::TEXT");
        }
        for altered in [
            text.replacen("'Infinity'", "'wrong'", 1),
            text.replacen("value", "other_value", 1),
            crate::impls::shared_helpers::render_minor_units_as_text(&operand, 3).to_string(),
            "printf('%s%d.%06d', '', 1, 0)".to_string(),
        ] {
            assert!(
                parse_expr(&altered).reverse_translate(&schema, &options).is_err(),
                "{altered}"
            );
        }
    }

    #[test]
    fn chained_zone_conversions_keep_cast_precedence() {
        let schema = empty_schema();
        let functions = TemporalFunctions::default();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(functions.clone()),
        );
        let expression = parse_expr(&format!(
            "{}('UTC', {}('UTC', naive))",
            functions.name(TimestampTzAtZone),
            functions.name(TimestampAtZone),
        ));
        let reversed = expression.reverse_translate(&schema, &options).unwrap().to_string();
        assert_eq!(
            reversed,
            "(naive::TIMESTAMP AT TIME ZONE 'UTC')::TIMESTAMPTZ AT TIME ZONE 'UTC'",
        );
    }

    #[test]
    fn canonical_clock_restores_now_without_admitting_other_strftime_calls() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(TemporalFunctions::default()),
        );
        let canonical = crate::impls::zone_clock::canonical_now();
        assert_eq!(canonical.reverse_translate(&schema, &options).unwrap().to_string(), "NOW()");
        let zoned = parse_expr(&format!(
            "{}('UTC', strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now'))",
            TemporalFunctions::default().name(TimestampTzAtZone),
        ));
        assert!(zoned.reverse_translate(&schema, &options).unwrap().to_string().contains("NOW()"));
        for expression in [
            "strftime('%Y', 'now')",
            "strftime('%Y-%m-%d %H:%M:%f000+00:00', '2001-01-01')",
            "strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now', '+1 day')",
            "strftime(DISTINCT '%Y-%m-%d %H:%M:%f000+00:00', 'now')",
        ] {
            assert!(parse_expr(expression).reverse_translate(&schema, &options).is_err());
        }
        let undeployed = TranslationContext::from_owned(Pg2SqliteOptions::default());
        assert_eq!(canonical.reverse_translate(&schema, &undeployed).unwrap().to_string(), "NOW()");
    }

    #[test]
    fn timetz_comparison_restores_each_operand_once() {
        let schema = crate::impls::test_fixtures::empty_schema();
        let options = TranslationContext::from_owned(
            crate::prelude::Pg2SqliteOptions::default()
                .with_temporal_functions(TemporalFunctions::default()),
        );
        for operator in ["=", "<>", "<", "<=", ">", ">="] {
            let expression = crate::impls::test_fixtures::parse_expr(&format!(
                "pg2sqlite_timetz_cmp(left_value, right_value) {operator} 0"
            ));
            let reversed = expression.reverse_translate(&schema, &options).unwrap().to_string();
            assert_eq!(reversed, format!("left_value::TIMETZ {operator} right_value::TIMETZ"));
        }
        for expression in [
            "pg2sqlite_timetz_cmp(left_value, right_value)",
            "pg2sqlite_timetz_cmp(left_value, right_value) < 1",
            "pg2sqlite_timetz_cmp(DISTINCT left_value, right_value) = 0",
        ] {
            assert!(
                crate::impls::test_fixtures::parse_expr(expression)
                    .reverse_translate(&schema, &options)
                    .is_err()
            );
        }
    }

    #[test]
    fn generated_interval_comparisons_restore_without_admitting_json_extraction() {
        let schema = crate::impls::test_fixtures::empty_schema();
        let options = TranslationContext::from_owned(
            crate::prelude::Pg2SqliteOptions::default()
                .with_temporal_functions(TemporalFunctions::default()),
        );
        let inline_options = options.without_subqueries();
        for context in [&options, &inline_options] {
            let operand = Expr::Identifier(Ident::new("duration"));
            let key = crate::impls::interval::keys::key_expr(&operand, context).unwrap();
            for right in [
                key.clone(),
                crate::impls::function_helpers::string_literal("10000000000100000000000"),
            ] {
                let expression = Expr::BinaryOp {
                    left: Box::new(key.clone()),
                    op: BinaryOperator::Lt,
                    right: Box::new(right),
                };
                let reversed = expression.reverse_translate(&schema, &options).unwrap().to_string();
                assert!(reversed.contains("duration::INTERVAL"), "{reversed}");
                assert!(!reversed.contains("json_extract"), "{reversed}");
            }
        }
        for expression in [
            "json_extract(duration, '$[0]') = 1",
            "datetime('2024-01-01', '+1 day')",
            "date('2024-01-01', '+1 day')",
        ] {
            assert!(
                crate::impls::test_fixtures::parse_expr(expression)
                    .reverse_translate(&schema, &options)
                    .is_err()
            );
        }
        let literal = crate::impls::test_fixtures::parse_expr(
            "'10000000000100000000000' = '10000000000200000000000'",
        );
        assert!(
            !literal.reverse_translate(&schema, &options).unwrap().to_string().contains("INTERVAL")
        );
    }

    use super::*;
    use crate::{
        impls::test_fixtures::{empty_schema, parse_expr},
        prelude::Pg2SqliteOptions,
    };

    #[test]
    fn configured_names_restore_every_temporal_operation() {
        let functions = TemporalFunctions::new("replica_");
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(functions.clone()),
        );
        let schema = empty_schema();
        let cases = [
            (DatePlusDays, "value, 1", "+"),
            (DateMinusDays, "value, 1", "-"),
            (DateDifference, "value, other", "-"),
            (DatePlusInterval, "value, other", "+"),
            (DateMinusInterval, "value, other", "-"),
            (DatePlusTime, "value, other", "+"),
            (DatePlusTimeTz, "value, other", "+"),
            (TimestampPlusInterval, "value, other", "+"),
            (TimestampMinusInterval, "value, other", "-"),
            (TimestampDifference, "value, other", "-"),
            (TimestampTzPlusInterval, "value, other, 'UTC'", "+"),
            (TimestampTzMinusInterval, "value, other, 'UTC'", "-"),
            (TimestampTzDifference, "value, other", "-"),
            (TimePlusInterval, "value, other", "+"),
            (TimeMinusInterval, "value, other", "-"),
            (TimeDifference, "value, other", "-"),
            (TimeTzPlusInterval, "value, other", "+"),
            (TimeTzMinusInterval, "value, other", "-"),
            (IntervalPlus, "value, other", "+"),
            (IntervalMinus, "value, other", "-"),
            (IntervalNegate, "value", "-"),
            (IntervalMultiply, "value, 2", "*"),
            (IntervalDivide, "value, 2", "/"),
            (IntervalSum, "value", "sum("),
            (IntervalAverage, "value", "avg("),
            (IntervalScale, "value, 'DAY TO SECOND', 3", "::INTERVAL DAY TO SECOND(3)"),
            (IntervalJustifyHours, "value", "justify_hours("),
            (IntervalJustifyDays, "value", "justify_days("),
            (IntervalJustifyInterval, "value", "justify_interval("),
            (TimeToInterval, "value", "::TIME::INTERVAL"),
            (IntervalToTime, "value", "::INTERVAL::TIME"),
            (TimestampTzToTimestamp, "value, 'UTC'", "::TIMESTAMP"),
            (TimestampToTimestampTz, "value, 'UTC'", "::TIMESTAMPTZ"),
            (TimestampTzToDate, "value, 'UTC'", "::DATE"),
            (DateToTimestampTz, "value, 'UTC'", "::TIMESTAMPTZ"),
            (TimestampTzToTime, "value, 'UTC'", "::TIME"),
            (TimestampTzToTimeTz, "value, 'UTC'", "::TIMETZ"),
            (TimeToTimeTz, "value, 'UTC', 'now'", "::TIMETZ"),
            (TimestampToDate, "value", "::DATE"),
            (TimestampToTime, "value", "::TIME"),
            (DateToTimestamp, "value", "::TIMESTAMP"),
            (TimeTzToTime, "value", "::TIME"),
            (TimestampTzOut, "value, 'UTC'", "::TEXT"),
            (TimeTzOut, "value", "::TEXT"),
            (Float8Out, "value", "::DOUBLE PRECISION::TEXT"),
            (TimestampTzAtZone, "'Europe/Rome', value", "AT TIME ZONE"),
            (TimestampAtZone, "'Europe/Rome', value", "AT TIME ZONE"),
            (TimeTzAtZone, "'Europe/Rome', value, 'now'", "AT TIME ZONE"),
            (TimestampTzAtIntervalZone, "'01:00', value", "AT TIME ZONE '01:00'::INTERVAL"),
            (TimestampAtIntervalZone, "'01:00', value", "AT TIME ZONE '01:00'::INTERVAL"),
            (TimeTzAtIntervalZone, "'01:00', value", "AT TIME ZONE '01:00'::INTERVAL"),
            (TimestampTrunc, "'day', value", "date_trunc("),
            (TimestampTzTrunc, "'day', value, 'Europe/Rome'", "date_trunc("),
            (IntervalTrunc, "'day', value", "date_trunc("),
            (EpochToTimestampTz, "1.5", "to_timestamp("),
            (EpochTextToTimestampTz, "'1.5'", "to_timestamp('1.5'::DOUBLE PRECISION)"),
            (MakeDate, "2024, 1, 2", "make_date("),
            (MakeTime, "1, 2, 3.5", "make_time("),
            (MakeTimestamp, "2024, 1, 2, 3, 4, 5.5", "make_timestamp("),
        ];
        for (operation, arguments, expected) in cases {
            let name = functions.name(operation);
            assert_eq!(classify_name(&name.to_uppercase(), &functions), Some(operation));
            let expression = parse_expr(&format!("{name}({arguments})"));
            let reversed = expression.reverse_translate(&schema, &options).unwrap().to_string();
            assert!(reversed.contains(expected), "{operation:?} became {reversed}");
            assert!(!reversed.contains("replica_"));
        }
    }

    #[test]
    fn configured_names_restore_every_temporal_operation_of_each_kind() {
        let functions = TemporalFunctions::new("replica_");
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(functions.clone()),
        );
        let schema = empty_schema();
        for kind in TemporalKind::ALL {
            let fields = if kind == TemporalKind::TimestampTz {
                "'epoch', value, 'UTC'"
            } else {
                "'epoch', value"
            };
            for (operation, arguments, expected) in [
                (Input(kind), "value, 'UTC', 'now'", "::"),
                (Extract(kind), fields, "EXTRACT("),
                (DatePart(kind), fields, "date_part("),
            ] {
                let expression = parse_expr(&format!("{}({arguments})", functions.name(operation)));
                let reversed = expression.reverse_translate(&schema, &options).unwrap().to_string();
                assert!(reversed.contains(expected), "{operation:?} became {reversed}");
            }
            if kind != TemporalKind::TimeTz {
                let arguments = if matches!(kind, TemporalKind::Date | TemporalKind::TimestampTz) {
                    "value, 'YYYY', 'UTC'"
                } else {
                    "value, 'YYYY'"
                };
                let expression =
                    parse_expr(&format!("{}({arguments})", functions.name(ToChar(kind))));
                assert!(
                    expression
                        .reverse_translate(&schema, &options)
                        .unwrap()
                        .to_string()
                        .starts_with("to_char(")
                );
            }
            if !matches!(kind, TemporalKind::Date | TemporalKind::Interval) {
                let expression = parse_expr(&format!("{}(value, 3)", functions.name(Round(kind))));
                assert!(
                    expression
                        .reverse_translate(&schema, &options)
                        .unwrap()
                        .to_string()
                        .contains("(3)")
                );
            }
        }
    }

    #[test]
    fn temporal_names_require_the_configured_prefix() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(Pg2SqliteOptions::default());
        assert!(
            parse_expr("pg2sqlite_date_pli(value, 1)")
                .reverse_translate(&schema, &options)
                .is_err()
        );
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(TemporalFunctions::new("replica_")),
        );
        assert!(
            parse_expr("pg2sqlite_date_pli(value, 1)")
                .reverse_translate(&schema, &options)
                .is_err()
        );
        assert!(
            parse_expr("replica_date_pli(value)").reverse_translate(&schema, &options).is_err()
        );
        assert!(
            parse_expr("replica_round_time(value, 7)")
                .reverse_translate(&schema, &options)
                .is_err()
        );
        assert_eq!(
            parse_expr("replica_extract_time('julian', value)")
                .reverse_translate(&schema, &options)
                .unwrap()
                .to_string(),
            "EXTRACT(JULIAN FROM value::TIME)",
        );
        for arguments in ["value, 'BOGUS', NULL", "value, NULL, 7", "value, NULL, precision"] {
            assert!(
                parse_expr(&format!("replica_interval_scale({arguments})"))
                    .reverse_translate(&schema, &options)
                    .is_err()
            );
        }
    }

    #[test]
    fn temporal_aggregates_keep_filter_and_distinct() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(TemporalFunctions::default()),
        );
        let expression =
            parse_expr("pg2sqlite_interval_sum(DISTINCT value) FILTER (WHERE value IS NOT NULL)");
        let reversed = expression.reverse_translate(&schema, &options).unwrap().to_string();
        assert!(reversed.starts_with("sum(DISTINCT "));
        assert!(reversed.contains("FILTER (WHERE value IS NOT NULL)"));
    }

    #[test]
    fn extract_restores_numeric_scale_and_postgres_field_names() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(TemporalFunctions::default()),
        );
        let expression = parse_expr("pg2sqlite_extract_time('second', value) = 1500000");
        let reversed = expression.reverse_translate(&schema, &options).unwrap().to_string();
        assert!(reversed.contains("= 1.500000"), "{reversed}");
        for (field, expected) in [("dow", "DOW"), ("doy", "DOY"), ("secs", "SECOND")] {
            let expression = parse_expr(&format!("pg2sqlite_extract_timestamp('{field}', value)"));
            let reversed = expression.reverse_translate(&schema, &options).unwrap().to_string();
            assert!(reversed.starts_with(&format!("EXTRACT({expected} FROM")), "{reversed}");
        }
    }
}
