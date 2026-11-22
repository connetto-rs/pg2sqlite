#![allow(missing_docs)]
//! Finding 2: an INSERT through an RLS view must succeed when the table has a
//! BEFORE INSERT maintenance trigger that mutates NEW but no UPDATE policy.
//! PostgreSQL ground truth: INSERT (1,'a') → (1,'a*'), DELETE succeeds.

use diesel::{Connection, QueryResult, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers;

// The RLS view is the user-visible surface; backing table is for result reads.
diesel::table! {
    rts (id) {
        id -> Integer,
        tag -> Nullable<Text>,
    }
}

diesel::table! {
    rts_rls (id) {
        id -> Integer,
        tag -> Nullable<Text>,
    }
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = rts_rls)]
struct RtsRow {
    id: i32,
    tag: Option<String>,
}

/// SELECT, INSERT (id>0), DELETE policies; no UPDATE policy.
const RTS_SCHEMA: &str = "
    CREATE TABLE rts (id INT PRIMARY KEY, tag TEXT);
    ALTER TABLE rts ENABLE ROW LEVEL SECURITY;
    CREATE POLICY rts_sel ON rts FOR SELECT USING (true);
    CREATE POLICY rts_ins ON rts FOR INSERT WITH CHECK (id > 0);
    CREATE POLICY rts_del ON rts FOR DELETE USING (true);
    CREATE FUNCTION rts_tag() RETURNS trigger AS $$
    BEGIN
        NEW.tag := NEW.tag || '*';
        RETURN NEW;
    END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER rts_tag_trg BEFORE INSERT ON rts FOR EACH ROW EXECUTE FUNCTION rts_tag();
";

fn insert_via_view(conn: &mut SqliteConnection, id: i32, tag: &str) -> QueryResult<usize> {
    // INSERT goes through the INSTEAD OF INSERT trigger on the RLS view.
    diesel::sql_query(format!("INSERT INTO rts (id, tag) VALUES ({id}, '{tag}')")).execute(conn)
}

/// PostgreSQL: INSERT (1,'a') succeeds and tag becomes 'a*'.
#[test]
fn insert_through_rls_view_with_before_insert_trigger_applies_mutation() {
    let mut conn = helpers::apply_translated_ddl(
        RTS_SCHEMA,
        &Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"),
    );
    insert_via_view(&mut conn, 1, "a").expect("INSERT must succeed");

    let rows = rts_rls::table
        .select(RtsRow::as_select())
        .load::<RtsRow>(&mut conn)
        .expect("select from backing table");

    assert_eq!(rows.len(), 1, "one row expected");
    assert_eq!(rows[0].id, 1);
    assert_eq!(rows[0].tag.as_deref(), Some("a*"), "trigger must have appended '*'");
}

/// The INSERT policy (id > 0) must still reject id = 0.
#[test]
fn insert_policy_still_enforced_when_trigger_is_present() {
    let mut conn = helpers::apply_translated_ddl(
        RTS_SCHEMA,
        &Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"),
    );
    let result = insert_via_view(&mut conn, 0, "x");
    assert!(result.is_err(), "id=0 violates INSERT WITH CHECK (id > 0)");
}

/// DELETE through the view must succeed after the INSERT.
#[test]
fn delete_after_insert_succeeds() {
    let mut conn = helpers::apply_translated_ddl(
        RTS_SCHEMA,
        &Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"),
    );
    insert_via_view(&mut conn, 1, "a").expect("insert");
    diesel::sql_query("DELETE FROM rts WHERE id = 1")
        .execute(&mut conn)
        .expect("DELETE through view must succeed");

    let count: i64 = rts_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 0);
}

/// Direct UPDATE to the backing table must still be denied when there is no
/// UPDATE policy; the maintenance exemption does not cover caller writes.
#[test]
fn direct_backing_update_denied_without_update_policy() {
    let mut conn = helpers::apply_translated_ddl(
        RTS_SCHEMA,
        &Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"),
    );
    insert_via_view(&mut conn, 1, "a").expect("insert");

    // This is a caller update directly to the backing table, not a maintenance
    // write; a different tag value ensures the exemption WHEN clause does not
    // match.
    let result =
        diesel::sql_query("UPDATE rts_rls SET tag = 'evil' WHERE id = 1").execute(&mut conn);
    assert!(result.is_err(), "direct backing UPDATE must be denied when no UPDATE policy exists");
}

// ── Two-column BEFORE INSERT maintenance trigger ──

diesel::table! {
    items (id) {
        id -> Integer,
        tag -> Nullable<Text>,
        slug -> Nullable<Text>,
    }
}

diesel::table! {
    items_rls (id) {
        id -> Integer,
        tag -> Nullable<Text>,
        slug -> Nullable<Text>,
    }
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = items_rls)]
struct ItemRow {
    tag: Option<String>,
    slug: Option<String>,
}

/// SELECT and INSERT policies; BEFORE INSERT trigger maintains two columns (tag
/// and slug).
const ITEMS_SCHEMA: &str = "
    CREATE TABLE items (id INT PRIMARY KEY, tag TEXT, slug TEXT);
    ALTER TABLE items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY items_sel ON items FOR SELECT USING (true);
    CREATE POLICY items_ins ON items FOR INSERT WITH CHECK (true);
    CREATE FUNCTION items_maintain() RETURNS TRIGGER AS $$
    BEGIN
        NEW.tag := NEW.tag || '!';
        NEW.slug := upper(NEW.slug);
        RETURN NEW;
    END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER items_maint_trg BEFORE INSERT ON items
    FOR EACH ROW EXECUTE FUNCTION items_maintain();
";

/// The INSERT succeeds because the backing-table guard lets the maintenance
/// trigger's own UPDATE through.
#[test]
fn rls_two_column_maintenance_trigger_exemption_fires() {
    // Execute: INSERT through the view must succeed.
    let mut conn = helpers::apply_translated_ddl(
        ITEMS_SCHEMA,
        &helpers::case_fold_functions::with_functions(Pg2SqliteOptions::default())
            .with_rls_audit_table_name("rls_audit"),
    );
    diesel::insert_into(items::table)
        .values((items::id.eq(1_i32), items::tag.eq("hello"), items::slug.eq("world")))
        .execute(&mut conn)
        .expect("INSERT through RLS view must succeed");
    let row = items_rls::table
        .filter(items_rls::id.eq(1))
        .select(ItemRow::as_select())
        .first(&mut conn)
        .expect("row must be readable from backing table");
    assert_eq!(row.tag.as_deref(), Some("hello!"), "tag must be maintained");
    assert_eq!(row.slug.as_deref(), Some("WORLD"), "slug must be maintained");
}

/// A caller UPDATE of the backing table stays denied without an UPDATE policy
/// even when it writes exactly the values the maintenance trigger computes.
#[test]
fn caller_update_writing_the_maintained_values_is_denied() {
    let mut conn = helpers::apply_translated_ddl(
        ITEMS_SCHEMA,
        &helpers::case_fold_functions::with_functions(Pg2SqliteOptions::default())
            .with_rls_audit_table_name("rls_audit"),
    );
    diesel::insert_into(items::table)
        .values((items::id.eq(1_i32), items::tag.eq("hello"), items::slug.eq("world")))
        .execute(&mut conn)
        .expect("INSERT through RLS view must succeed");
    let result = diesel::update(items_rls::table.find(1))
        .set((items_rls::tag.eq(items_rls::tag.concat("!")), items_rls::slug.eq("WORLD")))
        .execute(&mut conn);
    assert!(result.is_err(), "a caller UPDATE must be denied without an UPDATE policy");
}

diesel::table! {
    guarded (id) {
        id -> Integer,
        tag -> Nullable<Text>,
    }
}

diesel::table! {
    guarded_rls (id) {
        id -> Integer,
        tag -> Nullable<Text>,
    }
}

/// An `UPDATE` policy admitting no row, beside a `BEFORE INSERT` maintenance
/// trigger.
const GUARDED_SCHEMA: &str = "
    CREATE TABLE guarded (id INT PRIMARY KEY, tag TEXT);
    ALTER TABLE guarded ENABLE ROW LEVEL SECURITY;
    CREATE POLICY guarded_sel ON guarded FOR SELECT USING (true);
    CREATE POLICY guarded_ins ON guarded FOR INSERT WITH CHECK (true);
    CREATE POLICY guarded_upd ON guarded FOR UPDATE USING (false);
    CREATE FUNCTION guarded_tag() RETURNS trigger AS $$
    BEGIN
        NEW.tag := NEW.tag || '*';
        RETURN NEW;
    END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER guarded_tag_trg BEFORE INSERT ON guarded
        FOR EACH ROW EXECUTE FUNCTION guarded_tag();
";

/// PostgreSQL runs no `UPDATE` for a `BEFORE INSERT` trigger, so the insert
/// answers to the `INSERT` policy alone and stores `a*`, while a caller
/// `UPDATE` stays held to the `UPDATE` policy.
#[test]
fn maintenance_write_passes_an_update_policy_refusing_the_row() {
    let mut conn = helpers::apply_translated_ddl(
        GUARDED_SCHEMA,
        &Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"),
    );
    diesel::insert_into(guarded::table)
        .values((guarded::id.eq(1_i32), guarded::tag.eq("a")))
        .execute(&mut conn)
        .expect("INSERT through RLS view must succeed");
    let tag: Option<String> =
        guarded_rls::table.find(1).select(guarded_rls::tag).first(&mut conn).expect("select");
    assert_eq!(tag.as_deref(), Some("a*"));

    let result =
        diesel::update(guarded_rls::table.find(1)).set(guarded_rls::tag.eq("b")).execute(&mut conn);
    assert!(result.is_err(), "a caller UPDATE must answer to the UPDATE policy");
}

diesel::table! {
    priced (id) {
        id -> Integer,
        price -> Nullable<BigInt>,
    }
}

diesel::table! {
    priced_rls (id) {
        id -> Integer,
        price -> Nullable<BigInt>,
    }
}

/// The maintenance write stores `1.25` in a `NUMERIC(10,2)` column as the
/// scaled integer 125, so the exemption letting it past the missing `UPDATE`
/// policy has to expect 125. Expecting `1.25` denies the trigger's own write
/// and with it the insert. PostgreSQL stores 1.25.
#[test]
fn rls_maintenance_exemption_expects_the_scaled_numeric() {
    let mut conn = helpers::apply_translated_ddl(
        "CREATE TABLE priced (id INT PRIMARY KEY, price NUMERIC(10,2));
         ALTER TABLE priced ENABLE ROW LEVEL SECURITY;
         CREATE POLICY priced_sel ON priced FOR SELECT USING (true);
         CREATE POLICY priced_ins ON priced FOR INSERT WITH CHECK (true);
         CREATE FUNCTION fix_price() RETURNS trigger AS $$
         BEGIN
             NEW.price := 1.25;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER priced_fix BEFORE INSERT ON priced
             FOR EACH ROW EXECUTE FUNCTION fix_price();",
        &Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"),
    );
    diesel::insert_into(priced::table)
        .values(priced::id.eq(1))
        .execute(&mut conn)
        .expect("INSERT through RLS view must succeed");
    let price: Option<i64> =
        priced_rls::table.find(1).select(priced_rls::price).first(&mut conn).expect("select");
    assert_eq!(price, Some(125));
}

// ── Non-maintenance BEFORE INSERT trigger
// ─────────────────────────────────────

diesel::table! {
    notes (id) {
        id -> Integer,
        body -> Nullable<Text>,
    }
}

diesel::table! {
    notes_rls (id) {
        id -> Integer,
        body -> Nullable<Text>,
    }
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = notes_rls)]
struct NoteRow {
    body: Option<String>,
}

/// RLS table with a BEFORE INSERT trigger that does NOT assign to NEW, so it
/// is no maintenance trigger and the update-check trigger lets no write of it
/// through. INSERT through the view must still succeed.
const NOTES_SCHEMA: &str = "
    CREATE TABLE notes (id INT PRIMARY KEY, body TEXT);
    ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
    CREATE POLICY notes_sel ON notes FOR SELECT USING (true);
    CREATE POLICY notes_ins ON notes FOR INSERT WITH CHECK (true);
    CREATE FUNCTION notes_passthru() RETURNS TRIGGER AS $$
    BEGIN
        RETURN NEW;
    END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER notes_trg BEFORE INSERT ON notes
    FOR EACH ROW EXECUTE FUNCTION notes_passthru();
";

/// The non-maintenance trigger gets no exemption, so no IS DISTINCT FROM
/// appears in the update-check trigger, and INSERT succeeds.
#[test]
fn rls_non_maintenance_before_insert_trigger_has_no_exemption() {
    let opts = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let stmts = Pg2Sqlite::default()
        .sql(NOTES_SCHEMA)
        .expect("parse")
        .translate_to_sql(&opts)
        .expect("translate")
        .into_vec();
    let update_check = stmts
        .iter()
        .map(ToString::to_string)
        .find(|s| s.contains("update_check"))
        .expect("update-check trigger must be emitted");
    assert!(
        !update_check.contains("IS DISTINCT FROM"),
        "non-maintenance trigger must not produce an IS DISTINCT FROM exemption: {update_check}"
    );

    let mut conn = {
        let mut c = SqliteConnection::establish(":memory:").expect("connect");
        diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut c).expect("pragma");
        for stmt in &stmts {
            diesel::sql_query(stmt.as_str())
                .execute(&mut c)
                .unwrap_or_else(|e| panic!("DDL: {e}\n{stmt}"));
        }
        c
    };
    diesel::insert_into(notes::table)
        .values((notes::id.eq(1_i32), notes::body.eq("hello")))
        .execute(&mut conn)
        .expect("INSERT must succeed");
    let row = notes_rls::table
        .filter(notes_rls::id.eq(1))
        .select(NoteRow::as_select())
        .first(&mut conn)
        .expect("row must be readable");
    assert_eq!(row.body.as_deref(), Some("hello"), "body must be stored as-is");
}
