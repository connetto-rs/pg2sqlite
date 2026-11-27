//! Diesel registration of the temporal scalar and aggregate functions.

use alloc::{boxed::Box, format, string::String};

use diesel::{
    QueryResult,
    serialize::{IsNull, Output, ToSql},
    sql_types::{BigInt, Double, Nullable, Text},
    sqlite::{Sqlite, SqliteAggregateFunction, SqliteConnection, SqliteFunctionBehavior},
};
use sql_values::postgres::IntervalSum;

use super::{
    TemporalArgument, TemporalError, TemporalFunction, TemporalFunctions, TemporalKind,
    TemporalValue,
};

#[derive(Debug)]
struct Answer(Result<Option<TemporalValue>, TemporalError>);

impl ToSql<Nullable<Text>, Sqlite> for Answer {
    fn to_sql<'borrow>(
        &'borrow self,
        output: &mut Output<'borrow, '_, Sqlite>,
    ) -> diesel::serialize::Result {
        match &self.0 {
            Ok(Some(TemporalValue::Text(value))) => {
                <String as ToSql<Text, Sqlite>>::to_sql(value, output)
            }
            Ok(Some(TemporalValue::Integer(value))) => {
                output.set_value(*value);
                Ok(IsNull::No)
            }
            Ok(Some(TemporalValue::Real(value))) => {
                output.set_value(*value);
                Ok(IsNull::No)
            }
            Ok(None) => Ok(IsNull::Yes),
            Err(error) => Err(Box::new(error.clone())),
        }
    }
}

trait Argument {
    fn borrowed(&self) -> TemporalArgument<'_>;
}

impl Argument for Option<String> {
    fn borrowed(&self) -> TemporalArgument<'_> {
        self.as_deref().map_or(TemporalArgument::Null, TemporalArgument::Text)
    }
}
impl Argument for Option<i64> {
    fn borrowed(&self) -> TemporalArgument<'_> {
        self.map_or(TemporalArgument::Null, TemporalArgument::Integer)
    }
}
impl Argument for Option<f64> {
    fn borrowed(&self) -> TemporalArgument<'_> {
        self.map_or(TemporalArgument::Null, TemporalArgument::Real)
    }
}

const BEHAVIOR: SqliteFunctionBehavior =
    SqliteFunctionBehavior::DETERMINISTIC.union(SqliteFunctionBehavior::INNOCUOUS);

impl TemporalFunctions {
    /// Registers the temporal functions on a SQLite connection.
    ///
    /// # Errors
    /// Returns SQLite's error if registration fails.
    ///
    /// ```
    /// use diesel::{
    ///     prelude::*,
    ///     sql_types::{BigInt, Text},
    ///     sqlite::SqliteConnection,
    /// };
    /// use pg2sqlite_runtime::TemporalFunctions;
    /// #[declare_sql_function]
    /// extern "SQL" {
    ///     fn pg2sqlite_round_time(value: Text, precision: BigInt) -> Text;
    /// }
    /// let mut connection = SqliteConnection::establish(":memory:").unwrap();
    /// TemporalFunctions::default().register(&mut connection).unwrap();
    /// let answer: String = diesel::select(pg2sqlite_round_time("23:59:59.6", 0_i64))
    ///     .get_result(&mut connection)
    ///     .unwrap();
    /// assert_eq!(answer, "24:00:00");
    /// ```
    pub fn register(&self, connection: &mut SqliteConnection) -> QueryResult<()> {
        use TemporalFunction::{
            DateDifference, DateMinusDays, DateMinusInterval, DatePlusDays, DatePlusInterval,
            DatePlusTime, DatePlusTimeTz, DateToTimestamp, DateToTimestampTz, EpochToTimestampTz,
            IntervalAverage, IntervalDivide, IntervalMinus, IntervalMultiply, IntervalNegate,
            IntervalPlus, IntervalScale, IntervalSum, IntervalTrunc, MakeDate, MakeTime,
            MakeTimestamp, TimeDifference, TimeMinusInterval, TimePlusInterval, TimeToTimeTz,
            TimeTzAtZone, TimeTzMinusInterval, TimeTzOut, TimeTzPlusInterval, TimeTzToTime,
            TimestampAtZone, TimestampDifference, TimestampMinusInterval, TimestampPlusInterval,
            TimestampToDate, TimestampToTime, TimestampToTimestampTz, TimestampTrunc,
            TimestampTzAtZone, TimestampTzDifference, TimestampTzMinusInterval, TimestampTzOut,
            TimestampTzPlusInterval, TimestampTzToDate, TimestampTzToTime, TimestampTzToTimeTz,
            TimestampTzToTimestamp, TimestampTzTrunc,
        };
        for function in [
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
            TimestampTzAtZone,
            TimestampAtZone,
            TimeTzAtZone,
            TimestampTrunc,
            TimestampTzTrunc,
            IntervalTrunc,
            EpochToTimestampTz,
            TemporalFunction::EpochTextToTimestampTz,
            TemporalFunction::IntervalToTime,
            MakeDate,
            MakeTime,
            MakeTimestamp,
        ] {
            self.register_one(connection, function)?;
        }
        for function in [
            TemporalFunction::IntervalJustifyHours,
            TemporalFunction::IntervalJustifyDays,
            TemporalFunction::IntervalJustifyInterval,
            TemporalFunction::TimeToInterval,
            TemporalFunction::TimestampAtIntervalZone,
            TemporalFunction::TimestampTzAtIntervalZone,
            TemporalFunction::TimeTzAtIntervalZone,
            TemporalFunction::Float8Out,
            TemporalFunction::TimeTzCompare,
        ] {
            self.register_one(connection, function)?;
        }
        for kind in TemporalKind::ALL {
            for function in [
                TemporalFunction::Input(kind),
                TemporalFunction::Extract(kind),
                TemporalFunction::DatePart(kind),
            ] {
                self.register_one(connection, function)?;
            }
            if kind != TemporalKind::TimeTz {
                self.register_one(connection, TemporalFunction::ToChar(kind))?;
            }
            if !matches!(kind, TemporalKind::Date | TemporalKind::Interval) {
                self.register_one(connection, TemporalFunction::Round(kind))?;
            }
        }
        Ok(())
    }

    fn register_one(
        &self,
        connection: &mut SqliteConnection,
        function: TemporalFunction,
    ) -> QueryResult<()> {
        macro_rules! register {
            ($($argument:ident: $sql_type:ty => $rust_type:ty),+ $(,)?) => {
                connection.register_sql_function::<($(Nullable<$sql_type>,)+), Nullable<Text>, _, _, _>(
                    &self.name(function), BEHAVIOR,
                    move |($($argument,)+): ($(Option<$rust_type>,)+)| {
                        Answer(function.evaluate(&[$($argument.borrowed(),)+]))
                    },
                )
            };
        }
        use TemporalFunction::{
            DateDifference, DateMinusDays, DateMinusInterval, DatePlusDays, DatePlusInterval,
            DatePlusTime, DatePlusTimeTz, DateToTimestamp, DateToTimestampTz, EpochToTimestampTz,
            IntervalAverage, IntervalDivide, IntervalMinus, IntervalMultiply, IntervalNegate,
            IntervalPlus, IntervalScale, IntervalSum, IntervalTrunc, MakeDate, MakeTime,
            MakeTimestamp, TimeDifference, TimeMinusInterval, TimePlusInterval, TimeToTimeTz,
            TimeTzAtZone, TimeTzMinusInterval, TimeTzOut, TimeTzPlusInterval, TimeTzToTime,
            TimestampAtZone, TimestampDifference, TimestampMinusInterval, TimestampPlusInterval,
            TimestampToDate, TimestampToTime, TimestampToTimestampTz, TimestampTrunc,
            TimestampTzAtZone, TimestampTzDifference, TimestampTzMinusInterval, TimestampTzOut,
            TimestampTzPlusInterval, TimestampTzToDate, TimestampTzToTime, TimestampTzToTimeTz,
            TimestampTzToTimestamp, TimestampTzTrunc,
        };
        match function {
            TemporalFunction::IntervalJustifyHours | TemporalFunction::IntervalJustifyDays
                | TemporalFunction::IntervalJustifyInterval | TemporalFunction::TimeToInterval =>
                register!(value: Text => String),
            TemporalFunction::TimestampAtIntervalZone | TemporalFunction::TimestampTzAtIntervalZone
                | TemporalFunction::TimeTzAtIntervalZone =>
                register!(first: Text => String, second: Text => String),
            TemporalFunction::Float8Out => register!(value: Double => f64),
            TemporalFunction::TimeTzCompare => register!(first: Text => String, second: Text => String),
            DatePlusDays | DateMinusDays | TemporalFunction::Round(_) => register!(value: Text => String, count: BigInt => i64),
            DateDifference | DatePlusInterval | DateMinusInterval | DatePlusTime | DatePlusTimeTz
                | TimestampPlusInterval | TimestampMinusInterval | TimestampDifference
                | TimestampTzDifference | TimePlusInterval | TimeMinusInterval | TimeDifference
                | TimeTzPlusInterval | TimeTzMinusInterval | IntervalPlus | IntervalMinus
                | TimestampTzToTimestamp | TimestampToTimestampTz | TimestampTzToDate
                | DateToTimestampTz | TimestampTzToTime | TimestampTzToTimeTz | TimestampTzOut
                | TimestampTzAtZone | TimestampAtZone | TimestampTrunc | IntervalTrunc =>
                register!(first: Text => String, second: Text => String),
            TimestampTzPlusInterval | TimestampTzMinusInterval | TimeToTimeTz | TimeTzAtZone
                | TimestampTzTrunc | TemporalFunction::Input(_) =>
                register!(first: Text => String, second: Text => String, third: Text => String),
            IntervalNegate | TimestampToDate | TimestampToTime | DateToTimestamp | TimeTzToTime | TimeTzOut => register!(value: Text => String),
            IntervalMultiply | IntervalDivide => register!(value: Text => String, factor: Double => f64),
            EpochToTimestampTz => register!(seconds: Double => f64),
            TemporalFunction::EpochTextToTimestampTz => register!(seconds: Text => String),
            TemporalFunction::IntervalToTime => register!(value: Text => String),
            MakeDate => register!(year: BigInt => i64, month: BigInt => i64, day: BigInt => i64),
            MakeTime => register!(hour: BigInt => i64, minute: BigInt => i64, second: Double => f64),
            MakeTimestamp => register!(year: BigInt => i64, month: BigInt => i64, day: BigInt => i64, hour: BigInt => i64, minute: BigInt => i64, second: Double => f64),
            IntervalScale => register!(value: Text => String, fields: Text => String, precision: BigInt => i64),
            TemporalFunction::Extract(kind) | TemporalFunction::DatePart(kind) => {
                if kind == TemporalKind::TimestampTz {
                    register!(field: Text => String, value: Text => String, zone: Text => String)
                } else {
                    register!(field: Text => String, value: Text => String)
                }
            }
            TemporalFunction::ToChar(kind) => {
                if matches!(kind, TemporalKind::Date | TemporalKind::TimestampTz) {
                    register!(value: Text => String, template: Text => String, zone: Text => String)
                } else {
                    register!(value: Text => String, template: Text => String)
                }
            }
            IntervalSum => connection.register_aggregate_function::<Nullable<Text>, Nullable<Text>, _, _, Aggregate<false>>(&self.name(function), BEHAVIOR),
            IntervalAverage => connection.register_aggregate_function::<Nullable<Text>, Nullable<Text>, _, _, Aggregate<true>>(&self.name(function), BEHAVIOR),
        }
    }
}

#[derive(Default)]
struct Aggregate<const AVERAGE: bool> {
    sum: IntervalSum,
    error: Option<TemporalError>,
}

impl<const AVERAGE: bool> SqliteAggregateFunction<Option<String>> for Aggregate<AVERAGE> {
    type Output = Answer;

    fn step(&mut self, value: Option<String>) {
        if self.error.is_some() {
            return;
        }
        if let Some(value) = value {
            match super::evaluation::interval(&value) {
                Ok(value) => {
                    self.sum.merge(IntervalSum::of(value, 1));
                    if let Err(error) = self.sum.sum() {
                        self.error = Some(error.into());
                    }
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn finalize(aggregate: Option<Self>) -> Self::Output {
        let Some(aggregate) = aggregate else {
            return Answer(Ok(None));
        };
        if let Some(error) = aggregate.error {
            return Answer(Err(error));
        }
        let answer = if AVERAGE { aggregate.sum.avg() } else { aggregate.sum.sum() };
        Answer(
            answer
                .map(|value| value.map(|value| TemporalValue::Text(format!("{value}"))))
                .map_err(Into::into),
        )
    }
}
