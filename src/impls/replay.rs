//! Whether an expression may be written into more than one output position.
//!
//! Several lowerings name an operand twice: a guard plus a body, a sign test
//! plus a length, a prefix plus a suffix. PostgreSQL evaluates the operand
//! once, so the copies agree only while the operand answers the same value
//! every time it runs. A `random()`, a UUID call or a function this crate
//! cannot identify breaks that, and the emitted SQL then answers from a draw
//! PostgreSQL never made.

use alloc::{format, string::String, vec::Vec};
use core::ops::ControlFlow;

use sqlparser::ast::{Expr, Function, InTarget, Query, SetExpr, TableFactor, Visit, Visitor};

use crate::{
    errors::Error,
    impls::{session_variable, sqlite_functions::classify},
    options::TranslationContext,
};

/// The calls that answer a different value each time they run.
///
/// Sorted, and searched rather than matched. A clock function is absent on
/// purpose: `now()` and `current_timestamp` are fixed for the whole statement
/// in both engines, so naming one twice reads the same value twice.
/// `clock_timestamp` and `timeofday` are the two PostgreSQL spellings that
/// are not, and the sequence functions move a counter on every call.
const VOLATILE: &[&str] = &[
    "clock_timestamp",
    "currval",
    "gen_random_bytes",
    "gen_random_uuid",
    "lastval",
    "nextval",
    "pg_backend_pid",
    "random",
    "random_bytes",
    "random_normal",
    "randomblob",
    "setval",
    "statement_timestamp",
    "timeofday",
    "txid_current",
    "uuid_generate_v1",
    "uuid_generate_v4",
    "uuid_generate_v7",
    "uuidv4",
    "uuidv7",
];

/// True when evaluating `expr` a second time must answer what the first
/// evaluation answered.
///
/// Columns, literals and the operators over them replay; a call replays only
/// when this crate can name the function and knows it is not volatile, since
/// a function it cannot name may be anything the caller defined. A subquery
/// that reads a relation never replays: it may read rows another copy of the
/// same statement has already changed. One that reads none, a select over
/// derived tables or no source at all, replays when what it evaluates does.
pub(crate) fn is_replayable(expr: &Expr, options: &TranslationContext<'_>) -> bool {
    let mut check = ReplayCheck { options, replayable: true };
    let _: ControlFlow<()> = expr.visit(&mut check);
    check.replayable
}

/// Refuses a lowering that would name `operand` in more than one output
/// position.
pub(crate) fn reject_duplicated_operand(construct: &str, operand: &Expr) -> Error {
    Error::forward_refusal(format!(
        "{construct} has no SQLite form that reads {operand} once: the emitted expression names \
         it in more than one place, and this operand may answer a different value on each \
         evaluation, so the copies would disagree where PostgreSQL evaluates it once. Read the \
         value into a column or a variable first and name that, or compute {construct} in the \
         application."
    ))
}

struct ReplayCheck<'a, 'o> {
    options: &'a TranslationContext<'o>,
    replayable: bool,
}

impl ReplayCheck<'_, '_> {
    /// True when the call names a function this crate knows and knows to be
    /// deterministic.
    ///
    /// A reading of the session counts, a role keyword and a mapped setting
    /// under either of its names, ahead of the name inventories. A role is a
    /// literal once translated, and `current_setting` is STABLE in
    /// PostgreSQL, and row level security already reads the function the
    /// mapping names once per row, so a host whose function answered
    /// differently per call would already have an incoherent replica. A
    /// `current_setting` no mapping pairs stays unknown.
    fn call_replays(&self, function: &Function) -> bool {
        let Some(name) = crate::impls::object_name::last_ident(&function.name) else {
            return false;
        };
        let name = name.value.to_ascii_lowercase();
        let session = session_variable::session_read_of_function(function).is_some_and(|read| {
            match read {
                session_variable::SessionRead::Setting(setting) => {
                    self.options.find_session_variable(&setting).is_some()
                }
                session_variable::SessionRead::ActingRole(_)
                | session_variable::SessionRead::LoginRole => true,
            }
        });
        if session || session_variable::mapping_for_function(&name, self.options).is_some() {
            return true;
        }
        if VOLATILE.binary_search(&name.as_str()).is_ok() {
            return false;
        }
        if self.uuid_names().iter().any(|uuid| uuid.eq_ignore_ascii_case(&name)) {
            return false;
        }
        classify(&name).is_known()
    }

    /// The UUID function names this translation emits, which a caller names
    /// through the options and which answer a fresh value per call.
    fn uuid_names(&self) -> Vec<String> {
        let mut names = Vec::with_capacity(2);
        names.push(String::from(self.options.get_uuid_function_name()));
        if let Some(v7) = self.options.get_uuid_v7_function_name() {
            names.push(String::from(v7));
        }
        names
    }
}

impl Visitor for ReplayCheck<'_, '_> {
    type Break = ();

    fn post_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        let replays = match expr {
            Expr::Function(function) => self.call_replays(function),
            Expr::Subquery(query) => reads_no_relation(query),
            Expr::Exists { .. } | Expr::In { target: InTarget::Subquery(_), .. } => false,
            _ => true,
        };
        if replays {
            ControlFlow::Continue(())
        } else {
            self.replayable = false;
            ControlFlow::Break(())
        }
    }
}

/// Whether `query` reads no relation: it has no `WITH`, and every source of
/// every select is a derived table that reads none, or there is no source.
fn reads_no_relation(query: &Query) -> bool {
    query.with.is_none() && body_reads_no_relation(&query.body)
}

/// [`reads_no_relation`] for a query body.
fn body_reads_no_relation(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(select) => {
            select.from.iter().all(|source| {
                source.joins.is_empty()
                    && matches!(
                        &source.relation,
                        TableFactor::Derived { subquery, .. } if reads_no_relation(subquery)
                    )
            })
        }
        SetExpr::Query(query) => reads_no_relation(query),
        SetExpr::Values(_) => true,
        SetExpr::SetOperation { left, right, .. } => {
            body_reads_no_relation(left) && body_reads_no_relation(right)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

    use super::is_replayable;
    use crate::{options::TranslationContext, prelude::Pg2SqliteOptions};

    /// Whether the expression `sql` replays.
    fn replays(sql: &str) -> bool {
        let expr =
            Parser::new(&PostgreSqlDialect {}).try_with_sql(sql).unwrap().parse_expr().unwrap();
        is_replayable(&expr, &TranslationContext::from_owned(Pg2SqliteOptions::default()))
    }

    #[test]
    fn a_subquery_replays_only_when_it_reads_no_relation() {
        for sql in [
            "(SELECT 1)",
            "(SELECT x FROM (SELECT 1 AS x))",
            "(SELECT x FROM (SELECT 1 AS x) AS a, (SELECT 2 AS y) AS b)",
            "(SELECT column1 FROM (VALUES (1)))",
            "((SELECT 1))",
            "(SELECT 1 UNION SELECT 2)",
            "(SELECT x FROM ((SELECT 1 AS x) UNION (SELECT 2)) AS a)",
        ] {
            assert!(replays(sql), "{sql} reads no relation");
        }
        for sql in [
            "(SELECT x FROM t)",
            "(SELECT x FROM (SELECT x FROM t))",
            "(SELECT a.x FROM (SELECT 1 AS x) AS a JOIN (SELECT 1 AS x) AS b ON 1)",
            "(WITH w AS (SELECT 1 AS x) SELECT x FROM w)",
            "(SELECT 1 UNION SELECT x FROM t)",
            "(SELECT random())",
            "EXISTS (SELECT 1)",
            "1 IN (SELECT 1)",
            "(SELECT x FROM (TABLE t) AS a)",
        ] {
            assert!(!replays(sql), "{sql} may answer a different value");
        }
    }
}
