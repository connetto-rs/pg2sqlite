//! A statement naming a temporary table without a schema writes the table
//! `pg_temp` holds, which PostgreSQL searches before any other schema, so the
//! table's columns, defaults and types shape the write as they would for a
//! permanent one. The expected rows are PostgreSQL 18's.

use crate::helpers::run_translated::run_translated_default;

#[test]
fn a_write_into_a_temporary_table_follows_its_columns() {
    assert_eq!(
        run_translated_default(
            "CREATE TEMP TABLE t (a INTEGER, b TEXT DEFAULT 'd', c BOOLEAN);
             INSERT INTO t VALUES (1);
             INSERT INTO t VALUES (2, DEFAULT, 'yes');
             INSERT INTO t SELECT 3;
             UPDATE t SET c = 'off' WHERE a = 1;
             SELECT a || ' ' || b || ' ' || coalesce(c::text, 'null') FROM t ORDER BY a;"
        ),
        [Some("1 d false".to_owned()), Some("2 d true".to_owned()), Some("3 d null".to_owned())]
    );
}
