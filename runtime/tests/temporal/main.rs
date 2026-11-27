//! Every SQL function of `TemporalFunctions`, registered on a plain SQLite
//! connection, answers for the replica's stored text what PostgreSQL 18
//! answers.
//!
//! Each call passes the stored text of PostgreSQL's operands, so a
//! `timestamptz` is the UTC wall clock with six fractional digits followed by
//! `+00:00` and a `timetz` carries a `±HH:MM` offset. Each answer is
//! PostgreSQL 18.6's in the same stored form, an `EXTRACT` as minor units at
//! its scale, and a result outside the years 1 to 9999 raises `22008`.

mod constructors;
mod conversions;
mod date;
mod date_part;
mod extract;
mod input;
mod interval;
mod time;
mod timestamp;
mod timestamptz;
mod to_char;
mod trunc;
mod zones;

use std::collections::HashSet;

use diesel::{
    Connection, QueryResult, QueryableByName, RunQueryDsl,
    sql_types::{BigInt, Double, Nullable, Text},
    sqlite::{Sqlite, SqliteConnection},
};
use pg2sqlite_runtime::{TemporalFunction as Function, TemporalFunctions, TemporalKind as Kind};

/// The replica's text of the instant the clock-reading functions are given,
/// in Rome's and St. John's winter.
const NOW: &str = "2026-11-27 12:00:00.000000+00:00";

/// An instant in Rome's and St. John's summer.
const SUMMER: &str = "2026-07-01 12:00:00.000000+00:00";

/// One argument of a call.
#[derive(Debug, Clone, Copy)]
enum Argument {
    /// Bound as `TEXT`.
    Text(&'static str),
    /// Bound as `INTEGER`.
    Integer(i64),
    /// Bound as `REAL`.
    Real(f64),
    /// Bound as SQL NULL.
    Null,
}

/// A `TEXT` argument.
const fn text(value: &'static str) -> Argument {
    Argument::Text(value)
}

/// An `INTEGER` argument.
const fn integer(value: i64) -> Argument {
    Argument::Integer(value)
}

/// A `REAL` argument.
const fn real(value: f64) -> Argument {
    Argument::Real(value)
}

/// What a call has to answer.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// This `TEXT`.
    Text(&'static str),
    /// This `INTEGER`.
    Integer(i64),
    /// This `REAL`, bit for bit.
    Real(f64),
    /// SQL NULL.
    Null,
    /// An error, PostgreSQL's named for the failure message.
    Raises(&'static str),
}

/// Answers this `TEXT`.
const fn gives_text(value: &'static str) -> Expected {
    Expected::Text(value)
}

/// Answers this `INTEGER`.
const fn gives_integer(value: i64) -> Expected {
    Expected::Integer(value)
}

/// Answers this `REAL`.
const fn gives_real(value: f64) -> Expected {
    Expected::Real(value)
}

/// Answers SQL NULL.
const GIVES_NULL: Expected = Expected::Null;

/// Raises an error where PostgreSQL raised `postgres_error`.
const fn raises(postgres_error: &'static str) -> Expected {
    Expected::Raises(postgres_error)
}

impl Expected {
    /// What PostgreSQL answered, for the failure message.
    fn describe(self) -> String {
        match self {
            Self::Raises(postgres_error) => format!("raised {postgres_error}"),
            answer => format!("{answer:?}"),
        }
    }
}

/// One call of a registered function and what it has to answer.
#[derive(Debug, Clone, Copy)]
struct Call {
    function: Function,
    arguments: &'static [Argument],
    expected: Expected,
}

/// A [`Call`].
const fn call(function: Function, arguments: &'static [Argument], expected: Expected) -> Call {
    Call { function, arguments, expected }
}

/// The value a call answered, with its SQLite storage class.
#[derive(Debug, QueryableByName)]
struct Answer {
    /// `typeof` of the value.
    #[diesel(sql_type = Text)]
    kind: String,
    /// The value cast to text.
    #[diesel(sql_type = Nullable<Text>)]
    text: Option<String>,
    /// The value when it is a `REAL`.
    #[diesel(sql_type = Nullable<Double>)]
    real: Option<f64>,
}

/// A connection with every temporal function registered under
/// `functions`' names.
fn connection_with(functions: &TemporalFunctions) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    functions.register(&mut connection).expect("register the temporal functions");
    connection
}

/// What `query`, whose one value is `x`, answers with `arguments` bound in
/// order.
fn answer(
    connection: &mut SqliteConnection,
    query: &str,
    arguments: &[Argument],
) -> QueryResult<Answer> {
    // The function name comes from the registered prefix, which no typed DSL
    // names.
    let mut bound = diesel::sql_query(format!(
        "SELECT typeof(x) AS kind, CAST(x AS TEXT) AS text, \
         CASE WHEN typeof(x) = 'real' THEN x END AS real FROM ({query})"
    ))
    .into_boxed::<Sqlite>();
    for argument in arguments {
        bound = match *argument {
            Argument::Text(value) => bound.bind::<Text, _>(value),
            Argument::Integer(value) => bound.bind::<BigInt, _>(value),
            Argument::Real(value) => bound.bind::<Double, _>(value),
            Argument::Null => bound.bind::<Nullable<Text>, _>(None::<&str>),
        };
    }
    bound.get_result(connection)
}

/// What `function` answers for `arguments`.
fn call_answer(
    connection: &mut SqliteConnection,
    function: Function,
    arguments: &[Argument],
) -> QueryResult<Answer> {
    let placeholders = vec!["?"; arguments.len()].join(", ");
    let name = TemporalFunctions::default().name(function);
    answer(connection, &format!("SELECT {name}({placeholders}) AS x"), arguments)
}

/// Whether `outcome` is what `expected` says.
fn agrees(expected: Expected, outcome: &QueryResult<Answer>) -> bool {
    match (expected, outcome) {
        (Expected::Text(value), Ok(answer)) => {
            answer.kind == "text" && answer.text.as_deref() == Some(value)
        }
        (Expected::Integer(value), Ok(answer)) => {
            answer.kind == "integer" && answer.text == Some(value.to_string())
        }
        (Expected::Real(value), Ok(answer)) => {
            answer.kind == "real" && answer.real.map(f64::to_bits) == Some(value.to_bits())
        }
        (Expected::Null, Ok(answer)) => answer.kind == "null",
        (Expected::Raises(_), Err(_)) => true,
        _ => false,
    }
}

/// Every call of `calls` answers what PostgreSQL answered.
fn assert_calls(calls: &[Call]) {
    let mut connection = connection_with(&TemporalFunctions::default());
    let disagreements: Vec<String> = calls
        .iter()
        .filter_map(|call| {
            let outcome = call_answer(&mut connection, call.function, call.arguments);
            (!agrees(call.expected, &outcome)).then(|| {
                format!(
                    "{:?}{:?}\n  PostgreSQL {}\n  replica    {outcome:?}",
                    call.function,
                    call.arguments,
                    call.expected.describe()
                )
            })
        })
        .collect();
    assert!(
        disagreements.is_empty(),
        "{} of {} calls disagree with PostgreSQL:\n{}",
        disagreements.len(),
        calls.len(),
        disagreements.join("\n")
    );
}

/// Every group of calls.
const GROUPS: [&[Call]; 14] = [
    constructors::CALLS,
    conversions::CALLS,
    date::CALLS,
    date_part::CALLS,
    extract::CALLS,
    input::CALLS,
    interval::CALLS,
    time::CALLS,
    timestamp::CALLS,
    timestamptz::CALLS,
    to_char::CALLS,
    trunc::CALLS,
    zones::CALLS,
    SEASONAL,
];

/// See [`date`].
#[test]
fn date_functions_agree_with_postgres() {
    assert_calls(date::CALLS);
}

/// See [`timestamp`].
#[test]
fn timestamp_functions_agree_with_postgres() {
    assert_calls(timestamp::CALLS);
}

/// See [`timestamptz`].
#[test]
fn timestamptz_functions_agree_with_postgres() {
    assert_calls(timestamptz::CALLS);
}

/// See [`time`].
#[test]
fn time_functions_agree_with_postgres() {
    assert_calls(time::CALLS);
}

/// See [`interval`].
#[test]
fn interval_functions_agree_with_postgres() {
    assert_calls(interval::CALLS);
}

/// See [`conversions`].
#[test]
fn conversions_agree_with_postgres() {
    assert_calls(conversions::CALLS);
}

/// See [`input`].
#[test]
fn input_functions_agree_with_postgres() {
    assert_calls(input::CALLS);
}

/// See [`zones`].
#[test]
fn zone_functions_agree_with_postgres() {
    assert_calls(zones::CALLS);
}

/// See [`extract`].
#[test]
fn extract_agrees_with_postgres() {
    assert_calls(extract::CALLS);
}

/// See [`date_part`].
#[test]
fn date_part_agrees_with_postgres() {
    assert_calls(date_part::CALLS);
}

/// See [`trunc`].
#[test]
fn trunc_agrees_with_postgres() {
    assert_calls(trunc::CALLS);
}

/// See [`to_char`].
#[test]
fn to_char_agrees_with_postgres() {
    assert_calls(to_char::CALLS);
}

/// See [`constructors`].
#[test]
fn constructors_agree_with_postgres() {
    assert_calls(constructors::CALLS);
}

/// A NULL in any argument position answers NULL, for every function.
#[test]
fn a_null_argument_answers_null() {
    let mut connection = connection_with(&TemporalFunctions::default());
    let mut seen = HashSet::new();
    let mut disagreements = Vec::new();
    for call in GROUPS.into_iter().flatten().filter(|call| seen.insert(call.function)) {
        for position in 0..call.arguments.len() {
            let mut arguments = call.arguments.to_vec();
            arguments[position] = Argument::Null;
            let outcome = call_answer(&mut connection, call.function, &arguments);
            if !agrees(GIVES_NULL, &outcome) {
                disagreements.push(format!("{:?}{arguments:?}: {outcome:?}", call.function));
            }
        }
    }
    assert!(disagreements.is_empty(), "{}", disagreements.join("\n"));
}

/// Calls of `time_timetz` and `timetz_zone`, which take the offset the zone
/// has at `now` as PostgreSQL takes it at the current date. PostgreSQL
/// answered the winter rows on 2026-11-27, and the summer rows are Rome's
/// `+02` and St. John's `-02:30`.
const SEASONAL: &[Call] = &[
    call(
        Function::TimeToTimeTz,
        &[text("14:07:09.123456"), text("UTC"), text(NOW)],
        gives_text("14:07:09.123456+00:00"),
    ),
    call(
        Function::TimeToTimeTz,
        &[text("14:07:09.123456"), text("Europe/Rome"), text(NOW)],
        gives_text("14:07:09.123456+01:00"),
    ),
    call(
        Function::TimeToTimeTz,
        &[text("14:07:09.123456"), text("Europe/Rome"), text(SUMMER)],
        gives_text("14:07:09.123456+02:00"),
    ),
    call(
        Function::TimeToTimeTz,
        &[text("24:00:00"), text("America/St_Johns"), text(NOW)],
        gives_text("24:00:00-03:30"),
    ),
    call(
        Function::TimeToTimeTz,
        &[text("24:00:00"), text("America/St_Johns"), text(SUMMER)],
        gives_text("24:00:00-02:30"),
    ),
    call(
        Function::TimeTzAtZone,
        &[text("UTC"), text("23:59:59.999999-03:30"), text(NOW)],
        gives_text("03:29:59.999999+00:00"),
    ),
    call(
        Function::TimeTzAtZone,
        &[text("Europe/Rome"), text("14:07:09.123456+02:00"), text(NOW)],
        gives_text("13:07:09.123456+01:00"),
    ),
    call(
        Function::TimeTzAtZone,
        &[text("Europe/Rome"), text("14:07:09.123456+02:00"), text(SUMMER)],
        gives_text("14:07:09.123456+02:00"),
    ),
    call(
        Function::TimeTzAtZone,
        &[text("America/St_Johns"), text("23:59:59.999999-03:30"), text(NOW)],
        gives_text("23:59:59.999999-03:30"),
    ),
    call(
        Function::TimeTzAtZone,
        &[text("America/St_Johns"), text("14:07:09.123456+02:00"), text(SUMMER)],
        gives_text("09:37:09.123456-02:30"),
    ),
    call(
        Function::TimeTzAtZone,
        &[text("+05:30"), text("14:07:09.123456+02:00"), text(NOW)],
        gives_text("06:37:09.123456-05:30"),
    ),
];

/// See [`SEASONAL`].
#[test]
fn the_offset_at_now_follows_the_season() {
    assert_calls(SEASONAL);
}

/// Rows an interval aggregate runs over, and PostgreSQL's `sum` and `avg` of
/// them.
struct Aggregate {
    values: &'static [Option<&'static str>],
    sum: Option<&'static str>,
    average: Option<&'static str>,
}

/// The rows of [`the_interval_aggregates_agree_with_postgres`].
const AGGREGATES: &[Aggregate] = &[
    Aggregate {
        values: &[
            Some("1 day 02:03:04.567891"),
            Some("-00:00:00.000001"),
            Some("1 mon -1 days +00:00:00.5"),
        ],
        sum: Some("1 mon 02:03:05.06789"),
        average: Some("10 days 00:41:01.689297"),
    },
    Aggregate {
        values: &[Some("1 mon"), Some("00:00:00")],
        sum: Some("1 mon"),
        average: Some("15 days"),
    },
    Aggregate {
        values: &[Some("1 mon"), Some("00:00:00"), Some("00:00:00")],
        sum: Some("1 mon"),
        average: Some("10 days"),
    },
    Aggregate {
        values: &[
            Some("1 year 2 mons 3 days 04:05:06.789012"),
            None,
            Some("-2 years -3 days -04:05:06.000007"),
        ],
        sum: Some("-10 mons +00:00:00.789005"),
        average: Some("-5 mons +00:00:00.394502"),
    },
    Aggregate {
        values: &[Some("00:00:00.000001"), Some("00:00:00.000001"), Some("00:00:00.000001")],
        sum: Some("00:00:00.000003"),
        average: Some("00:00:00.000001"),
    },
    Aggregate { values: &[None], sum: None, average: None },
    Aggregate { values: &[], sum: None, average: None },
];

/// `interval_sum` and `interval_avg` answer PostgreSQL's `sum` and `avg`,
/// skipping NULL rows and answering NULL over no rows.
#[test]
fn the_interval_aggregates_agree_with_postgres() {
    let mut connection = connection_with(&TemporalFunctions::default());
    let mut disagreements = Vec::new();
    for Aggregate { values, sum, average } in AGGREGATES {
        let rows = if values.is_empty() {
            "SELECT NULL AS v WHERE 0".to_owned()
        } else {
            vec!["SELECT ? AS v"; values.len()].join(" UNION ALL ")
        };
        let arguments: Vec<Argument> =
            values.iter().map(|value| value.map_or(Argument::Null, Argument::Text)).collect();
        for (function, expected) in
            [(Function::IntervalSum, sum), (Function::IntervalAverage, average)]
        {
            let name = TemporalFunctions::default().name(function);
            let outcome = answer(
                &mut connection,
                &format!("SELECT {name}(v) AS x FROM ({rows})"),
                &arguments,
            );
            let expected = expected.map_or(GIVES_NULL, Expected::Text);
            if !agrees(expected, &outcome) {
                disagreements.push(format!(
                    "{function:?} over {values:?}\n  PostgreSQL {}\n  replica    {outcome:?}",
                    expected.describe()
                ));
            }
        }
    }
    assert!(disagreements.is_empty(), "{}", disagreements.join("\n"));
}

/// The functions are registered under the prefix given, and not under the
/// default one.
#[test]
fn the_prefix_names_every_function() {
    let functions = TemporalFunctions::new("pg_");
    let mut connection = connection_with(&functions);
    let arguments = [text("2024-01-31 12:00:00"), text("1 mon")];
    let prefixed = answer(
        &mut connection,
        &format!("SELECT {}(?, ?) AS x", functions.name(Function::TimestampPlusInterval)),
        &arguments,
    );
    assert!(agrees(gives_text("2024-02-29 12:00:00"), &prefixed), "{prefixed:?}");
    assert!(
        call_answer(&mut connection, Function::TimestampPlusInterval, &arguments).is_err(),
        "the default name stays unregistered"
    );
}

#[test]
fn computed_minimum_interval_remains_an_operand() {
    use pg2sqlite_runtime::{
        TemporalArgument::{Real, Text},
        TemporalError, TemporalValue,
    };
    let Some(TemporalValue::Text(minimum)) = Function::IntervalMultiply
        .evaluate(&[Text("-1281023894:00:27.387904"), Real(2.0)])
        .unwrap()
    else {
        panic!("an interval multiplication returns text");
    };
    assert_eq!(
        Function::TimeMinusInterval.evaluate(&[Text("12:00:00.001"), Text(&minimum)]).unwrap(),
        Some(TemporalValue::Text("07:59:05.225192".into())),
    );
    assert!(matches!(
        Function::IntervalNegate.evaluate(&[Text(&minimum)]),
        Err(TemporalError::Interval(sql_values::postgres::IntervalError::OutOfRange)),
    ));
    assert!(
        Function::Input(Kind::Interval)
            .evaluate(&[Text(&minimum), Text("UTC"), Text(NOW),])
            .is_err()
    );
}
