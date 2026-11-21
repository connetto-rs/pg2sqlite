//! Replayed writes preserve their physical effects and column bindings.

use diesel::prelude::*;

use super::{open, run, script};

diesel::table! { scratch (id) { id -> Integer, } }
diesel::table! { source (id) { id -> Integer, } }
diesel::table! { inputs (id) { id -> Integer, } }

#[test]
fn a_view_read_does_not_become_a_persistent_write() {
    let script = script(
        "CREATE TABLE source(id INT PRIMARY KEY);
         INSERT INTO source VALUES(1);
         CREATE TEMP TABLE scratch(id INT PRIMARY KEY);
         CREATE TEMP VIEW chosen AS SELECT scratch.id FROM scratch
             WHERE EXISTS(SELECT 1 FROM source);
         CREATE FUNCTION put_row() RETURNS trigger AS $$
             BEGIN INSERT INTO scratch VALUES(NEW.id); RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER put_row INSTEAD OF INSERT ON chosen
             FOR EACH ROW EXECUTE FUNCTION put_row();
         INSERT INTO chosen VALUES(2);",
    );
    let files = tempfile::tempdir().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        assert_eq!(
            scratch::table.select(scratch::id).load::<i32>(&mut creating).expect("initial scratch"),
            [2]
        );
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("temporary-only write"));
    assert_eq!(
        scratch::table.select(scratch::id).load::<i32>(&mut reopened).expect("replayed scratch"),
        [2]
    );
    assert_eq!(
        source::table.select(source::id).load::<i32>(&mut reopened).expect("persistent source"),
        [1]
    );
}

/// A replayed trigger reads its body against the schema it runs on, so an
/// upsert naming columns renamed after the trigger fails on each connection
/// as on PostgreSQL 18.
#[test]
fn an_upsert_trigger_naming_renamed_columns_fails_on_each_connection() {
    let cases = [
        (
            "INSERT INTO ledger(id,value) VALUES(NEW.id,NEW.id)
                 ON CONFLICT(id) DO UPDATE SET value=EXCLUDED.value;",
            "ALTER TABLE ledger RENAME COLUMN value TO amount;",
            r#"column "value" of relation "ledger" does not exist"#,
        ),
        (
            "INSERT INTO ledger(id,value) VALUES(1,NEW.id)
                 ON CONFLICT(id) DO UPDATE SET value=EXCLUDED.value
                 WHERE EXCLUDED.value > ledger.value;",
            "ALTER TABLE ledger RENAME COLUMN id TO key;
             ALTER TABLE ledger RENAME COLUMN value TO amount;",
            r#"column "id" of relation "ledger" does not exist"#,
        ),
    ];
    for (upsert, renames, message) in cases {
        let script = script(&format!(
            "CREATE TABLE ledger(id INT PRIMARY KEY, value INT NOT NULL);
             INSERT INTO ledger VALUES(1,10);
             CREATE TEMP TABLE inputs(id INT);
             CREATE FUNCTION apply_input() RETURNS trigger AS $$
                 BEGIN {upsert} RETURN NEW; END;
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER apply_input AFTER INSERT ON inputs
                 FOR EACH ROW EXECUTE FUNCTION apply_input();
             {renames}"
        ));
        let files = tempfile::tempdir().expect("files");
        {
            let mut creating = open(&files);
            run(&mut creating, script.iter());
            assert_undefined_column(
                diesel::insert_into(inputs::table).values(inputs::id.eq(7)).execute(&mut creating),
                message,
            );
        }
        let mut reopened = open(&files);
        run(&mut reopened, script.connect().expect("connection program"));
        assert_undefined_column(
            diesel::insert_into(inputs::table).values(inputs::id.eq(7)).execute(&mut reopened),
            message,
        );
        let ledger = diesel::dsl::sql::<(diesel::sql_types::Integer, diesel::sql_types::Integer)>(
            "SELECT * FROM ledger",
        );
        assert_eq!(ledger.load::<(i32, i32)>(&mut reopened).expect("persistent ledger"), [(1, 10)]);
    }
}

/// Asserts that `inserted` failed with SQLSTATE `42703` and `message`.
fn assert_undefined_column(inserted: QueryResult<usize>, message: &str) {
    let Err(diesel::result::Error::DatabaseError(_, information)) = inserted else {
        panic!("the insert answered {inserted:?}");
    };
    let refused = pg2sqlite::prelude::postgres_error(information.message())
        .unwrap_or_else(|| panic!("{:?} is no replica refusal", information.message()));
    assert_eq!((refused.sqlstate(), refused.message()), (sqlstate::pg::UNDEFINED_COLUMN, message));
}
