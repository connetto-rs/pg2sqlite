//! The `CHECK` a column stored as `TEXT` carries for the PostgreSQL type it
//! stands for.
//!
//! A `uuid` under [`UuidRepresentation::Text`], a date, a time, a timestamp
//! and a JSON document all land in a `TEXT` column, and the translation writes
//! each in one form. A bound parameter or a row applied directly reaches the
//! column as it is, so the column itself admits only that form, which is the
//! form a caller binds. Every check passes on NULL, which `NOT NULL` refuses
//! where the column declares it.
//! [`Pg2SqliteOptions::with_stored_text_checks_omitted`] leaves every one of
//! them out together.
//!
//! The forms, each next to its template:
//!
//! - `uuid`: lowercase hexadecimal, hyphenated 8-4-4-4-12.
//! - `timestamptz`: UTC with six fractional digits, `2024-01-01
//!   10:00:00.000000+00:00`, or `infinity` or `-infinity`.
//! - `timestamp`: `2024-01-01 10:00:00` with up to six fractional digits, or
//!   `infinity` or `-infinity`.
//! - `date`: `2024-01-01`, or `infinity` or `-infinity`.
//! - `time`: `10:00:00` with up to six fractional digits, or `24:00:00`.
//! - `timetz`: a `time` followed by a `±HH:MM` offset of at most 15 hours.
//! - `json` and `jsonb`: JSON text SQLite's `json_valid` reads.
//!
//! A calendar date is held to the days its month has by reading it back
//! through `date()`, which moves `2023-02-29` to March. `date()` also reads
//! the year 0, which PostgreSQL has no such day for, hence the bound on the
//! text from `0001`. SQLite's `time()` reads hours up to 24, so the hour is
//! bounded on the text as well.

use core::ops::ControlFlow;

use sqlparser::{
    ast::{DataType, Expr, Ident, VisitMut, VisitorMut},
    dialect::SQLiteDialect,
    parser::Parser,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        temporal_literals::{TemporalLiteralKind, temporal_literal_kind},
        translator_impls::uuid::is_uuid_data_type,
    },
    options::Pg2SqliteOptions,
    traits::UuidRepresentation,
};

/// The identifier each template names the column by.
const PLACEHOLDER: &str = "pg2sqlite_stored_text";

const UUID: &str = "pg2sqlite_stored_text GLOB '[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]'";

const TIMESTAMPTZ: &str = "pg2sqlite_stored_text IN ('infinity', '-infinity') OR (pg2sqlite_stored_text GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9] [0-2][0-9]:[0-5][0-9]:[0-5][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00' AND pg2sqlite_stored_text >= '0001' AND substr(pg2sqlite_stored_text, 12, 2) < '24' AND date(substr(pg2sqlite_stored_text, 1, 10)) IS substr(pg2sqlite_stored_text, 1, 10))";

const TIMESTAMP: &str = "pg2sqlite_stored_text IN ('infinity', '-infinity') OR (pg2sqlite_stored_text GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9] [0-2][0-9]:[0-5][0-9]:[0-5][0-9]*' AND pg2sqlite_stored_text >= '0001' AND substr(pg2sqlite_stored_text, 12, 2) < '24' AND date(substr(pg2sqlite_stored_text, 1, 10)) IS substr(pg2sqlite_stored_text, 1, 10) AND (length(pg2sqlite_stored_text) = 19 OR (length(pg2sqlite_stored_text) <= 26 AND substr(pg2sqlite_stored_text, 20) GLOB '.[0-9]*' AND NOT (substr(pg2sqlite_stored_text, 21) GLOB '*[^0-9]*'))))";

const DATE: &str = "pg2sqlite_stored_text IN ('infinity', '-infinity') OR (pg2sqlite_stored_text >= '0001' AND date(pg2sqlite_stored_text) IS pg2sqlite_stored_text)";

const TIME: &str = "pg2sqlite_stored_text = '24:00:00' OR (pg2sqlite_stored_text GLOB '[0-2][0-9]:[0-5][0-9]:[0-5][0-9]*' AND substr(pg2sqlite_stored_text, 1, 2) < '24' AND (length(pg2sqlite_stored_text) = 8 OR (length(pg2sqlite_stored_text) <= 15 AND substr(pg2sqlite_stored_text, 9) GLOB '.[0-9]*' AND NOT (substr(pg2sqlite_stored_text, 10) GLOB '*[^0-9]*'))))";

const TIMETZ: &str = "pg2sqlite_stored_text GLOB '[0-2][0-9]:[0-5][0-9]:[0-5][0-9]*[+-][0-1][0-9]:[0-5][0-9]' AND substr(pg2sqlite_stored_text, -5, 2) <= '15' AND (length(pg2sqlite_stored_text) = 14 OR (length(pg2sqlite_stored_text) <= 21 AND substr(pg2sqlite_stored_text, 9, length(pg2sqlite_stored_text) - 14) GLOB '.[0-9]*' AND NOT (substr(pg2sqlite_stored_text, 10, length(pg2sqlite_stored_text) - 15) GLOB '*[^0-9]*'))) AND (substr(pg2sqlite_stored_text, 1, length(pg2sqlite_stored_text) - 6) = '24:00:00' OR substr(pg2sqlite_stored_text, 1, 2) < '24')";

const JSON: &str = "json_valid(pg2sqlite_stored_text)";

/// The `CHECK` holding `column` to the text the translation writes for
/// `data_type`, or `None` for a type stored as anything else and when
/// [`Pg2SqliteOptions::with_stored_text_checks_omitted`] leaves the checks
/// out.
///
/// # Errors
///
/// Returns [`Error::TranslationRefusal`] when a template fails to parse,
/// which no input can cause.
pub(crate) fn stored_text_check(
    column: &Ident,
    data_type: &DataType,
    options: &Pg2SqliteOptions,
) -> Result<Option<Expr>, Error> {
    if options.is_stored_text_checks_omitted() {
        return Ok(None);
    }
    let template = match (temporal_literal_kind(data_type), data_type) {
        (Some(TemporalLiteralKind::Timestamp { zoned: true, .. }), _) => TIMESTAMPTZ,
        (Some(TemporalLiteralKind::Timestamp { zoned: false, .. }), _) => TIMESTAMP,
        (Some(TemporalLiteralKind::Date), _) => DATE,
        (Some(TemporalLiteralKind::Time { zoned: false, .. }), _) => TIME,
        (Some(TemporalLiteralKind::Time { zoned: true, .. }), _) => TIMETZ,
        (None, DataType::JSON | DataType::JSONB) => JSON,
        (None, data_type)
            if is_uuid_data_type(data_type)
                && matches!(options.get_uuid_representation(), Some(UuidRepresentation::Text)) =>
        {
            UUID
        }
        (None, _) => return Ok(None),
    };
    substitute(template, column).map(Some)
}

/// `template` parsed, with every [`PLACEHOLDER`] naming `column`.
fn substitute(template: &str, column: &Ident) -> Result<Expr, Error> {
    let mut expr = Parser::new(&SQLiteDialect {})
        .try_with_sql(template)
        .and_then(|mut parser| parser.parse_expr())
        .map_err(|error| {
            Error::forward_refusal(format!(
                "the stored text check template fails to parse as an expression: {error}"
            ))
        })?;
    let _: ControlFlow<()> = expr.visit(&mut NameColumn { column });
    Ok(expr)
}

/// Replaces the template's placeholder with the column.
struct NameColumn<'column> {
    column: &'column Ident,
}

impl VisitorMut for NameColumn<'_> {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        if matches!(expr, Expr::Identifier(ident) if ident.value == PLACEHOLDER) {
            *expr = Expr::Identifier(self.column.clone());
        }
        ControlFlow::Continue(())
    }
}
