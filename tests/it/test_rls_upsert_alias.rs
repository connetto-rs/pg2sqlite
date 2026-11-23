//! Aliased conflict updates resolve both rows under strict row-level security.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::{helpers, helpers::deployment::deployment};

diesel::table! {
    items (id) {
        id -> Integer,
        owner -> Text,
        name -> Text,
        val -> Integer,
    }
}

const SCHEMA: &str = "
    CREATE ROLE alias_actor;
    CREATE TABLE items (
        id INTEGER PRIMARY KEY,
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        val INTEGER NOT NULL
    );
    ALTER TABLE items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY items_select ON items FOR SELECT USING (owner = current_user);
    CREATE POLICY items_insert ON items FOR INSERT WITH CHECK (owner = current_user);
    CREATE POLICY items_update ON items FOR UPDATE USING (owner = current_user);
    GRANT SELECT, INSERT, UPDATE ON items TO alias_actor;
";

const UPSERT: &str = "
    INSERT INTO items AS x VALUES
        (1, 'alias_actor', 'new', 20),
        (2, 'alias_actor', 'skip', 20),
        (3, 'alias_actor', 'fresh', 40)
    ON CONFLICT (id) DO UPDATE
        SET name = x.name || excluded.name, val = excluded.val
        WHERE x.val < excluded.val;
";

fn assert_upsert(options: &Pg2SqliteOptions) {
    let mut connection = helpers::apply_translated_ddl(SCHEMA, options);
    diesel::insert_into(items::table)
        .values([
            (
                items::id.eq(1),
                items::owner.eq("alias_actor"),
                items::name.eq("old"),
                items::val.eq(10),
            ),
            (
                items::id.eq(2),
                items::owner.eq("alias_actor"),
                items::name.eq("keep"),
                items::val.eq(30),
            ),
        ])
        .execute(&mut connection)
        .expect("seed visible rows");
    let schema_count = Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("parse schema")
        .translate(options)
        .expect("translate schema")
        .into_vec()
        .len();
    let translated = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}\n{UPSERT}"))
        .expect("parse upsert")
        .translate(options)
        .expect("translate aliased upsert")
        .into_vec();
    for statement in translated.into_iter().skip(schema_count) {
        connection.batch_execute(&statement.to_string()).expect("execute translated upsert");
    }
    let rows = items::table
        .order(items::id)
        .load::<(i32, String, String, i32)>(&mut connection)
        .expect("read visible rows");
    assert_eq!(
        rows,
        vec![
            (1, "alias_actor".into(), "oldnew".into(), 20),
            (2, "alias_actor".into(), "keep".into(), 30),
            (3, "alias_actor".into(), "fresh".into(), 40),
        ]
    );
}

fn strict_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_strict_rls_validation()
        .with_deployment(deployment(["alias_actor"], []))
        .with_acting_role("alias_actor")
        .with_rls_audit_table_name("rls_audit")
}

#[test]
fn aliased_upsert_default_backing_suffix() {
    assert_upsert(&strict_options());
}

#[test]
fn aliased_upsert_custom_backing_suffix() {
    assert_upsert(&strict_options().with_rls_table_suffix("_secure"));
}
