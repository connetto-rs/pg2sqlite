//! Whether an expression may be written into more than one output position.
//!
//! Several lowerings name an operand twice: a guard plus a body, a sign test
//! plus a length, a prefix plus a suffix. PostgreSQL evaluates the operand
//! once, so the copies agree only while the operand answers the same value
//! every time it runs. A `random()`, a UUID call or a function this crate
//! cannot identify breaks that, and the emitted SQL then answers from a draw
//! PostgreSQL never made.

use core::ops::ControlFlow;

use sqlparser::ast::{Expr, Function, InTarget, Query, SetExpr, TableFactor, Visit, Visitor};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{session_variable, sqlite_functions::classify},
    options::TranslationContext,
};

/// The calls that answer a different value each time they run.
///
/// Sorted, and searched rather than matched. A clock function is absent on
/// purpose: `now()`, `current_timestamp` and `statement_timestamp()` are
/// fixed for the whole statement in both engines, so naming one twice reads
/// the same value twice. `clock_timestamp` and `timeofday` are the two
/// PostgreSQL spellings that are not, `pg2sqlite_clock_timestamp` is the
/// reader the first becomes, and the sequence functions move a counter on
/// every call.
const VOLATILE: &[&str] = &[
    "clock_timestamp",
    "currval",
    "gen_random_bytes",
    "gen_random_uuid",
    "lastval",
    "nextval",
    "pg2sqlite_clock_timestamp",
    "pg_backend_pid",
    "random",
    "random_bytes",
    "random_normal",
    "randomblob",
    "setval",
    "timeofday",
    "txid_current",
    "uuid_generate_v1",
    "uuid_generate_v4",
    "uuid_generate_v7",
    "uuidv4",
    "uuidv7",
];

/// Whether a built-in or configured function can change between calls.
pub(crate) fn is_volatile_call(name: &str, options: &TranslationContext<'_>) -> bool {
    VOLATILE.binary_search(&name).is_ok()
        || name.eq_ignore_ascii_case(options.get_uuid_function_name())
        || options
            .get_uuid_v7_function_name()
            .is_some_and(|name_v7| name.eq_ignore_ascii_case(name_v7))
        || options.declares_user_defined_function(name)
}

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

/// True when every call `query` makes answers what it answered the first
/// time, the relations it reads aside, so reading it again in the same
/// statement reads the same rows wherever the relations it reads do.
pub(crate) fn calls_replay(query: &Query, options: &TranslationContext<'_>) -> bool {
    let mut check = CallCheck { check: ReplayCheck { options, replayable: true } };
    let _: ControlFlow<()> = query.visit(&mut check);
    check.check.replayable
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
    /// A reading of the session counts, a role keyword, a mapped setting
    /// under either of its names and a server setting the replica answers,
    /// ahead of the name inventories. A role and a server setting are
    /// literals once translated, and `current_setting` is STABLE in
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
                session_variable::SessionRead::ServerSetting(setting) => {
                    session_variable::server_setting_answer(&setting).is_some()
                }
                session_variable::SessionRead::ActingRole(_)
                | session_variable::SessionRead::LoginRole => true,
            }
        });
        if session || session_variable::mapping_for_function(&name, self.options).is_some() {
            return true;
        }
        if self.options.get_temporal_functions().is_some_and(|functions| {
            crate::impls::reverse_translator_impls::temporal::classify_name(&name, functions)
                .is_some()
        }) {
            return true;
        }
        if is_volatile_call(&name, self.options) {
            return false;
        }
        classify(&name).is_known()
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

struct CallCheck<'a, 'o> {
    check: ReplayCheck<'a, 'o>,
}

impl CallCheck<'_, '_> {
    fn found(&mut self, replays: bool) -> ControlFlow<()> {
        if replays {
            ControlFlow::Continue(())
        } else {
            self.check.replayable = false;
            ControlFlow::Break(())
        }
    }
}

impl Visitor for CallCheck<'_, '_> {
    type Break = ();

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        let replays = !matches!(
            factor,
            TableFactor::Table { args: Some(_), .. }
                | TableFactor::Function { .. }
                | TableFactor::TableFunction { .. }
        );
        self.found(replays)
    }

    fn post_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        let replays = match expr {
            Expr::Function(function) => self.check.call_replays(function),
            _ => true,
        };
        self.found(replays)
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
