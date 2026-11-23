//! A trigger on a table under row-level security, which the replica emits on
//! the backing table behind the table's view. Its body still runs as a
//! trigger on the table it was created on, as in PostgreSQL.

use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::Pg2SqliteOptions;

use crate::helpers;

diesel::table! {
    notes (id) {
        id -> Integer,
    }
}

diesel::table! {
    trigger_log (id) {
        id -> Integer,
        table_name -> Text,
    }
}

/// The replica of a secured `notes` table whose `AFTER INSERT` trigger runs
/// `body`, with `trigger_log` for the body to write to.
fn secured_notes_running(body: &str) -> SqliteConnection {
    let schema = format!(
        "CREATE TABLE notes (id INT PRIMARY KEY);
        ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
        CREATE POLICY notes_read ON notes FOR SELECT USING (true);
        CREATE POLICY notes_write ON notes FOR INSERT WITH CHECK (true);
        CREATE TABLE trigger_log (id INT PRIMARY KEY, table_name TEXT NOT NULL);
        CREATE FUNCTION notes_trigger() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN
            {body}
            RETURN NEW;
        END;
        $$;
        CREATE TRIGGER notes_trigger AFTER INSERT ON notes
            FOR EACH ROW EXECUTE FUNCTION notes_trigger();"
    );
    helpers::apply_translated_ddl(
        &schema,
        &Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"),
    )
}

fn insert_note(connection: &mut SqliteConnection, id: i32) -> diesel::QueryResult<usize> {
    diesel::insert_into(notes::table).values(notes::id.eq(id)).execute(connection)
}

#[test]
fn tg_table_name_is_the_table_the_trigger_was_created_on() {
    let mut connection = secured_notes_running(
        "INSERT INTO trigger_log (id, table_name) VALUES (NEW.id, TG_TABLE_NAME);",
    );
    insert_note(&mut connection, 1).expect("insert through the view");
    let logged: Vec<(i32, String)> = trigger_log::table
        .select((trigger_log::id, trigger_log::table_name))
        .load(&mut connection)
        .expect("read the log");
    assert_eq!(logged, [(1, "notes".to_owned())]);
}

/// PostgreSQL raises for a field `NEW` lacks only where the body reaches it.
#[test]
fn a_field_the_table_lacks_raises_only_where_the_body_reaches_it() {
    let mut connection = secured_notes_running(
        "IF NEW.id > 5 THEN
            INSERT INTO trigger_log (id, table_name) VALUES (NEW.id, NEW.missing);
        END IF;",
    );
    insert_note(&mut connection, 1).expect("the branch reading the field is not reached");
    let error = insert_note(&mut connection, 9).expect_err("the branch reads the field");
    assert_eq!(error.to_string(), "42703: record \"new\" has no field \"missing\"");
}
