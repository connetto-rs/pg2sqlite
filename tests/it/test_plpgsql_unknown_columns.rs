//! A name a PL/pgSQL trigger body reads as a column stays a column on the
//! replica.
//!
//! PostgreSQL resolves the SQL of a PL/pgSQL body when a run first reaches it,
//! so a body naming a column no relation holds loads and fails each run that
//! reaches that statement with `42703`. SQLite reads a double-quoted name no
//! column in scope holds as a string, so the translated body aborts with
//! PostgreSQL's error where the run reaches the statement instead.

use diesel::{
    Connection, ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection,
    connection::SimpleConnection, result::Error as DieselError,
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, postgres_error};
use sqlstate::pg;

diesel::table! {
    docs (id) {
        id -> Integer,
        owner -> Text,
        body -> Nullable<Text>,
    }
}

diesel::table! {
    audit (id) {
        id -> Integer,
        note -> Nullable<Text>,
    }
}

/// `guard` reads `"nosuch"` under an `IF` for ids 2 to 5, and `assign`
/// assigns it to `NEW.body` for ids above 5.
const SCHEMA: &str = r#"
CREATE TABLE docs (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, body TEXT);
CREATE FUNCTION guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.id BETWEEN 2 AND 5 THEN
    IF EXISTS (SELECT 1 FROM docs WHERE "nosuch" = 'x') THEN
      RAISE EXCEPTION 'blocked';
    END IF;
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER guard AFTER INSERT ON docs FOR EACH ROW EXECUTE FUNCTION guard();
CREATE FUNCTION assign() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.id > 5 THEN
    NEW.body := "nosuch";
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER assign BEFORE INSERT ON docs FOR EACH ROW EXECUTE FUNCTION assign();
"#;

/// The replica of `schema` on SQLite with its default flags.
fn replica(schema: &str) -> SqliteConnection {
    let script = Pg2Sqlite::default()
        .sql(schema)
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap_or_else(|error| panic!("{error}"));
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    connection.batch_execute(&script.into_vec().join(";\n")).unwrap();
    connection
}

fn insert(connection: &mut SqliteConnection, id: i32, owner: &str) -> Result<usize, DieselError> {
    diesel::insert_into(docs::table)
        .values((docs::id.eq(id), docs::owner.eq(owner), docs::body.eq("text")))
        .execute(connection)
}

/// Asserts that `inserted` failed with SQLSTATE `42703` and `message`.
fn assert_undefined_column(inserted: Result<usize, DieselError>, message: &str) {
    let Err(DieselError::DatabaseError(_, information)) = inserted else {
        panic!("the insert answered {inserted:?}");
    };
    let refused = postgres_error(information.message())
        .unwrap_or_else(|| panic!("{:?} is no replica refusal", information.message()));
    assert_eq!((refused.sqlstate(), refused.message()), (pg::UNDEFINED_COLUMN, message));
}

fn ids(connection: &mut SqliteConnection) -> Vec<i32> {
    docs::table.select(docs::id).order(docs::id).load(connection).unwrap()
}

/// Each insert a run of the body reaches `"nosuch"` for fails as on
/// PostgreSQL 18, and an insert no run reaches it for succeeds.
#[test]
fn a_run_reaching_an_unknown_column_fails_with_undefined_column() {
    let mut connection = replica(SCHEMA);
    insert(&mut connection, 1, "alice").unwrap();
    for id in [2, 6] {
        assert_undefined_column(
            insert(&mut connection, id, "alice"),
            r#"column "nosuch" does not exist"#,
        );
    }
    assert_eq!(ids(&mut connection), [1]);
}

/// A later `ELSIF` naming an unknown column fails only where the branches
/// before it do not run, and the statements before it run.
#[test]
fn an_elsif_naming_an_unknown_column_fails_only_where_it_is_tested() {
    let mut connection = replica(
        r#"
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, body TEXT);
        CREATE TABLE audit (id INTEGER);
        CREATE FUNCTION audit() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE
          seen integer := NEW.id;
        BEGIN
          INSERT INTO audit (id) VALUES (seen);
          IF NEW.owner = 'alice' THEN
            INSERT INTO audit (id) VALUES (NEW.id * 10);
          ELSIF "nosuch" IS NULL THEN
            INSERT INTO audit (id) VALUES (-1);
          END IF;
          RETURN NEW;
        END $$;
        CREATE TRIGGER audit AFTER INSERT ON docs FOR EACH ROW EXECUTE FUNCTION audit();
        "#,
    );
    insert(&mut connection, 1, "alice").unwrap();
    assert_undefined_column(insert(&mut connection, 2, "bob"), r#"column "nosuch" does not exist"#);
    assert_eq!(ids(&mut connection), [1]);
    let audited: Vec<i32> = audit::table.select(audit::id).load(&mut connection).unwrap();
    assert_eq!(audited, [1, 10]);
}

/// `TG_NARG` is no variable PL/pgSQL declares, so PostgreSQL reads it as a
/// column and fails each run reaching it, as it fails the unquoted names of
/// any column no relation holds.
#[test]
fn an_unknown_unquoted_name_fails_where_it_is_read() {
    let mut connection = replica(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, body TEXT);
        CREATE TABLE audit (id INTEGER, note TEXT);
        CREATE FUNCTION audit() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
          INSERT INTO audit (id) VALUES (TG_NARG);
          RETURN NEW;
        END $$;
        CREATE TRIGGER audit AFTER INSERT ON docs FOR EACH ROW EXECUTE FUNCTION audit();
        ",
    );
    assert_undefined_column(
        insert(&mut connection, 1, "alice"),
        r#"column "tg_narg" does not exist"#,
    );
    assert_eq!(ids(&mut connection), Vec::<i32>::new());
}

/// Variables, `NEW`, `FOUND`, `TG_OP` and the columns of a body's own
/// relations resolve, so a body naming them runs.
#[test]
fn names_a_body_reaches_resolve() {
    let mut connection = replica(
        r#"
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, body TEXT);
        CREATE TABLE audit (id INTEGER, note TEXT);
        CREATE FUNCTION audit() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE
          "Label" text := 'kept';
        BEGIN
          PERFORM 1 FROM docs d WHERE d."owner" = NEW.owner;
          IF FOUND AND TG_OP = 'INSERT' THEN
            INSERT INTO audit (id, note) VALUES (NEW."id", "Label");
          END IF;
          RETURN NEW;
        END $$;
        CREATE TRIGGER audit AFTER INSERT ON docs FOR EACH ROW EXECUTE FUNCTION audit();
        "#,
    );
    insert(&mut connection, 1, "alice").unwrap();
    let audited: Vec<i32> = audit::table.select(audit::id).load(&mut connection).unwrap();
    assert_eq!(audited, [1]);
}

/// An upsert's `EXCLUDED` row and its target table resolve in a body.
#[test]
fn an_upsert_in_a_body_resolves_its_excluded_row() {
    let mut connection = replica(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, body TEXT);
        CREATE TABLE audit (id INTEGER PRIMARY KEY, note TEXT);
        CREATE FUNCTION audit() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
          INSERT INTO audit (id, note) VALUES (1, NEW.owner)
            ON CONFLICT (id) DO UPDATE SET note = EXCLUDED.note WHERE audit.note <> EXCLUDED.note;
          RETURN NEW;
        END $$;
        CREATE TRIGGER audit AFTER INSERT ON docs FOR EACH ROW EXECUTE FUNCTION audit();
        ",
    );
    insert(&mut connection, 1, "alice").unwrap();
    insert(&mut connection, 2, "bob").unwrap();
    let audited: Vec<(i32, Option<String>)> = audit::table.load(&mut connection).unwrap();
    assert_eq!(audited, [(1, Some("bob".to_owned()))]);
}

/// A field of `NEW` or `OLD` the trigger's table lacks fails a run reaching
/// it, and a statement naming both an unknown column and such a field fails
/// on the one written first, as on PostgreSQL 18.
#[test]
fn a_missing_field_of_a_row_fails_in_written_order() {
    let cases = [
        (
            r#"INSERT INTO audit (note) VALUES ("nosuch" || NEW.zz);"#,
            r#"column "nosuch" does not exist"#,
        ),
        (
            r#"INSERT INTO audit (note) VALUES (NEW.zz || "nosuch");"#,
            r#"record "new" has no field "zz""#,
        ),
        (
            "IF NEW.id > 5 AND OLD.zz > 0 THEN RETURN NULL; END IF;",
            r#"record "old" has no field "zz""#,
        ),
    ];
    for (statement, message) in cases {
        let mut connection = replica(&format!(
            "
            CREATE TABLE docs (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, body TEXT);
            CREATE TABLE audit (id INTEGER, note TEXT);
            CREATE FUNCTION audit() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
              {statement}
              RETURN NEW;
            END $$;
            CREATE TRIGGER audit AFTER INSERT ON docs FOR EACH ROW EXECUTE FUNCTION audit();
            "
        ));
        assert_undefined_column(insert(&mut connection, 1, "alice"), message);
        assert_eq!(ids(&mut connection), Vec::<i32>::new());
    }
}

/// A `BEFORE` trigger assigning a field its table lacks loads and fails each
/// run reaching the assignment, as on PostgreSQL 18, and a run not reaching it
/// writes the other assignments.
#[test]
fn an_assignment_to_a_field_the_table_lacks_fails_where_it_is_reached() {
    let mut connection = replica(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, body TEXT);
        CREATE FUNCTION stamp() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
          NEW.body := 'stamped';
          IF NEW.id > 5 THEN
            NEW.nope := 'x';
          END IF;
          RETURN NEW;
        END $$;
        CREATE TRIGGER stamp BEFORE INSERT ON docs FOR EACH ROW EXECUTE FUNCTION stamp();
        ",
    );
    insert(&mut connection, 1, "alice").unwrap();
    assert_undefined_column(
        insert(&mut connection, 6, "alice"),
        r#"record "new" has no field "nope""#,
    );
    let stored: Vec<Option<String>> = docs::table.select(docs::body).load(&mut connection).unwrap();
    assert_eq!(stored, [Some("stamped".to_owned())]);
}

/// A read naming more than one part past `NEW` takes the part before the last
/// for a relation, so a run reaching it fails as on PostgreSQL 18.
#[test]
fn a_nested_field_of_new_fails_as_a_relation_reference() {
    let cases = [
        ("NEW.zz.x", pg::UNDEFINED_TABLE, r#"missing FROM-clause entry for table "zz""#),
        (
            "NEW.a.b.c",
            pg::FEATURE_NOT_SUPPORTED,
            "cross-database references are not implemented: new.a.b.c",
        ),
    ];
    for (read, sqlstate, message) in cases {
        let mut connection = replica(&format!(
            "
            CREATE TABLE docs (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, body TEXT);
            CREATE FUNCTION nested() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
              NEW.body := {read};
              RETURN NEW;
            END $$;
            CREATE TRIGGER nested BEFORE INSERT ON docs FOR EACH ROW EXECUTE FUNCTION nested();
            "
        ));
        let Err(DieselError::DatabaseError(_, information)) = insert(&mut connection, 1, "alice")
        else {
            panic!("the insert reading {read} succeeded");
        };
        let refused = postgres_error(information.message())
            .unwrap_or_else(|| panic!("{:?} is no replica refusal", information.message()));
        assert_eq!((refused.sqlstate(), refused.message()), (sqlstate, message));
        assert_eq!(ids(&mut connection), Vec::<i32>::new());
    }
}
