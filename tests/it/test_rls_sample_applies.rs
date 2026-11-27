//! End-to-end check that the playground's RLS sample translates and then
//! applies cleanly against an in-memory SQLite when a no-arg
//! `current_app_user()` UDF is registered returning `42` (matching how
//! the playground's `db::reopen` configures the connection). This pins
//! the user-visible behaviour so a regression in either the sample SQL
//! or the translator's trigger emission shows up as a normal test
//! failure rather than a "in-memory apply failed" toast.

use diesel::{Connection, connection::SimpleConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};

use crate::helpers::{Replica, SqliteAnyValue, query_values};

diesel::define_sql_function! {
    /// Returns the sample caller's identifier.
    fn current_app_user() -> diesel::sql_types::BigInt;
}

use crate::{
    helpers::deployment::{caller, deployment},
    sample_sql::RLS_SQL as RLS_SAMPLE,
};

fn translate_rls_sample() -> String {
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_deployment(deployment([], [caller("app.user_id", "integer")]))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "current_app_user"));
    let stmts = Pg2Sqlite::default()
        .sql(RLS_SAMPLE)
        .expect("parse")
        .translate(&opts)
        .expect("translate")
        .into_vec();
    stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n")
}

#[test]
fn rls_sample_applies_to_in_memory_sqlite() {
    let mut conn = Replica::establish(":memory:").expect("open in-memory db");
    current_app_user_utils::register_impl(
        conn.registration().expect("registration").sqlite(),
        || 42i64,
    )
    .expect("register current_app_user");

    let sql = translate_rls_sample();
    conn.batch_execute(&sql).expect("apply translated RLS sample");

    let visible_rows = match query_values(&mut conn, "SELECT COUNT(*) FROM documents", &[])
        .expect("count documents view")[0][0]
    {
        SqliteAnyValue::Integer(value) => value,
        ref value => panic!("expected integer, got {value:?}"),
    };
    assert_eq!(visible_rows, 2, "user 42 should see exactly the two seeded rows");

    let backing_rows = match query_values(&mut conn, "SELECT COUNT(*) FROM documents_rls", &[])
        .expect("count documents_rls backing table")[0][0]
    {
        SqliteAnyValue::Integer(value) => value,
        ref value => panic!("expected integer, got {value:?}"),
    };
    assert_eq!(backing_rows, 2, "backing table should hold the two seeded rows");
}

#[test]
fn rls_sample_blocks_disallowed_insert() {
    let mut conn = Replica::establish(":memory:").expect("open in-memory db");
    current_app_user_utils::register_impl(
        conn.registration().expect("registration").sqlite(),
        || 42i64,
    )
    .expect("register current_app_user");

    let sql = translate_rls_sample();
    conn.batch_execute(&sql).expect("apply translated RLS sample");

    let result = conn.execute(
        "INSERT INTO documents (id, owner_id, title) VALUES (?1, ?2, ?3)",
        &[
            SqliteAnyValue::Integer(99),
            SqliteAnyValue::Integer(99),
            SqliteAnyValue::Text("Owned by someone else".into()),
        ],
    );
    let err = result.expect_err("policy must reject foreign-owner insert");
    assert!(
        err.to_string().contains("new row violates row-level security policy"),
        "unexpected error: {err}"
    );
}
