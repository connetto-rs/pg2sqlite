//! Runtime calls for PostgreSQL interval arithmetic.

use pg2sqlite_runtime::TemporalFunction;
use sql_traits::structs::ParserDB;
use sql_values::postgres::IntervalTypmod;
use sqlparser::ast::{
    BinaryOperator, CastKind, DataType, Expr, UnaryOperator, Value, ValueWithSpan,
};

use super::keys::{self, IntervalSide};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        expression_type::{is_interval_typed, type_name},
        function_helpers::{number_literal, string_literal},
        shared_helpers::numeric_scale,
        temporal_arithmetic::{TemporalKind, temporal_kind_of},
        temporal_functions,
    },
    options::TranslationContext,
    traits::translator::TranslatorWithContext,
    warnings::WarningSink,
};

fn interval_operand(
    operand: &Expr,
    operator: &str,
    other: &Expr,
    is_left: bool,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    match keys::interval_side(operand, schema, options)? {
        IntervalSide::Null => Ok(Expr::Value(ValueWithSpan::from(Value::Null))),
        IntervalSide::BareLiteral(text) => {
            Ok(string_literal(&super::literal_text(&text, IntervalTypmod::default())?))
        }
        IntervalSide::TypedLiteral { text, typmod } => {
            Ok(string_literal(&super::literal_text(&text, typmod)?))
        }
        IntervalSide::Keyed(expression) => {
            expression.translate_with_warnings(schema, options, emit)
        }
        IntervalSide::Other => {
            if operator == "-"
                && !is_left
                && temporal_kind_of(operand, schema, options)
                    == Some(TemporalKind::Time { zoned: false })
            {
                return cast(operand, None, None, schema, options, emit);
            }
            let (left, right) = if is_left { (operand, other) } else { (other, operand) };
            Err(keys::operator_does_not_exist(left, operator, right, schema, options))
        }
    }
}

pub(crate) fn literal(
    operand: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    temporal_functions::functions(options, "interval literal")?;
    interval_operand(operand, "INTERVAL", operand, true, schema, options, emit)
}

pub(crate) fn negated(
    operand: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    let argument = interval_operand(operand, "-", operand, true, schema, options, emit)?;
    temporal_functions::call(
        options,
        TemporalFunction::IntervalNegate,
        vec![argument],
        "interval negation",
    )
}

fn shifted_target(
    operator: &BinaryOperator,
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Expr>, Error> {
    let subtract = matches!(operator, BinaryOperator::Minus);
    let (target, kind, interval, interval_is_left) =
        match (temporal_kind_of(left, schema, options), temporal_kind_of(right, schema, options)) {
            (Some(kind), None) => (left, kind, right, false),
            (None, Some(kind)) if !subtract => (right, kind, left, true),
            _ => return Ok(None),
        };
    let function = match (kind, subtract) {
        (TemporalKind::Date, false) => TemporalFunction::DatePlusInterval,
        (TemporalKind::Date, true) => TemporalFunction::DateMinusInterval,
        (TemporalKind::Timestamp, false) => TemporalFunction::TimestampPlusInterval,
        (TemporalKind::Timestamp, true) => TemporalFunction::TimestampMinusInterval,
        (TemporalKind::TimestampTz, false) => TemporalFunction::TimestampTzPlusInterval,
        (TemporalKind::TimestampTz, true) => TemporalFunction::TimestampTzMinusInterval,
        (TemporalKind::Time { zoned: false }, false) => TemporalFunction::TimePlusInterval,
        (TemporalKind::Time { zoned: false }, true) => TemporalFunction::TimeMinusInterval,
        (TemporalKind::Time { zoned: true }, false) => TemporalFunction::TimeTzPlusInterval,
        (TemporalKind::Time { zoned: true }, true) => TemporalFunction::TimeTzMinusInterval,
    };
    let operation = format!("{left} {operator} {right}");
    temporal_functions::functions(options, &operation)?;
    let interval = interval_operand(
        interval,
        if subtract { "-" } else { "+" },
        target,
        interval_is_left,
        schema,
        options,
        emit,
    )?;
    let mut arguments = vec![target.translate_with_warnings(schema, options, emit)?, interval];
    if kind == TemporalKind::TimestampTz {
        arguments.push(temporal_functions::session_zone(options, schema, &operation)?);
    }
    temporal_functions::call(options, function, arguments, &operation).map(Some)
}

pub(crate) fn binary(
    operator: &BinaryOperator,
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    if let Some(shifted) = shifted_target(operator, left, right, schema, options, emit)? {
        return Ok(shifted);
    }
    let subtract = matches!(operator, BinaryOperator::Minus);
    let spelling = if subtract { "-" } else { "+" };
    let arguments = vec![
        interval_operand(left, spelling, right, true, schema, options, emit)?,
        interval_operand(right, spelling, left, false, schema, options, emit)?,
    ];
    temporal_functions::call(
        options,
        if subtract { TemporalFunction::IntervalMinus } else { TemporalFunction::IntervalPlus },
        arguments,
        &format!("{left} {operator} {right}"),
    )
}

fn factor(
    operand: &Expr,
    interval: &Expr,
    operator: &str,
    is_left: bool,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    match operand {
        Expr::Nested(inner) => {
            return factor(inner, interval, operator, is_left, schema, options, emit);
        }
        Expr::UnaryOp { op: sign @ (UnaryOperator::Minus | UnaryOperator::Plus), expr } => {
            return Ok(Expr::UnaryOp {
                op: *sign,
                expr: Box::new(factor(expr, interval, operator, is_left, schema, options, emit)?),
            });
        }
        Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. })
            if !digits.replace('_', "").parse::<f64>().is_ok_and(f64::is_finite) =>
        {
            return Err(Error::forward_refusal(format!(
                "{digits} is out of range for double precision"
            )));
        }
        _ => {}
    }
    let numeric = type_name(operand, schema, options).ok().flatten().is_some_and(|name| {
        let lowered = name.to_ascii_lowercase();
        ["int", "smallint", "bigint", "numeric", "decimal", "real", "double", "float"]
            .iter()
            .any(|family| lowered.starts_with(family))
    });
    let literal = matches!(
        operand,
        Expr::Value(ValueWithSpan {
            value: Value::Number(..) | Value::Null | Value::Placeholder(_),
            ..
        })
    );
    if !numeric && !literal {
        let (left, right) = if is_left { (operand, interval) } else { (interval, operand) };
        return Err(keys::operator_does_not_exist(left, operator, right, schema, options));
    }
    let translated = operand.translate_with_warnings(schema, options, emit)?;
    let real = Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(translated),
        data_type: DataType::Real,
        format: None,
    };
    let scale = numeric_scale(operand, schema, options)?.unwrap_or(0);
    if scale == 0 {
        return Ok(real);
    }
    Ok(Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(real),
        op: BinaryOperator::Divide,
        right: Box::new(number_literal(&format!("1e{scale}"))),
    })))
}

pub(crate) fn scaled_binary(
    operator: &BinaryOperator,
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    let divide = matches!(operator, BinaryOperator::Divide);
    let spelling = if divide { "/" } else { "*" };
    let left_interval = is_interval_typed(left, schema, options);
    let right_interval = is_interval_typed(right, schema, options);
    if right_interval && (left_interval || divide) {
        return Err(keys::operator_does_not_exist(left, spelling, right, schema, options));
    }
    let (interval, multiplier, interval_is_left) =
        if left_interval { (left, right, true) } else { (right, left, false) };
    let arguments = vec![
        interval_operand(interval, spelling, multiplier, interval_is_left, schema, options, emit)?,
        factor(multiplier, interval, spelling, !interval_is_left, schema, options, emit)?,
    ];
    temporal_functions::call(
        options,
        if divide { TemporalFunction::IntervalDivide } else { TemporalFunction::IntervalMultiply },
        arguments,
        &format!("{left} {operator} {right}"),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Aggregate {
    Sum,
    Avg,
}

pub(crate) fn aggregate(
    kind: Aggregate,
    argument: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    let translated = argument.translate_with_warnings(schema, options, emit)?;
    temporal_functions::call(
        options,
        match kind {
            Aggregate::Sum => TemporalFunction::IntervalSum,
            Aggregate::Avg => TemporalFunction::IntervalAverage,
        },
        vec![translated],
        "interval aggregate",
    )
}

pub(crate) fn cast(
    operand: &Expr,
    fields: Option<&sqlparser::ast::IntervalFields>,
    precision: Option<u64>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    super::data_type_typmod(fields, precision)?;
    temporal_functions::functions(options, "interval cast")?;
    let interval = is_interval_typed(operand, schema, options);
    let time =
        temporal_kind_of(operand, schema, options) == Some(TemporalKind::Time { zoned: false });
    let source = type_name(operand, schema, options)?;
    if !interval
        && !time
        && source.as_deref().is_some_and(|name| {
            let lowered = name.to_ascii_lowercase();
            !["unknown", "text", "varchar", "character", "char"]
                .iter()
                .any(|family| lowered.starts_with(family))
        })
    {
        return Err(Error::forward_refusal(format!("Cannot cast {operand} to interval.")));
    }
    let translated = operand.translate_with_warnings(schema, options, emit)?;
    let value = if interval {
        translated
    } else if time {
        temporal_functions::call(
            options,
            TemporalFunction::TimeToInterval,
            vec![translated],
            "time to interval cast",
        )?
    } else {
        temporal_functions::call(
            options,
            TemporalFunction::Input(pg2sqlite_runtime::TemporalKind::Interval),
            vec![
                translated,
                temporal_functions::session_zone(options, schema, "interval input")?,
                crate::impls::zone_clock::transaction_instant(),
            ],
            "interval input",
        )?
    };
    if fields.is_none() && precision.is_none() {
        return Ok(value);
    }
    let null = || Expr::Value(ValueWithSpan::from(Value::Null));
    temporal_functions::call(
        options,
        TemporalFunction::IntervalScale,
        vec![
            value,
            fields.map_or_else(null, |fields| string_literal(&fields.to_string())),
            precision.map_or_else(null, |precision| number_literal(&precision.to_string())),
        ],
        "interval typmod",
    )
}

#[cfg(test)]
mod tests {
    use crate::{
        impls::test_fixtures::{empty_schema, parse_expr},
        options::TranslationContext,
        prelude::Pg2SqliteOptions,
        traits::translator::TranslatorWithContext,
    };

    #[test]
    fn nullable_temporal_fields_are_evaluated_at_runtime() {
        let schema = sql_traits::structs::ParserDB::parse::<sqlparser::dialect::PostgreSqlDialect>(
            "CREATE ROLE app;",
        )
        .unwrap();
        let deployment = sql_traits::structs::Deployment::new("app", ["app"], [])
            .unwrap()
            .with_default_time_zone("UTC")
            .unwrap();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_temporal_functions(pg2sqlite_runtime::TemporalFunctions::default())
                .with_deployment(deployment),
        );
        for expression in [
            "extract(hour from NULL::date)",
            "extract(fortnight from NULL::timestamp)",
            "date_trunc('fortnight', NULL::timestamp)",
            "date_trunc('timezone', NULL::timestamp)",
            "(NULL::interval)::time",
            "(interval '25 hours')::time(2)",
            "(NULL::time) AT TIME ZONE 'UTC'",
            "time '12:00' AT TIME ZONE 'UTC'",
        ] {
            parse_expr(expression)
                .translate_with_warnings(&schema, &options, &mut |_| {})
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
        }
    }

    #[test]
    fn numeric_epoch_coercions_preserve_minor_units_and_precedence() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_temporal_functions(pg2sqlite_runtime::TemporalFunctions::default()),
        );
        for (expression, expected) in [
            ("'1709647629.123456'::numeric(18,6)", "CAST(1709647629123456 AS INTEGER)"),
            ("NULL::numeric(18,6)", "NULL"),
        ] {
            let translated = parse_expr(expression)
                .translate_with_warnings(&schema, &options, &mut |_| {})
                .unwrap();
            assert_eq!(translated.to_string(), expected);
        }
        let translated =
            parse_expr("to_timestamp(extract(epoch from timestamp '2024-01-01') + 0.000001)")
                .translate_with_warnings(&schema, &options, &mut |_| {})
                .unwrap();
        assert!(translated.to_string().contains("+ 0000001) / 1000000.0"));
    }

    #[test]
    fn date_coercions_require_the_deployment_zone() {
        let schema = sql_traits::structs::ParserDB::parse::<sqlparser::dialect::PostgreSqlDialect>(
            "CREATE ROLE app;",
        )
        .unwrap();
        let unconfigured = TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_temporal_functions(pg2sqlite_runtime::TemporalFunctions::default()),
        );
        let deployment = sql_traits::structs::Deployment::new("app", ["app"], [])
            .unwrap()
            .with_default_time_zone("Europe/Rome")
            .unwrap();
        let configured = TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_temporal_functions(pg2sqlite_runtime::TemporalFunctions::default())
                .with_deployment(deployment),
        );
        for expression in [
            "date_trunc('day', date '2024-01-01')",
            "date '2024-01-01' AT TIME ZONE 'UTC'",
            "date '2024-01-01' AT TIME ZONE interval '-08:00'",
        ] {
            let expression = parse_expr(expression);
            assert!(
                expression.translate_with_warnings(&schema, &unconfigured, &mut |_| {}).is_err()
            );
            let translated =
                expression.translate_with_warnings(&schema, &configured, &mut |_| {}).unwrap();
            assert!(translated.to_string().contains("pg2sqlite_date_timestamptz"));
            assert!(translated.to_string().contains("'Europe/Rome'"));
        }
    }

    #[test]
    fn epoch_text_arguments_use_strict_float_parsing() {
        let schema = empty_schema();
        let functions = pg2sqlite_runtime::TemporalFunctions::default();
        let name = functions.name(pg2sqlite_runtime::TemporalFunction::EpochTextToTimestampTz);
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_temporal_functions(functions),
        );
        for expression in [
            "to_timestamp('Infinity')",
            "to_timestamp(' -Inf ')",
            "to_timestamp('NaN')",
            "to_timestamp('invalid')",
            "to_timestamp(('Infinity'::text)::double precision)",
            "to_timestamp((' -Inf '::varchar)::double precision)",
        ] {
            let translated = parse_expr(expression)
                .translate_with_warnings(&schema, &options, &mut |_| {})
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
            assert!(translated.to_string().starts_with(&name), "{expression}: {translated}");
        }
        assert!(
            parse_expr("to_timestamp('Infinity'::text)")
                .translate_with_warnings(&schema, &options, &mut |_| {})
                .is_err(),
        );
    }

    #[test]
    fn timetz_comparisons_use_postgres_ordering() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_temporal_functions(pg2sqlite_runtime::TemporalFunctions::default()),
        );
        for operator in ["=", "<>", "<", "<=", ">", ">="] {
            let expression =
                parse_expr(&format!("timetz '14:07:09.123456+02' {operator} timetz '00:00:00-14'"));
            let translated =
                expression.translate_with_warnings(&schema, &options, &mut |_| {}).unwrap();
            assert!(translated.to_string().starts_with("pg2sqlite_timetz_cmp("));
            assert!(translated.to_string().ends_with(&format!(" {operator} 0")));
        }
    }

    #[test]
    fn temporal_float_text_uses_postgres_output() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_temporal_functions(pg2sqlite_runtime::TemporalFunctions::default()),
        );
        let translated = parse_expr("date_part('microseconds', time '14:07:09.123456')::text")
            .translate_with_warnings(&schema, &options, &mut |_| {})
            .unwrap();
        assert_eq!(
            translated.to_string(),
            "pg2sqlite_float8_out(pg2sqlite_date_part_time('microseconds', '14:07:09.123456'))",
        );
    }

    #[test]
    fn interval_runtime_routes_preserve_implicit_casts() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_temporal_functions(pg2sqlite_runtime::TemporalFunctions::default()),
        );
        for (expression, expected) in [
            ("interval '1 day'", "'1 day'"),
            ("interval '1:02:03' hour to minute", "'01:02:00'"),
            ("time '12:34:56'::interval", "pg2sqlite_time_interval"),
            ("interval '1 day' - time '12:34:56'", "pg2sqlite_time_interval"),
            ("date_trunc('hour', time '12:34:56')", "pg2sqlite_interval_trunc"),
            ("justify_hours(interval '25 hours')", "pg2sqlite_interval_justify_hours"),
            ("justify_days(interval '31 days')", "pg2sqlite_interval_justify_days"),
            ("justify_interval(interval '31 days')", "pg2sqlite_interval_justify_interval"),
            ("to_char(interval '25 hours', 'HH24')", "pg2sqlite_interval_to_char"),
            ("timestamp '2024-01-01' AT TIME ZONE interval '-08:00'", "pg2sqlite_timestamp_izone"),
        ] {
            let translated = parse_expr(expression)
                .translate_with_warnings(&schema, &options, &mut |_| {})
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
            assert!(translated.to_string().contains(expected), "{expression}: {translated}");
        }
    }

    #[test]
    fn arithmetic_requires_temporal_functions_even_for_literals_and_nulls() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(Pg2SqliteOptions::default());
        for expression in [
            "interval '1 day'",
            "time '12:34:56'::interval",
            "date_trunc('hour', time '12:34:56')",
            "justify_hours(interval '25 hours')",
            "justify_days(interval '31 days')",
            "justify_interval(interval '31 days')",
            "to_char(interval '25 hours', 'HH24')",
            "timestamp '2024-01-01' AT TIME ZONE interval '-08:00'",
            "interval '1 day' + interval '2 days'",
            "interval '1 day' - interval '2 days'",
            "-interval '1 day'",
            "interval '1 day' * 2",
            "2 * interval '1 day'",
            "interval '1 day' / 2",
            "interval '1 day' + NULL",
            "interval '1 day' * NULL",
            "date '2024-01-01' + interval '1 day'",
            "interval '1 day' + date '2024-01-01'",
            "time '12:00' - interval '1 hour'",
            "sum(interval '1 day')",
            "avg(interval '1 day')",
            "CAST(CAST('1 day' AS text) AS interval)",
            "CAST(interval '1 day' AS interval hour to minute)",
            "CAST(NULL AS interval)",
        ] {
            let error = parse_expr(expression)
                .translate_with_warnings(&schema, &options, &mut |_| {})
                .unwrap_err();
            assert!(error.to_string().contains("with_temporal_functions"), "{expression}: {error}");
        }
    }

    #[test]
    fn interval_factors_distinguish_overflow_from_explicit_infinity() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_temporal_functions(pg2sqlite_runtime::TemporalFunctions::default()),
        );
        for expression in
            ["interval '1 day' * 1e400", "interval '1 day' * -1e400", "interval '1 day' / 1e400"]
        {
            let error = parse_expr(expression)
                .translate_with_warnings(&schema, &options, &mut |_| {})
                .unwrap_err();
            assert!(error.to_string().contains("out of range"), "{expression}: {error}");
        }
        for expression in [
            "interval '1 day' * 'Infinity'::double precision",
            "interval '1 day' * (1::integer + 1)",
        ] {
            parse_expr(expression)
                .translate_with_warnings(&schema, &options, &mut |_| {})
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
        }
    }

    #[test]
    fn interval_scaling_rejects_invalid_operand_types_and_direction() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_temporal_functions(pg2sqlite_runtime::TemporalFunctions::default()),
        );
        for expression in [
            "2 / interval '1 day'",
            "interval '1 day' * interval '2 days'",
            "interval '1 day' * true",
        ] {
            assert!(
                parse_expr(expression)
                    .translate_with_warnings(&schema, &options, &mut |_| {})
                    .is_err(),
                "{expression}"
            );
        }
    }
}
