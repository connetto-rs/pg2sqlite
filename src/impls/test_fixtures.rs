//! Fixtures shared by the `#[cfg(test)]` modules under `src/`.

#[cfg(not(feature = "std"))]
use alloc::{string::ToString, vec, vec::Vec};

use sql_traits::structs::{ColumnScope, IdentifierCase, ParserDB};
use sqlparser::{
    ast::{Insert, LimitClause, Offset, OffsetRows, Query, Select, SetExpr, Statement},
    dialect::PostgreSqlDialect,
    parser::Parser,
};

/// The schema built from no statements.
pub(crate) fn empty_schema() -> ParserDB {
    ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
}

/// Parses `sql` as an expression with the PostgreSQL dialect.
pub(crate) fn parse_expr(sql: &str) -> sqlparser::ast::Expr {
    Parser::new(&PostgreSqlDialect {}).try_with_sql(sql).unwrap().parse_expr().unwrap()
}

/// Parses `sql` and requires its first statement to be a query.
pub(crate) fn parse_query(sql: &str) -> Query {
    let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0);
    match stmt {
        Statement::Query(query) => *query,
        other => panic!("expected query, got: {other:?}"),
    }
}

/// Parses `sql` and requires its first statement to be an `INSERT`.
pub(crate) fn parse_insert(sql: &str) -> Insert {
    let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse").remove(0);
    let Statement::Insert(insert) = stmt else {
        panic!("expected insert");
    };
    insert
}

/// The `SELECT` body of `query`, panicking when the body is not a plain select.
pub(crate) fn select_mut(query: &mut Query) -> &mut Select {
    let SetExpr::Select(select) = query.body.as_mut() else {
        panic!("expected select");
    };
    select
}

/// A `LIMIT` clause carrying an `OFFSET` and a `LIMIT BY` entry.
pub(crate) fn limit_offset_clause() -> LimitClause {
    LimitClause::LimitOffset {
        limit: Some(parse_expr("10")),
        offset: Some(Offset { value: parse_expr("1"), rows: OffsetRows::None }),
        limit_by: vec![parse_expr("2")],
    }
}

/// Runs `f` with the schema built from `schema_sql` and options scoped to
/// `query_sql`.
pub(crate) fn with_scoped_options<F, T>(schema_sql: &str, query_sql: &str, f: F) -> T
where
    F: for<'a> FnOnce(&'a ParserDB, &crate::options::TranslationContext<'a>) -> T,
{
    let schema = ParserDB::parse::<PostgreSqlDialect>(schema_sql).unwrap();
    let options =
        crate::options::TranslationContext::from_owned(crate::prelude::Pg2SqliteOptions::default());
    let query =
        Parser::new(&PostgreSqlDialect {}).try_with_sql(query_sql).unwrap().parse_query().unwrap();
    let scope = ColumnScope::from_query(&query, &schema, IdentifierCase::AsWritten).unwrap();
    let scoped = options.with_scope(&scope);
    f(&schema, &scoped)
}
