//! Conflict updates resolve the incoming row throughout their expressions.

use crate::helpers::run_translated::run_translated_default;

#[test]
fn excluded_expression_arithmetic() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE t (k INTEGER PRIMARY KEY, n INTEGER); INSERT INTO t VALUES (1, 1); INSERT INTO t VALUES (1, 10) ON CONFLICT (k) DO UPDATE SET n = excluded.n + 5; SELECT n FROM t;"
        ),
        vec![Some("15".to_owned())]
    );
}

#[test]
fn excluded_expression_existing_row() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE t (k INTEGER PRIMARY KEY, n INTEGER); INSERT INTO t VALUES (1, 15); INSERT INTO t VALUES (1, 10) ON CONFLICT (k) DO UPDATE SET n = t.n + excluded.n; SELECT n FROM t;"
        ),
        vec![Some("25".to_owned())]
    );
}

#[test]
fn excluded_expression_function() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE t (k INTEGER PRIMARY KEY, n INTEGER); INSERT INTO t VALUES (1, 25); INSERT INTO t VALUES (1, -10) ON CONFLICT (k) DO UPDATE SET n = abs(excluded.n); SELECT n FROM t;"
        ),
        vec![Some("10".to_owned())]
    );
}

#[test]
fn excluded_expression_predicate() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE t (k INTEGER PRIMARY KEY, n INTEGER); INSERT INTO t VALUES (1, 10); INSERT INTO t VALUES (1, 20) ON CONFLICT (k) DO UPDATE SET n = excluded.n WHERE excluded.n > t.n; SELECT n FROM t;"
        ),
        vec![Some("20".to_owned())]
    );
}
