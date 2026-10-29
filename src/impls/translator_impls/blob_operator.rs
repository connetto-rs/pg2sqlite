//! `||`, `+`, `-` and `*` over `bytea` and pgvector values, which the replica
//! stores as BLOB.
//!
//! SQLite's own operators read a BLOB as text or as a number: `x'01' ||
//! x'02'` answers TEXT, and `x'01' + x'02'` answers 0. So each operator
//! PostgreSQL defines over these types is lowered to a form that keeps the
//! bytes, and each one it does not define is refused, as the server would.

#[cfg(not(feature = "std"))]
use alloc::{borrow::ToOwned, boxed::Box, format, string::String, vec};

use sql_traits::structs::ParserDB;
use sqlparser::ast::{BinaryOperator, CastKind, DataType, Expr};

use crate::{
    errors::Error,
    impls::{
        bind_once::bind_once,
        expr_helpers::case_when,
        expression_type::{UNKNOWN, parse_type_name, type_name},
        function_helpers::{simple_function_expr, single_quoted_literal, string_literal},
        shared_helpers::{convert_bytea_hex_literal, is_bound_parameter},
        translator_impls::vector::{halfvec_refusal, is_halfvec_data_type, is_vector_data_type},
    },
    traits::translator::TranslatorWithContext,
};

/// What an operand's type is known to be.
enum Operand {
    /// An untyped literal or a bound parameter, which takes the type of the
    /// other side the way PostgreSQL resolves `unknown`.
    Untyped,
    /// A declared, cast or inferred type.
    Typed(DataType),
    /// Nothing the statement writes decides it.
    Unresolved,
}

/// The two families of BLOB-stored values an operator is defined over.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Bytea,
    Vector,
}

/// How an operator over a BLOB-stored operand is lowered.
#[derive(Clone, Copy)]
enum Lowering {
    /// `CAST(l || r AS BLOB)`, which keeps every byte of both.
    BlobConcat(Family),
    /// The `bytea` side's text form, `\x` and lowercase hex digits,
    /// concatenated with text.
    ByteaText { bytea_on_left: bool },
    /// A sqlite-vec element-wise function, NULL when either side is.
    VectorArithmetic(&'static str),
}

/// An operator PostgreSQL resolves over a BLOB-stored operand, with the type
/// it answers.
pub(crate) struct BlobOperator {
    lowering: Lowering,
    result: DataType,
}

impl BlobOperator {
    /// The type the operator answers.
    #[must_use]
    pub(crate) fn result_type(&self) -> &DataType {
        &self.result
    }
}

/// The operand's type. A type the scope cannot read is unresolved, which
/// matters only beside a BLOB-stored operand, where [`resolve`] refuses it.
fn operand(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Operand {
    if is_bound_parameter(expr) {
        return Operand::Untyped;
    }
    match type_name(expr, schema, options) {
        Ok(Some(name)) if name == UNKNOWN => Operand::Untyped,
        Ok(Some(name)) => parse_type_name(&name).map_or(Operand::Unresolved, Operand::Typed),
        Ok(None) | Err(_) => Operand::Unresolved,
    }
}

fn family(data_type: &DataType) -> Option<Family> {
    if matches!(data_type, DataType::Bytea) {
        Some(Family::Bytea)
    } else if is_vector_data_type(data_type) {
        Some(Family::Vector)
    } else {
        None
    }
}

/// True for the types PostgreSQL's `anynonarray || text` takes as text.
fn is_text(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Text
            | DataType::Varchar(_)
            | DataType::CharacterVarying(_)
            | DataType::CharVarying(_)
            | DataType::Char(_)
            | DataType::Character(_)
    )
}

/// Resolves `left op right` when an operand is stored as a BLOB, or [`None`]
/// when neither is and SQLite's operator already answers what PostgreSQL's
/// does.
///
/// # Errors
///
/// Refuses an operator PostgreSQL does not define over the operand types, one
/// sqlite-vec has no function for, a `halfvec` operand, and an operand whose
/// type the statement does not decide beside a BLOB-stored one.
pub(crate) fn resolve(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<BlobOperator>, Error> {
    if !matches!(
        op,
        BinaryOperator::StringConcat
            | BinaryOperator::Plus
            | BinaryOperator::Minus
            | BinaryOperator::Multiply
    ) {
        return Ok(None);
    }
    let (left_type, right_type) = (operand(left, schema, options), operand(right, schema, options));
    let typed = |side: &Operand| {
        match side {
            Operand::Typed(data_type) => Some(data_type.clone()),
            _ => None,
        }
    };
    let sides = [typed(&left_type), typed(&right_type)];
    // An array operand makes `||` an append, which the array lowering owns.
    if sides.iter().flatten().any(|data_type| matches!(data_type, DataType::Array(_))) {
        return Ok(None);
    }
    if sides.iter().flatten().any(is_halfvec_data_type) {
        return Err(halfvec_refusal());
    }
    let Some((blob_on_left, blob_type, blob_family)) =
        sides.iter().enumerate().find_map(|(index, side)| {
            let data_type = side.as_ref()?;
            family(data_type).map(|found| (index == 0, data_type.clone(), found))
        })
    else {
        return Ok(None);
    };
    let other = if blob_on_left { &right_type } else { &left_type };
    let other_family = match other {
        Operand::Untyped => Some(blob_family),
        Operand::Typed(data_type) => family(data_type),
        Operand::Unresolved => {
            let unresolved = if blob_on_left { right } else { left };
            return Err(Error::forward_refusal(format!(
                "{op} beside a {blob_type} value needs the type of {unresolved}, which the \
                 statement does not decide, and the operator PostgreSQL picks depends on it. \
                 Cast {unresolved} to the type it holds."
            )));
        }
    };
    let undefined = || {
        let describe = |side: &Operand| {
            match side {
                Operand::Typed(data_type) => format!("{data_type}"),
                _ => String::from("an untyped value"),
            }
        };
        Error::forward_refusal(format!(
            "{op} over {} and {} has no PostgreSQL operator, so the server refuses it with \
             `operator does not exist`.",
            describe(&left_type),
            describe(&right_type)
        ))
    };
    let lowering = match (blob_family, op) {
        (Family::Bytea, BinaryOperator::StringConcat) if other_family == Some(Family::Bytea) => {
            Lowering::BlobConcat(Family::Bytea)
        }
        (Family::Bytea, BinaryOperator::StringConcat) if matches!(other, Operand::Typed(data_type) if is_text(data_type)) =>
        {
            return Ok(Some(BlobOperator {
                lowering: Lowering::ByteaText { bytea_on_left: blob_on_left },
                result: DataType::Text,
            }));
        }
        (Family::Vector, _) if other_family != Some(Family::Vector) => return Err(undefined()),
        (Family::Vector, BinaryOperator::StringConcat) => Lowering::BlobConcat(Family::Vector),
        (Family::Vector, BinaryOperator::Plus) => Lowering::VectorArithmetic("vec_add"),
        (Family::Vector, BinaryOperator::Minus) => Lowering::VectorArithmetic("vec_sub"),
        (Family::Vector, _) => {
            return Err(Error::forward_refusal(
                "* over two vectors multiplies them element by element in pgvector, and \
                 sqlite-vec has no function that does. Multiply the elements in the application."
                    .to_owned(),
            ));
        }
        (Family::Bytea, _) => return Err(undefined()),
    };
    Ok(Some(BlobOperator { lowering, result: blob_type }))
}

/// Emits the lowering [`resolve`] chose.
///
/// # Errors
///
/// Propagates the translation of either operand, the refusal a malformed
/// `bytea` literal earns, and the refusal for an operand a repeated form
/// cannot bind once.
pub(crate) fn translate(
    resolved: &BlobOperator,
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Expr, Error> {
    let mut side = |source: &Expr| -> Result<Expr, Error> {
        let translated = source.translate_with_warnings(schema, options, emit)?;
        match resolved.lowering {
            Lowering::BlobConcat(Family::Bytea) => convert_bytea_hex_literal(translated),
            Lowering::BlobConcat(Family::Vector) if single_quoted_literal(source).is_some() => {
                Ok(simple_function_expr("vec_f32", vec![translated], None))
            }
            _ => Ok(translated),
        }
    };
    let (left_sql, right_sql) = (side(left)?, side(right)?);
    match resolved.lowering {
        Lowering::BlobConcat(_) => {
            Ok(Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(Expr::Nested(Box::new(Expr::BinaryOp {
                    left: Box::new(left_sql),
                    op: BinaryOperator::StringConcat,
                    right: Box::new(right_sql),
                }))),
                data_type: DataType::Blob(None),
                format: None,
            })
        }
        Lowering::ByteaText { bytea_on_left } => {
            let (bytea_source, bytea_sql, text_sql) = if bytea_on_left {
                (left, left_sql, right_sql)
            } else {
                (right, right_sql, left_sql)
            };
            // `hex(NULL)` is the empty string, so the guard keeps NULL.
            let rendered = bind_once("||", [bytea_source], [bytea_sql], options, |[bytea]| {
                case_when(
                    Expr::IsNotNull(Box::new(bytea.clone())),
                    Expr::BinaryOp {
                        left: Box::new(string_literal("\\x")),
                        op: BinaryOperator::StringConcat,
                        right: Box::new(simple_function_expr(
                            "lower",
                            vec![simple_function_expr("hex", vec![bytea.clone()], None)],
                            None,
                        )),
                    },
                    None,
                )
            })?;
            let (left_sql, right_sql) =
                if bytea_on_left { (rendered, text_sql) } else { (text_sql, rendered) };
            Ok(Expr::BinaryOp {
                left: Box::new(left_sql),
                op: BinaryOperator::StringConcat,
                right: Box::new(right_sql),
            })
        }
        // sqlite-vec refuses a NULL argument, where pgvector's operators are
        // strict and answer NULL.
        Lowering::VectorArithmetic(function) => {
            bind_once(function, [left, right], [left_sql, right_sql], options, |[l, r]| {
                case_when(
                    Expr::BinaryOp {
                        left: Box::new(Expr::IsNotNull(Box::new(l.clone()))),
                        op: BinaryOperator::And,
                        right: Box::new(Expr::IsNotNull(Box::new(r.clone()))),
                    },
                    simple_function_expr(function, vec![l.clone(), r.clone()], None),
                    None,
                )
            })
        }
    }
}
