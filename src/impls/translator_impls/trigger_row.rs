//! The whole row a row trigger reads as `NEW` or `OLD`.
//!
//! SQLite reads a trigger's rows one column at a time, `NEW.c`, and has no
//! value for the row itself. Every use of a whole row PostgreSQL makes in a
//! trigger is a function of its columns, so each is rewritten over them, in
//! PostgreSQL, before translation: a JSON object of the row is the matching
//! constructor over its columns, and two rows compare column by column. The
//! rewritten expression is then translated as written, so each column converts
//! and compares as that column read on its own does.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{BinaryOperator, Expr, Ident, TriggerEvent, Value};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::function_helpers::{simple_function_expr, string_literal},
};

/// Which of a row trigger's two rows a name reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PseudoRow {
    New,
    Old,
}

impl PseudoRow {
    /// The row `name` reads, under PostgreSQL's folding of an identifier.
    fn named(name: &Ident) -> Option<Self> {
        let reads = |row: &str| {
            if name.quote_style.is_some() {
                name.value == row
            } else {
                name.value.eq_ignore_ascii_case(row)
            }
        };
        if reads("new") {
            Some(Self::New)
        } else if reads("old") {
            Some(Self::Old)
        } else {
            None
        }
    }

    const fn keyword(self) -> &'static str {
        match self {
            Self::New => "NEW",
            Self::Old => "OLD",
        }
    }
}

/// The table a row trigger fires on, and which of `NEW` and `OLD` its events
/// give it.
#[derive(Clone, Copy)]
pub(crate) struct TriggerRows<'a> {
    table: &'a <ParserDB as DatabaseLike>::Table,
    new: bool,
    old: bool,
}

impl<'a> TriggerRows<'a> {
    /// The rows a trigger on `table` firing for `events` reads.
    pub(crate) fn new(
        table: &'a <ParserDB as DatabaseLike>::Table,
        events: &[TriggerEvent],
    ) -> Self {
        let any = |wanted: fn(&TriggerEvent) -> bool| events.iter().any(wanted);
        Self {
            table,
            new: any(|event| matches!(event, TriggerEvent::Insert | TriggerEvent::Update(_))),
            old: any(|event| matches!(event, TriggerEvent::Update(_) | TriggerEvent::Delete)),
        }
    }

    /// The same table, read by the part of the trigger that fires for
    /// `events`.
    pub(crate) fn for_events(self, events: &[TriggerEvent]) -> Self {
        Self::new(self.table, events)
    }

    /// The whole row `expr` reads, or `None` when it reads something else.
    pub(crate) fn row(self, expr: &Expr) -> Option<TriggerRow<'a>> {
        let mut expr = expr;
        while let Expr::Nested(inner) = expr {
            expr = inner;
        }
        let Expr::Identifier(name) = expr else { return None };
        let which = PseudoRow::named(name)?;
        let present = match which {
            PseudoRow::New => self.new,
            PseudoRow::Old => self.old,
        };
        Some(TriggerRow { rows: self, which, present })
    }
}

/// One of a row trigger's two rows, read whole.
pub(crate) struct TriggerRow<'a> {
    rows: TriggerRows<'a>,
    which: PseudoRow,
    /// Whether the event gives the trigger this row. PostgreSQL reads `OLD`
    /// in an insert and `NEW` in a delete as `NULL`, and SQLite refuses to
    /// prepare a trigger that reads a column of either.
    present: bool,
}

impl TriggerRow<'_> {
    /// The reference that reads `column` from this row, spelled as the column
    /// was declared.
    fn reference(&self, column: &<ParserDB as DatabaseLike>::Column) -> Expr {
        let name = if column.column_name_is_quoted() {
            Ident::with_quote('"', column.column_name())
        } else {
            Ident::new(column.column_name())
        };
        Expr::CompoundIdentifier(vec![Ident::new(self.which.keyword()), name])
    }

    /// `constructor` over every column of the row, keyed by the name
    /// PostgreSQL stores for the column, which is the object PostgreSQL makes
    /// of a row, or `NULL` for a row the event does not give.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema cannot list the table's columns.
    pub(crate) fn json_object(&self, constructor: &str, schema: &ParserDB) -> Result<Expr, Error> {
        if !self.present {
            return Ok(Expr::Value(Value::Null.into()));
        }
        let arguments = self
            .rows
            .table
            .columns(schema)?
            .flat_map(|column| {
                [string_literal(&column.stored_column_name()), self.reference(column)]
            })
            .collect();
        Ok(simple_function_expr(constructor, arguments, None))
    }

    /// `self IS [NOT] DISTINCT FROM other`.
    ///
    /// PostgreSQL compares two rows of one type field by field, a NULL field
    /// equal to a NULL field, so two rows are distinct when any column is, and
    /// a table with no columns has rows that are never distinct. A row the
    /// event does not give is `NULL` as a whole, distinct from any row the
    /// event gives, even one whose every field is `NULL`, measured on 18.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema cannot list the table's columns.
    pub(crate) fn distinct(
        &self,
        other: &Self,
        is_not_distinct: bool,
        schema: &ParserDB,
    ) -> Result<Expr, Error> {
        if !self.present || !other.present {
            let same = self.present == other.present;
            return Ok(Expr::Value(Value::Boolean(same == is_not_distinct).into()));
        }
        let columns = self.rows.table.columns(schema)?.map(|column| {
            let (left, right) =
                (Box::new(self.reference(column)), Box::new(other.reference(column)));
            Expr::Nested(Box::new(if is_not_distinct {
                Expr::IsNotDistinctFrom(left, right)
            } else {
                Expr::IsDistinctFrom(left, right)
            }))
        });
        Ok(columns
            .reduce(|left, right| {
                Expr::BinaryOp {
                    left: Box::new(left),
                    op: if is_not_distinct { BinaryOperator::And } else { BinaryOperator::Or },
                    right: Box::new(right),
                }
            })
            .unwrap_or_else(|| Expr::Value(Value::Boolean(is_not_distinct).into())))
    }

    /// The refusal of this row read anywhere it has no rewrite.
    pub(crate) fn refusal(&self) -> Error {
        Error::forward_refusal(format!(
            "`{keyword}` reads the whole row of `{table}`, and SQLite has no row value, only its \
             columns. A whole row translates as the argument of `to_json`, `to_jsonb` or \
             `row_to_json`, and compared with the other row by `IS DISTINCT FROM` or `IS NOT \
             DISTINCT FROM`. Elsewhere read its columns, as `{keyword}.<column>`.",
            keyword = self.which.keyword(),
            table = self.rows.table.table_name(),
        ))
    }
}
