//! Session-zone conversions through the temporal runtime.

use pg2sqlite_runtime::TemporalFunction;
use sql_traits::structs::ParserDB;
use sqlparser::ast::Expr;

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        function_helpers::{simple_function_expr, string_literal},
        temporal_functions::{call, session_zone},
    },
    options::TranslationContext,
};

pub(crate) fn canonical_now() -> Expr {
    simple_function_expr(
        "strftime",
        vec![string_literal("%Y-%m-%d %H:%M:%f000+00:00"), string_literal("now")],
        None,
    )
}

pub(crate) fn conversion_wall_clock(
    kind: crate::impls::temporal_literals::TemporalLiteralKind,
    source: Option<crate::impls::timezone::TimestampAwareness>,
    operation: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Expr>, Error> {
    use crate::impls::{temporal_literals::TemporalLiteralKind, timezone::TimestampAwareness};
    let into_instant = matches!(kind, TemporalLiteralKind::Timestamp { zoned: true, .. });
    match source {
        Some(TimestampAwareness::Aware) if !into_instant => {
            session_zone(options, schema, operation).map(Some)
        }
        Some(TimestampAwareness::Naive) if into_instant => {
            session_zone(options, schema, operation).map(Some)
        }
        _ => Ok(None),
    }
}

pub(crate) fn timestamptz_text_form(
    instant: Expr,
    operation: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    call(
        options,
        TemporalFunction::TimestampTzOut,
        vec![instant, session_zone(options, schema, operation)?],
        operation,
    )
}

/// The clock functions PostgreSQL answers from the request's zone.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalClock {
    /// `localtimestamp`.
    Timestamp,
    /// `localtime`.
    Time,
    /// `current_date`.
    Date,
    /// `current_time`, a time with time zone.
    TimeWithZone,
}

impl LocalClock {
    /// The function's name as PostgreSQL spells it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Timestamp => "localtimestamp",
            Self::Time => "localtime",
            Self::Date => "current_date",
            Self::TimeWithZone => "current_time",
        }
    }
}
