//! Every refusal a translated write guard raises is recognisable from the
//! message SQLite reports, and no other write error is.

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use diesel::{
    Connection as _, ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection,
    connection::SimpleConnection, result::Error,
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, is_policy_refusal};

const POLICY_SCHEMA: &str = "
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    body TEXT NOT NULL,
    doubled INTEGER GENERATED ALWAYS AS (id * 2) STORED
);
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
CREATE POLICY orders_select ON orders FOR SELECT USING (true);
CREATE POLICY orders_insert ON orders FOR INSERT WITH CHECK (owner = 'anna');
CREATE POLICY orders_update ON orders FOR UPDATE USING (true) WITH CHECK (owner = 'anna');
CREATE TABLE locked (id INTEGER PRIMARY KEY);
ALTER TABLE locked ENABLE ROW LEVEL SECURITY;
CREATE POLICY locked_select ON locked FOR SELECT USING (true);
";

const READ_ONLY_SCHEMA: &str = "
CREATE ROLE app_user;
CREATE TABLE reference_items (id INTEGER PRIMARY KEY);
CREATE TABLE secure_reference_items (id INTEGER PRIMARY KEY);
ALTER TABLE secure_reference_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY secure_reference_items_select ON secure_reference_items
    FOR SELECT USING (true);
GRANT SELECT ON reference_items TO app_user;
GRANT SELECT ON secure_reference_items TO app_user;
";

mod schema {
    diesel::table! {
        orders (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
            doubled -> Integer,
        }
    }
    diesel::table! {
        orders_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        locked (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        locked_rls (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        reference_items (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        secure_reference_items_rls (id) {
            id -> Integer,
        }
    }
}

diesel::define_sql_function! {
    /// Reports whether generated write guards are exempt.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

fn apply(pg: &str, options: &Pg2SqliteOptions) -> (SqliteConnection, Arc<AtomicBool>) {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    let exempt = Arc::new(AtomicBool::new(false));
    let function_state = Arc::clone(&exempt);
    write_is_exempt_utils::register_nondeterministic_impl(&mut connection, move || {
        function_state.load(Ordering::Relaxed)
    })
    .expect("register write exemption");
    let statements = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse PostgreSQL")
        .translate_to_sql(options)
        .expect("translate SQLite")
        .into_vec();
    for statement in statements {
        connection.batch_execute(&statement).expect("apply translated statement");
    }
    (connection, exempt)
}

fn policy_connection() -> (SqliteConnection, Arc<AtomicBool>) {
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_strict_rls_write_deny()
        .with_write_exemption_function("write_is_exempt");
    apply(POLICY_SCHEMA, &options)
}

fn read_only_connection() -> SqliteConnection {
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("app_user")
        .with_rls_audit_table_name("rls_audit")
        .with_write_exemption_function("write_is_exempt");
    apply(READ_ONLY_SCHEMA, &options).0
}

fn error_message(result: Result<usize, Error>, write: &str) -> String {
    match result {
        Err(Error::DatabaseError(_, information)) => information.message().to_owned(),
        other => panic!("{write} must fail with a database error, got {other:?}"),
    }
}

#[test]
fn every_guard_refusal_is_recognised() {
    use schema::{locked, locked_rls, orders, orders_rls, reference_items};

    let (mut connection, exempt) = policy_connection();
    diesel::insert_into(orders::table)
        .values((orders::id.eq(1), orders::owner.eq("anna"), orders::body.eq("open")))
        .execute(&mut connection)
        .expect("an insert the policy admits");
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(locked_rls::table)
        .values(locked_rls::id.eq(1))
        .execute(&mut connection)
        .expect("an exempt insert into the backing table");
    exempt.store(false, Ordering::Relaxed);

    let refusals = [
        (
            "an insert failing WITH CHECK",
            diesel::insert_into(orders::table)
                .values((orders::id.eq(2), orders::owner.eq("mallory"), orders::body.eq("open")))
                .execute(&mut connection),
        ),
        (
            "a backing-table insert failing WITH CHECK",
            diesel::insert_into(orders_rls::table)
                .values((
                    orders_rls::id.eq(2),
                    orders_rls::owner.eq("mallory"),
                    orders_rls::body.eq("open"),
                ))
                .execute(&mut connection),
        ),
        (
            "an update failing WITH CHECK",
            diesel::update(orders::table.find(1))
                .set(orders::owner.eq("mallory"))
                .execute(&mut connection),
        ),
        (
            "a backing-table update failing WITH CHECK",
            diesel::update(orders_rls::table.find(1))
                .set(orders_rls::owner.eq("mallory"))
                .execute(&mut connection),
        ),
        ("a delete without a DELETE policy", {
            diesel::delete(orders::table.find(1)).execute(&mut connection)
        }),
        ("an insert without an INSERT policy", {
            diesel::insert_into(locked::table).values(locked::id.eq(2)).execute(&mut connection)
        }),
        ("a backing-table insert without an INSERT policy", {
            diesel::insert_into(locked_rls::table)
                .values(locked_rls::id.eq(2))
                .execute(&mut connection)
        }),
        ("an update without an UPDATE policy", {
            diesel::update(locked::table.find(1)).set(locked::id.eq(3)).execute(&mut connection)
        }),
    ];

    let mut read_only = read_only_connection();
    let read_only_refusals = [
        ("an insert into a read-only table", {
            diesel::insert_into(reference_items::table)
                .values(reference_items::id.eq(1))
                .execute(&mut read_only)
        }),
        ("an insert into a read-only RLS backing table", {
            diesel::insert_into(schema::secure_reference_items_rls::table)
                .values(schema::secure_reference_items_rls::id.eq(1))
                .execute(&mut read_only)
        }),
    ];

    for (write, result) in refusals.into_iter().chain(read_only_refusals) {
        let message = error_message(result, write);
        assert!(is_policy_refusal(&message), "{write} raised an unrecognised refusal: {message}");
    }
}

#[test]
fn other_write_errors_are_not_policy_refusals() {
    use schema::orders;

    let (mut connection, _) = policy_connection();
    let errors = [
        ("an insert missing a NOT NULL column", {
            diesel::insert_into(orders::table)
                .values((orders::id.eq(1), orders::owner.eq("anna")))
                .execute(&mut connection)
        }),
        ("an insert writing a generated column", {
            diesel::insert_into(orders::table)
                .values((
                    orders::id.eq(1),
                    orders::owner.eq("anna"),
                    orders::body.eq("open"),
                    orders::doubled.eq(5),
                ))
                .execute(&mut connection)
        }),
    ];

    for (write, result) in errors {
        let message = error_message(result, write);
        assert!(!is_policy_refusal(&message), "{write} was taken for a refusal: {message}");
    }
}
