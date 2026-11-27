//! Every temporal operation, at microsecond resolution, answers on the replica
//! what PostgreSQL 18 answers, in three session zones.
//!
//! Each case is one operand, one expression over it and PostgreSQL's answer
//! read as text. The operand reaches the expression twice, once as a literal
//! in the query and once read from a stored row, so the answer computed while
//! translating and the one computed while SQLite runs are both held to it.
//! The emitted query then goes through the reverse translation and forward
//! again, and has to answer the same.
//!
//! Every answer below was read off PostgreSQL 18.6. A refused case names the
//! error PostgreSQL raised, and the replica has to refuse it too, while
//! translating or while running. The replica holds only the years 1 to 9999,
//! so an operand or a result outside them is refused with `22008` where
//! PostgreSQL prints a BC or five-digit year. A `timetz` whose offset
//! PostgreSQL reads off the current date is left out, since its answer
//! changes with the season.

mod aggregates;
mod casts;
mod comparisons;
mod constructors;
mod date_arithmetic;
mod date_part;
mod date_trunc;
mod extract;
mod interval_arithmetic;
mod null_operands;
mod time_arithmetic;
mod time_zones;
mod timestamp_arithmetic;
mod timestamptz_arithmetic;
mod timetz_arithmetic;
mod to_char;
mod to_timestamp;

use diesel::prelude::*;
use diesel_dynamic_schema::dynamic_value::{DynamicRow, NamedField};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, Script};

use crate::helpers::{
    deployment::utc_script,
    run_translated::{TextCell, first_column_text},
    temporal,
};

/// The session zones every case runs in. Rome moves its offset and kept local
/// mean time until 1893, St. John's keeps a half-hour offset, and UTC has
/// none.
const ZONES: [&str; 3] = ["UTC", "Europe/Rome", "America/St_Johns"];

/// The operand types, as PostgreSQL spells them in a cast and a column.
const TIMESTAMP: &str = "timestamp";
/// See [`TIMESTAMP`].
const TIMESTAMPTZ: &str = "timestamptz";
/// See [`TIMESTAMP`].
const TIME: &str = "time";
/// See [`TIMESTAMP`].
const TIMETZ: &str = "timetz";
/// See [`TIMESTAMP`].
const DATE: &str = "date";
/// See [`TIMESTAMP`].
const INTERVAL: &str = "interval";
/// See [`TIMESTAMP`].
const TEXT: &str = "text";
/// See [`TIMESTAMP`].
const DOUBLE: &str = "double precision";
/// See [`TIMESTAMP`].
const NUMERIC: &str = "numeric(18,6)";
/// See [`TIMESTAMP`].
const INTEGER: &str = "integer";

/// What PostgreSQL answered for a case in one zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// The value, cast to text.
    Text(&'static str),
    /// SQL NULL.
    Null,
    /// PostgreSQL raised this error.
    Refused(&'static str),
}

/// A case's answer in each session zone.
#[derive(Debug, Clone, Copy)]
enum Answers {
    /// The same answer in every zone.
    Every(Answer),
    /// One answer for each of [`ZONES`], in order.
    Zoned([Answer; 3]),
}

impl Answers {
    /// The answer in the zone at `index` of [`ZONES`].
    fn in_zone(self, index: usize) -> Answer {
        match self {
            Self::Every(answer) => answer,
            Self::Zoned(answers) => answers[index],
        }
    }
}

/// SQL NULL.
const NULL: Answer = Answer::Null;

/// A value PostgreSQL printed.
const fn text(value: &'static str) -> Answer {
    Answer::Text(value)
}

/// An error PostgreSQL raised.
const fn refused(postgres_error: &'static str) -> Answer {
    Answer::Refused(postgres_error)
}

/// One operand of type `kind` written as `value`, or NULL, an expression
/// reading it as `{v}`, and what PostgreSQL answered.
#[derive(Debug, Clone, Copy)]
struct Case {
    kind: &'static str,
    value: Option<&'static str>,
    expression: &'static str,
    answers: Answers,
}

/// A [`Case`] whose operand is `value`.
const fn case(
    kind: &'static str,
    value: &'static str,
    expression: &'static str,
    answers: Answers,
) -> Case {
    Case { kind, value: Some(value), expression, answers }
}

/// A [`Case`] whose operand is NULL.
const fn null_operand(kind: &'static str, expression: &'static str, answers: Answers) -> Case {
    Case { kind, value: None, expression, answers }
}

/// How the operand reaches the expression.
#[derive(Debug, Clone, Copy)]
enum Operand {
    /// Written into the query as a cast literal.
    Literal,
    /// Read from the one row of `samples`.
    Stored,
}

impl Case {
    /// The operand as a cast literal.
    fn literal(&self) -> String {
        match self.value {
            Some(value) => format!("CAST('{value}' AS {})", self.kind),
            None => format!("CAST(NULL AS {})", self.kind),
        }
    }

    /// The table holding the operand, and its one row.
    fn setup(&self) -> String {
        format!(
            "CREATE TABLE samples (v {kind});\nINSERT INTO samples (v) VALUES ({literal});\n",
            kind = self.kind,
            literal = self.literal()
        )
    }

    /// What runs before the query, the table only when the operand is
    /// stored.
    fn prefix(&self, operand: Operand) -> String {
        match operand {
            Operand::Literal => String::new(),
            Operand::Stored => self.setup(),
        }
    }

    /// The script reading the expression as text, with the operand reaching
    /// it as `operand` says.
    fn script(&self, operand: Operand) -> String {
        let query = match operand {
            Operand::Literal => {
                format!(
                    "SELECT CAST(({}) AS TEXT);",
                    self.expression.replace("{v}", &self.literal())
                )
            }
            Operand::Stored => {
                format!(
                    "SELECT CAST(({}) AS TEXT) FROM samples;",
                    self.expression.replace("{v}", "v")
                )
            }
        };
        format!("{}{query}", self.prefix(operand))
    }

    /// How the case went in `zone` with `operand`, for the failure message.
    fn describe(&self, zone: &str, operand: Operand, answer: Answer, outcome: &Outcome) -> String {
        format!(
            "[{zone}] {kind} {value:?} {operand:?} in {expression}\n  PostgreSQL {answer:?}\n  replica    {outcome:?}",
            kind = self.kind,
            value = self.value,
            expression = self.expression,
        )
    }
}

/// What the replica answered, or why it gave no answer.
type Outcome = Result<Option<String>, String>;

/// The script pg2sqlite emits for `pg`, led by the role the request logs in
/// as, under the temporal functions of `zone`.
fn translate(pg: &str, zone: &str) -> Result<Script<String>, String> {
    Pg2Sqlite::default()
        .sql(&utc_script(pg))
        .and_then(|source| source.translate_to_sql(&temporal::options(zone)))
        .map_err(|error| format!("translation refused it: {error}"))
}

/// Runs `script` on a fresh replica and answers its last statement's single
/// value.
fn run(script: Script<String>) -> Outcome {
    let mut connection = temporal::connection();
    script
        .register_functions(&mut connection)
        .map_err(|error| format!("SQLite refused the script's functions: {error}"))?;
    let statements = script.into_vec();
    let (probe, setup) = statements.split_last().ok_or("nothing was emitted")?;
    // The emitted statements are runtime text no typed DSL carries.
    for statement in setup {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .map_err(|error| format!("SQLite refused the setup: {error}\n{statement}"))?;
    }
    let mut rows = diesel::sql_query(probe)
        .load::<DynamicRow<NamedField<TextCell>>>(&mut connection)
        .map_err(|error| format!("SQLite refused it: {error}\n{probe}"))?;
    match rows.len() {
        1 => Ok(first_column_text(rows.remove(0))),
        count => Err(format!("{count} rows instead of one\n{probe}")),
    }
}

/// The PostgreSQL query the reverse translation gives for the emitted
/// `sqlite` probe over the schema `setup` declares.
fn reverse(setup: &str, sqlite: &str, zone: &str) -> Result<String, String> {
    let source = Pg2Sqlite::default().sql(&utc_script(setup)).map_err(|error| error.to_string())?;
    let schema = source.build_schema().map_err(|error| error.to_string())?;
    source
        .reverse_sql(&format!("{sqlite};"), &schema, &temporal::options(zone))
        .map_err(|error| format!("the reverse translation refused it: {error}"))?
        .pop()
        .map(|statement| format!("{statement};"))
        .ok_or_else(|| "the reverse translation emitted nothing".to_owned())
}

/// Whether `outcome` is what PostgreSQL answered.
fn agrees(answer: Answer, outcome: &Outcome) -> bool {
    match (answer, outcome) {
        (Answer::Text(expected), Ok(Some(value))) => value == expected,
        (Answer::Null, Ok(None)) | (Answer::Refused(_), Err(_)) => true,
        _ => false,
    }
}

/// Every case of `cases` answers on the replica what PostgreSQL answered,
/// with the operand written and with it stored, in every zone.
fn assert_agrees(cases: &[Case]) {
    let mut disagreements = Vec::new();
    for (index, zone) in ZONES.into_iter().enumerate() {
        for case in cases {
            let answer = case.answers.in_zone(index);
            for operand in [Operand::Literal, Operand::Stored] {
                let outcome = translate(&case.script(operand), zone).and_then(run);
                if !agrees(answer, &outcome) {
                    disagreements.push(case.describe(zone, operand, answer, &outcome));
                }
            }
        }
    }
    assert!(
        disagreements.is_empty(),
        "{} of {} disagree with PostgreSQL:\n{}",
        disagreements.len(),
        cases.len() * ZONES.len() * 2,
        disagreements.join("\n")
    );
}

/// The reverse translation of every case PostgreSQL answers, translated
/// forward again, still answers what PostgreSQL answered.
fn assert_survives_reversal(cases: &[Case]) {
    let mut disagreements = Vec::new();
    for (index, zone) in ZONES.into_iter().enumerate() {
        for case in cases {
            let answer = case.answers.in_zone(index);
            if matches!(answer, Answer::Refused(_)) {
                continue;
            }
            for operand in [Operand::Literal, Operand::Stored] {
                let prefix = case.prefix(operand);
                let outcome = translate(&case.script(operand), zone)
                    .and_then(|script| {
                        script.into_vec().pop().ok_or_else(|| "nothing was emitted".to_owned())
                    })
                    .and_then(|sqlite| reverse(&case.setup(), &sqlite, zone))
                    .and_then(|reversed| {
                        translate(&format!("{prefix}{reversed}"), zone)
                            .and_then(run)
                            .map_err(|error| format!("{error}\n  reversed   {reversed}"))
                    });
                if !agrees(answer, &outcome) {
                    disagreements.push(case.describe(zone, operand, answer, &outcome));
                }
            }
        }
    }
    assert!(
        disagreements.is_empty(),
        "{} reversals disagree with PostgreSQL:\n{}",
        disagreements.len(),
        disagreements.join("\n")
    );
}

/// One operation of each family, which plain options refuse, since only the
/// registered temporal functions compute temporal values.
const ONE_OF_EACH_FAMILY: &[&str] = &[
    "timestamp '2024-03-05 14:07:09.123456' + interval '1 microsecond'",
    "timestamptz '2024-03-05 14:07:09.123456+00' - interval '1 day'",
    "time '23:59:59.999999' + interval '1 microsecond'",
    "timetz '23:59:59.999999+02' + interval '1 microsecond'",
    "date '2024-03-05' + interval '1 mon'",
    "interval '1 day 02:03:04.567891' * 2",
    "timestamp '2024-03-05 14:07:09.123456' - timestamp '2000-01-01 00:00:00'",
    "timestamp '2024-03-05 14:07:09.123456' AT TIME ZONE 'Europe/Rome'",
    "timezone('+05:30', timestamptz '2024-03-05 14:07:09.123456+00')",
    "CAST(timestamptz '2024-03-05 14:07:09.123456+00' AS time)",
    "extract(epoch from timestamp '2024-03-05 14:07:09.123456')",
    "date_part('second', time '14:07:09.123456')",
    "date_trunc('milliseconds', timestamp '2024-03-05 14:07:09.123456')",
    "to_timestamp(1709647629.123456)",
    "to_char(timestamp '2024-03-05 14:07:09.123456', 'HH24:MI:SS.US')",
    "avg(interval '1 day 02:03:04.567891')",
    "make_timestamp(2024, 2, 29, 23, 59, 59.999999)",
];

/// Without the temporal functions every family's operation is refused.
#[test]
fn plain_options_refuse_every_temporal_family() {
    let translated: Vec<&str> = ONE_OF_EACH_FAMILY
        .iter()
        .copied()
        .filter(|expression| {
            Pg2Sqlite::default()
                .sql(&utc_script(&format!("SELECT CAST(({expression}) AS TEXT);")))
                .and_then(|source| source.translate_to_sql(&Pg2SqliteOptions::default()))
                .is_ok()
        })
        .collect();
    assert!(translated.is_empty(), "plain options translated {translated:#?}");
}

/// See [`timestamp_arithmetic`].
#[test]
fn timestamp_arithmetic_agrees_with_postgres() {
    assert_agrees(timestamp_arithmetic::CASES);
}

/// See [`timestamp_arithmetic`].
#[test]
fn timestamp_arithmetic_survives_reversal() {
    assert_survives_reversal(timestamp_arithmetic::CASES);
}

/// See [`timestamptz_arithmetic`].
#[test]
fn timestamptz_arithmetic_agrees_with_postgres() {
    assert_agrees(timestamptz_arithmetic::CASES);
}

/// See [`timestamptz_arithmetic`].
#[test]
fn timestamptz_arithmetic_survives_reversal() {
    assert_survives_reversal(timestamptz_arithmetic::CASES);
}

/// See [`time_arithmetic`].
#[test]
fn time_arithmetic_agrees_with_postgres() {
    assert_agrees(time_arithmetic::CASES);
}

/// See [`time_arithmetic`].
#[test]
fn time_arithmetic_survives_reversal() {
    assert_survives_reversal(time_arithmetic::CASES);
}

/// See [`timetz_arithmetic`].
#[test]
fn timetz_arithmetic_agrees_with_postgres() {
    assert_agrees(timetz_arithmetic::CASES);
}

/// See [`timetz_arithmetic`].
#[test]
fn timetz_arithmetic_survives_reversal() {
    assert_survives_reversal(timetz_arithmetic::CASES);
}

/// See [`date_arithmetic`].
#[test]
fn date_arithmetic_agrees_with_postgres() {
    assert_agrees(date_arithmetic::CASES);
}

/// See [`date_arithmetic`].
#[test]
fn date_arithmetic_survives_reversal() {
    assert_survives_reversal(date_arithmetic::CASES);
}

/// See [`interval_arithmetic`].
#[test]
fn interval_arithmetic_agrees_with_postgres() {
    assert_agrees(interval_arithmetic::CASES);
}

/// See [`interval_arithmetic`].
#[test]
fn interval_arithmetic_survives_reversal() {
    assert_survives_reversal(interval_arithmetic::CASES);
}

/// See [`comparisons`].
#[test]
fn comparisons_agree_with_postgres() {
    assert_agrees(comparisons::CASES);
}

/// See [`comparisons`].
#[test]
fn comparisons_survive_reversal() {
    assert_survives_reversal(comparisons::CASES);
}

/// See [`time_zones`].
#[test]
fn time_zones_agree_with_postgres() {
    assert_agrees(time_zones::CASES);
}

/// See [`time_zones`].
#[test]
fn time_zones_survive_reversal() {
    assert_survives_reversal(time_zones::CASES);
}

/// See [`casts`].
#[test]
fn casts_agree_with_postgres() {
    assert_agrees(casts::CASES);
}

/// See [`casts`].
#[test]
fn casts_survive_reversal() {
    assert_survives_reversal(casts::CASES);
}

/// See [`extract`].
#[test]
fn extract_agrees_with_postgres() {
    assert_agrees(extract::CASES);
}

/// See [`extract`].
#[test]
fn extract_survives_reversal() {
    assert_survives_reversal(extract::CASES);
}

/// See [`date_part`].
#[test]
fn date_part_agrees_with_postgres() {
    assert_agrees(date_part::CASES);
}

/// See [`date_part`].
#[test]
fn date_part_survives_reversal() {
    assert_survives_reversal(date_part::CASES);
}

/// See [`date_trunc`].
#[test]
fn date_trunc_agrees_with_postgres() {
    assert_agrees(date_trunc::CASES);
}

/// See [`date_trunc`].
#[test]
fn date_trunc_survives_reversal() {
    assert_survives_reversal(date_trunc::CASES);
}

/// See [`to_timestamp`].
#[test]
fn to_timestamp_agrees_with_postgres() {
    assert_agrees(to_timestamp::CASES);
}

/// See [`to_timestamp`].
#[test]
fn to_timestamp_survives_reversal() {
    assert_survives_reversal(to_timestamp::CASES);
}

/// See [`to_char`].
#[test]
fn to_char_agrees_with_postgres() {
    assert_agrees(to_char::CASES);
}

/// See [`to_char`].
#[test]
fn to_char_survives_reversal() {
    assert_survives_reversal(to_char::CASES);
}

/// See [`aggregates`].
#[test]
fn aggregates_agree_with_postgres() {
    assert_agrees(aggregates::CASES);
}

/// See [`aggregates`].
#[test]
fn aggregates_survive_reversal() {
    assert_survives_reversal(aggregates::CASES);
}

/// See [`constructors`].
#[test]
fn constructors_agree_with_postgres() {
    assert_agrees(constructors::CASES);
}

/// See [`constructors`].
#[test]
fn constructors_survive_reversal() {
    assert_survives_reversal(constructors::CASES);
}

/// See [`null_operands`].
#[test]
fn null_operands_agree_with_postgres() {
    assert_agrees(null_operands::CASES);
}

/// See [`null_operands`].
#[test]
fn null_operands_survive_reversal() {
    assert_survives_reversal(null_operands::CASES);
}

#[test]
fn julian_extraction_has_no_fixed_scale() {
    for kind in [DATE, TIMESTAMP, TIMESTAMPTZ] {
        for value in ["NULL", "'2024-03-05'"] {
            let script = format!("SELECT EXTRACT(JULIAN FROM CAST({value} AS {kind}));");
            let error =
                translate(&script, "UTC").expect_err("Julian extraction needs an unbounded scale");
            assert!(error.contains("scale"), "{error}");
        }
    }
}
