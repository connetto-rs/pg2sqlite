//! The canonical text format of SQLite's current instant.

use sqlparser::ast::Expr;

/// SQLite's millisecond clock padded to the stored microsecond format.
pub(crate) const TIMESTAMPTZ_FORMAT: &str = "%Y-%m-%d %H:%M:%f000+00:00";

/// Whether `expr`, through any parentheses, is `strftime` formatting an
/// instant as [`TIMESTAMPTZ_FORMAT`].
#[must_use]
pub(crate) fn is_canonical_timestamptz_call(mut expr: &Expr) -> bool {
    while let Expr::Nested(inner) = expr {
        expr = inner;
    }
    let Expr::Function(function) = expr else { return false };
    super::function_helpers::is_function_named(function, "strftime")
        && matches!(
            super::shared_helpers::function_argument_exprs(&function.args).as_slice(),
            [format, _] if super::function_helpers::single_quoted_literal(format)
                == Some(TIMESTAMPTZ_FORMAT)
        )
}
