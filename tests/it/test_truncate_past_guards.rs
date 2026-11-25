//! A `TRUNCATE` empties its tables as PostgreSQL does, holding the role to
//! the `TRUNCATE` privilege alone and firing no row trigger.
//!
//! PostgreSQL applies no policy and no `DELETE` grant to a `TRUNCATE`, so the
//! guards holding the acting role to those let the deletes through, and only
//! while they run.

use diesel::{
    connection::SimpleConnection, prelude::*, sql_types::BigInt, sqlite::SqliteConnection,
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, guard_refusal};

use crate::helpers;

diesel::define_sql_function! {
    /// Whether a write is the server's delivery, true only while seeding.
    fn delivered() -> diesel::sql_types::Bool;
}

#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = BigInt)]
    count: i64,
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("delivered")
        .with_acting_role("app")
}

fn translated(pg: &str, schema_pg: Option<&str>) -> Vec<String> {
    let parsed = Pg2Sqlite::default().sql(pg).expect("parse");
    let script = match schema_pg {
        Some(schema_pg) => {
            let schema = Pg2Sqlite::default().sql(schema_pg).unwrap().build_schema().unwrap();
            parsed.translate_to_sql_with_schema(&schema, &options())
        }
        None => parsed.translate_to_sql(&options()),
    };
    script.unwrap_or_else(|error| panic!("{error}\n{pg}")).into_vec()
}

fn apply(connection: &mut SqliteConnection, statements: &[String]) -> QueryResult<()> {
    for statement in statements {
        connection.batch_execute(statement)?;
    }
    Ok(())
}

/// `schema_pg` applied to a fresh connection, then `delivered` as the
/// server's delivery.
fn replica(schema_pg: &str, delivered_rows: &str) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("open");
    delivered_utils::register_nondeterministic_impl(&mut connection, || true).expect("register");
    apply(&mut connection, &translated(schema_pg, None)).expect("the schema applies");
    connection.batch_execute(delivered_rows).expect("the delivery applies");
    delivered_utils::register_nondeterministic_impl(&mut connection, || false).expect("register");
    connection
}

/// The acting role's `pg`, translated against `schema_pg`, run on
/// `connection`.
fn run(connection: &mut SqliteConnection, schema_pg: &str, pg: &str) -> QueryResult<()> {
    apply(connection, &translated(pg, Some(schema_pg)))
}

fn count(connection: &mut SqliteConnection, table: &str) -> i64 {
    diesel::sql_query(format!("SELECT count(*) AS count FROM {table}"))
        .get_result::<Count>(connection)
        .expect("count")
        .count
}

const READ_ONLY_KINDS: [(&str, &str, &str); 4] = [
    (
        "row-secured, read-only",
        "CREATE ROLE app; CREATE TABLE t (id INT PRIMARY KEY, secret INT);
         ALTER TABLE t ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, TRUNCATE ON t TO app;
         CREATE POLICY t_select ON t FOR SELECT TO app USING (true);",
        "t_rls",
    ),
    (
        "row-secured, no row deletable",
        "CREATE ROLE app; CREATE TABLE t (id INT PRIMARY KEY, secret INT);
         ALTER TABLE t ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, DELETE, TRUNCATE ON t TO app;
         CREATE POLICY t_select ON t FOR SELECT TO app USING (true);
         CREATE POLICY t_delete ON t FOR DELETE TO app USING (false);",
        "t_rls",
    ),
    (
        "plain, read-only",
        "CREATE ROLE app; CREATE TABLE t (id INT PRIMARY KEY, secret INT);
         GRANT SELECT, TRUNCATE ON t TO app;",
        "t",
    ),
    (
        "one column readable",
        "CREATE ROLE app; CREATE TABLE t (id INT PRIMARY KEY, secret INT);
         GRANT SELECT (id), TRUNCATE ON t TO app;",
        "t_rls",
    ),
];

#[test]
fn a_truncate_empties_a_table_the_role_may_not_delete_from() {
    for (kind, schema_pg, storage) in READ_ONLY_KINDS {
        let mut connection =
            replica(schema_pg, &format!("INSERT INTO {storage} VALUES (1, 1), (2, 2);"));
        run(&mut connection, schema_pg, "TRUNCATE t;")
            .unwrap_or_else(|error| panic!("{kind}: {error}"));
        assert_eq!(count(&mut connection, storage), 0, "{kind}");
    }
}

#[test]
fn the_guards_hold_again_after_a_truncate() {
    for (kind, schema_pg, storage) in READ_ONLY_KINDS {
        let mut connection = replica(schema_pg, &format!("INSERT INTO {storage} VALUES (1, 1);"));
        run(&mut connection, schema_pg, "TRUNCATE t;").expect("the truncate runs");
        let refused = connection
            .batch_execute(&format!("INSERT INTO {storage} VALUES (3, 3);"))
            .expect_err("the guards hold the role to its grants again");
        assert!(guard_refusal(&refused.to_string()).is_some(), "{kind}: {refused}");
        assert_eq!(count(&mut connection, "pg2sqlite_definer_frames"), 0, "{kind}");
    }
}

#[test]
fn a_failed_truncate_leaves_no_frame_and_no_way_past_the_guards() {
    let (_, schema_pg, storage) = READ_ONLY_KINDS[2];
    let mut connection = replica(schema_pg, "INSERT INTO t VALUES (1, 1), (2, 2);");
    connection
        .batch_execute(
            "CREATE TEMP TRIGGER fail_delete BEFORE DELETE ON t
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .expect("inject a failure");

    let failed = run(&mut connection, schema_pg, "TRUNCATE t;").expect_err("the delete fails");
    assert!(failed.to_string().contains("injected failure"), "{failed}");
    assert_eq!(count(&mut connection, storage), 2);
    assert_eq!(count(&mut connection, "pg2sqlite_definer_frames"), 0);
    let refused =
        connection.batch_execute("INSERT INTO t VALUES (3, 3)").expect_err("the guards still hold");
    assert!(guard_refusal(&refused.to_string()).is_some(), "{refused}");

    connection.batch_execute("DROP TRIGGER fail_delete").expect("drop the failure");
    run(&mut connection, schema_pg, "TRUNCATE t;").expect("the next truncate runs");
    assert_eq!(count(&mut connection, storage), 0);
}

#[test]
fn what_a_failed_truncate_leaves_does_not_block_a_later_rename() {
    let schema_pg = "CREATE ROLE app;
        CREATE TABLE t (id INT PRIMARY KEY, secret INT);
        CREATE TABLE u (id INT PRIMARY KEY, gone INT);
        GRANT SELECT, TRUNCATE ON t TO app;
        GRANT ALL ON u TO app;";
    let after_drop = format!("{schema_pg} DROP TABLE t;");
    for migration in [
        "ALTER TABLE u RENAME TO w;",
        "ALTER TABLE u RENAME COLUMN gone TO kept;",
        "ALTER TABLE u DROP COLUMN gone;",
    ] {
        let mut connection = replica(schema_pg, "INSERT INTO t VALUES (1, 1);");
        connection
            .batch_execute(
                "CREATE TEMP TRIGGER fail_delete BEFORE DELETE ON t
                 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
            )
            .expect("inject a failure");
        run(&mut connection, schema_pg, "TRUNCATE t;").expect_err("the delete fails");
        // What the failure left names `t`, and SQLite refuses these over a
        // trigger naming a table that is gone.
        run(&mut connection, schema_pg, "DROP TABLE t;").expect("t is dropped");
        run(&mut connection, &after_drop, migration)
            .unwrap_or_else(|error| panic!("{migration}: {error}"));
    }
}

#[test]
fn a_truncate_fires_no_row_trigger() {
    let schema_pg = "CREATE ROLE app;
        CREATE TABLE t (id INT PRIMARY KEY, parent_id INT REFERENCES t (id) ON DELETE SET NULL);
        CREATE TABLE log (id INT);
        GRANT ALL ON t, log TO app;
        CREATE FUNCTION log_row() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN INSERT INTO log VALUES (OLD.id); RETURN NULL; END $$;
        CREATE TRIGGER t_before_delete BEFORE DELETE ON t
          FOR EACH ROW EXECUTE FUNCTION log_row();
        CREATE TRIGGER t_after_delete AFTER DELETE ON t
          FOR EACH ROW EXECUTE FUNCTION log_row();
        CREATE TRIGGER t_after_update AFTER UPDATE ON t
          FOR EACH ROW EXECUTE FUNCTION log_row();";
    let mut connection = replica(schema_pg, "INSERT INTO t VALUES (1, NULL), (2, 1);");

    run(&mut connection, schema_pg, "TRUNCATE t;").expect("the truncate runs");
    assert_eq!(count(&mut connection, "t"), 0);
    assert_eq!(count(&mut connection, "log"), 0);

    // Outside a TRUNCATE the BEFORE trigger fires and, returning NULL, skips
    // the delete.
    connection.batch_execute("INSERT INTO t VALUES (3, NULL)").expect("insert a row");
    run(&mut connection, schema_pg, "DELETE FROM t;").expect("the delete runs");
    assert_eq!(count(&mut connection, "log"), 1);
    assert_eq!(count(&mut connection, "t"), 1);
}

/// `notes`, whose insert runs `clear_t()` as `cleaner`, truncating `t` when
/// the note's id is positive, `t` read-only for `app` and truncatable by
/// `cleaner` alone.
const DEFINER: &str = "CREATE ROLE app;
    CREATE ROLE cleaner;
    CREATE TABLE notes (id INT PRIMARY KEY);
    CREATE TABLE t (id INT PRIMARY KEY, secret INT);
    CREATE FUNCTION clear_t() RETURNS trigger
    LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
    BEGIN
      IF NEW.id > 0 THEN
        TRUNCATE public.t;
      END IF;
      RETURN NULL;
    END
    $$;
    ALTER FUNCTION clear_t() OWNER TO cleaner;
    CREATE TRIGGER notes_clear AFTER INSERT ON notes
      FOR EACH ROW EXECUTE FUNCTION clear_t();
    GRANT SELECT, INSERT ON notes TO app;
    GRANT SELECT ON t TO app;
    GRANT TRUNCATE ON t TO cleaner;";

#[test]
fn a_definer_body_truncates_past_the_guards_where_it_reaches_the_statement() {
    let mut connection = replica(DEFINER, "INSERT INTO t VALUES (1, 1), (2, 2);");

    run(&mut connection, DEFINER, "INSERT INTO notes VALUES (0);").expect("note 0 goes in");
    assert_eq!(count(&mut connection, "t"), 2);
    run(&mut connection, DEFINER, "INSERT INTO notes VALUES (1);").expect("cleaner truncates t");
    assert_eq!(count(&mut connection, "t"), 0);
    assert_eq!(count(&mut connection, "pg2sqlite_definer_frames"), 0);
}
