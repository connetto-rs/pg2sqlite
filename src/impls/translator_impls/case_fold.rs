//! The SQLite function a PostgreSQL case fold becomes.
//!
//! PostgreSQL folds a text's case by the `LC_CTYPE` of its collation. Under
//! `C` and `POSIX` that touches ASCII letters only, which is what SQLite's
//! `lower` and `upper` do. Under the database default of a UTF-8 locale it
//! folds every cased letter, so `lower('Ä')` answers `ä` where SQLite answers
//! `Ä`, and only the functions the host registers through
//! [`CaseFoldFunctions`] answer as the server does. Every other collation is
//! refused where the translation meets its `COLLATE`.

use core::fmt::Display;

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Expr, ObjectName, ObjectNamePart, Value};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        expr_helpers::for_each_child_expr,
        shared_helpers::{DeclaredCollation, function_arg_expr, inspect_declared_in_scope},
    },
    options::TranslationContext,
    traits::CaseFoldFunctions,
};

/// The case a fold maps a text onto.
#[derive(Clone, Copy)]
pub(crate) enum Case {
    /// `lower`, which `ILIKE` folds through as well.
    Lower,
    /// `upper`.
    Upper,
}

impl Case {
    /// SQLite's own function, which folds ASCII letters only.
    fn builtin(self) -> &'static str {
        match self {
            Self::Lower => "lower",
            Self::Upper => "upper",
        }
    }

    /// The registered function folding as the server does.
    fn registered(self, functions: &CaseFoldFunctions) -> &str {
        match self {
            Self::Lower => &functions.lower,
            Self::Upper => &functions.upper,
        }
    }
}

/// How PostgreSQL folds a text, which its collation decides.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Folding {
    /// `C` and `POSIX`, which fold ASCII letters only.
    Ascii,
    /// The database default, which folds by the server's `LC_CTYPE`.
    Locale,
}

/// The folding an expression carries, and whether a `COLLATE` wrote it.
#[derive(Clone, Copy)]
enum Derived {
    /// Written with `COLLATE`, which outranks any collation a column carries.
    Explicit(Folding),
    /// Carried up from a column the expression reads.
    Implicit(Folding),
    /// Read from no column and written with no `COLLATE`, which leaves the
    /// database default.
    Unset,
}

impl Derived {
    /// The folding PostgreSQL settles on.
    fn settled(self) -> Folding {
        match self {
            Self::Explicit(folding) | Self::Implicit(folding) => folding,
            Self::Unset => Folding::Locale,
        }
    }

    /// The derivation of an expression reading both `self` and `other`.
    ///
    /// An explicit collation decides, as in PostgreSQL. Two implicit ones that
    /// disagree are a PostgreSQL error when both texts are collatable, so the
    /// only valid input reaching that arm reads a `C` text beside a column of
    /// a type with no collation, which this derivation cannot tell from the
    /// default and reads as the locale. The `C` text decides there.
    fn joined(self, other: Self) -> Self {
        match (self, other) {
            (Self::Explicit(_), _)
            | (Self::Implicit(Folding::Ascii), Self::Implicit(_))
            | (_, Self::Unset) => self,
            (_, Self::Explicit(_) | Self::Implicit(_)) => other,
        }
    }
}

/// The SQLite function folding `operands` to `case` as PostgreSQL does in
/// `call`.
///
/// Every operand ASCII text, or a text collated `C` or `POSIX`, folds through
/// SQLite's own function. Anything else folds by the server's `LC_CTYPE`,
/// through the registered function.
///
/// # Errors
///
/// Refuses a fold by the server's `LC_CTYPE` without registered functions.
pub(crate) fn fold_function<'o>(
    case: Case,
    call: &dyn Display,
    operands: &[&Expr],
    schema: &ParserDB,
    options: &'o TranslationContext<'_>,
) -> Result<&'o str, Error> {
    if operands.iter().all(|operand| is_ascii_text(operand)) {
        return Ok(case.builtin());
    }
    let derived = operands.iter().fold(Derived::Unset, |derived, operand| {
        derived.joined(derivation(operand, schema, options))
    });
    if derived.settled() == Folding::Ascii {
        return Ok(case.builtin());
    }
    options.get_case_fold_functions().map(|functions| case.registered(functions)).ok_or_else(|| {
        Error::forward_refusal(format!(
            "{call} folds case by the server's LC_CTYPE, which under a UTF-8 locale folds every \
             cased letter, and SQLite's {builtin}() folds ASCII letters only, so a letter such as \
             Ä or ä would keep its case where PostgreSQL changes it. Register functions folding as \
             the server does, with CaseFoldFunctions::register under the diesel feature for a \
             glibc UTF-8 LC_CTYPE, and name them with \
             with_case_fold_functions(CaseFoldFunctions::new(\"pg_lower\", \"pg_upper\")), or \
             with CaseFoldFunctions::new(\"lower\", \"upper\") when the server's LC_CTYPE is C.",
            builtin = case.builtin()
        ))
    })
}

/// True for an ASCII literal, or for `encode`, whose `hex`, `base64` and
/// `escape` answers are ASCII, so every collation folds them alike.
#[cfg_attr(feature = "std", recursive::recursive)]
fn is_ascii_text(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => is_ascii_text(inner),
        Expr::Value(literal) => {
            match &literal.value {
                Value::Null | Value::Number(..) | Value::Boolean(_) => true,
                Value::SingleQuotedString(text) => text.is_ascii(),
                Value::DollarQuotedString(text) => text.value.is_ascii(),
                _ => false,
            }
        }
        Expr::Array(array) => array.elem.iter().all(is_ascii_text),
        Expr::Function(function) => {
            matches!(function.name.0.as_slice(), [ObjectNamePart::Identifier(name)]
                if name.value.eq_ignore_ascii_case("encode"))
        }
        _ => false,
    }
}

/// The folding `expr` carries, derived bottom up as PostgreSQL derives a
/// collation.
#[cfg_attr(feature = "std", recursive::recursive)]
fn derivation(expr: &Expr, schema: &ParserDB, options: &TranslationContext<'_>) -> Derived {
    match expr {
        Expr::Nested(inner) => derivation(inner, schema, options),
        Expr::Collate { collation, .. } => Derived::Explicit(collation_folding(collation)),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            Derived::Implicit(reference_folding(expr, schema, options))
        }
        _ => {
            let mut derived = Derived::Unset;
            for_each_child_expr(expr, &mut |child| {
                derived = derived.joined(derivation(child, schema, options));
            });
            derived
        }
    }
}

/// The folding the column `reference` names is declared with.
///
/// A column of a function called in `FROM` takes the folding of the
/// arguments the call writes, as PostgreSQL derives its collation. A
/// reference the scope does not settle, such as a PL/pgSQL variable or a name
/// no relation in scope declares, and a collation the declaration changes
/// without naming, read as the database default, which is the collation of
/// every text declared without one.
fn reference_folding(
    reference: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Folding {
    inspect_declared_in_scope(
        reference,
        schema,
        options,
        |column| {
            Some(match column.collation() {
                DeclaredCollation::TypeDefault | DeclaredCollation::Unnamed => Folding::Locale,
                DeclaredCollation::Named(collation) => collation_folding(collation),
                DeclaredCollation::Call { arguments, scope } => {
                    let scoped = options.with_definition_scope(*scope);
                    arguments
                        .iter()
                        .filter_map(function_arg_expr)
                        .fold(Derived::Unset, |derived, argument| {
                            derived.joined(derivation(argument, schema, &scoped))
                        })
                        .settled()
                }
            })
        },
        |expression, schema, options| Ok(Some(derivation(expression, schema, options).settled())),
    )
    .ok()
    .flatten()
    .unwrap_or(Folding::Locale)
}

/// How a text collated `collation` folds.
fn collation_folding(collation: &ObjectName) -> Folding {
    let ascii_only = collation.0.last().and_then(ObjectNamePart::as_ident).is_some_and(|name| {
        name.value.eq_ignore_ascii_case("C") || name.value.eq_ignore_ascii_case("POSIX")
    });
    if ascii_only { Folding::Ascii } else { Folding::Locale }
}
