//! The triggers a foreign key action fires.
//!
//! PostgreSQL runs a key's `CASCADE`, `SET NULL` and `SET DEFAULT` as the
//! referencing table's owner, and every trigger the action fires, and every
//! trigger those fire in turn, runs as that owner. A table whose owner the
//! schema never declares belongs to a role the replica holds past every
//! guard.

use diesel::{
    connection::SimpleConnection, prelude::*, sql_types::BigInt, sqlite::SqliteConnection,
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, postgres_error};
use sql_traits::structs::ParserDB;

use crate::helpers;

diesel::define_sql_function! {
    /// Whether the connection writes past the generated guards.
    fn key_owner_exempt() -> diesel::sql_types::Bool;
}

#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = BigInt)]
    count: i64,
}

/// `app` reads `log` and writes `parents`, `keeper` may also write `log`,
/// and `log_child` logs the row a delete removes.
const COMMON: &str = "CREATE ROLE app;
    CREATE ROLE keeper;
    CREATE TABLE parents (id INT PRIMARY KEY);
    GRANT SELECT, UPDATE, DELETE ON parents TO app;
    CREATE TABLE log (id INT);
    GRANT SELECT ON log TO app;
    GRANT SELECT, INSERT ON log TO keeper;
    CREATE FUNCTION log_child() RETURNS trigger LANGUAGE plpgsql AS $$
    BEGIN
      INSERT INTO log VALUES (OLD.id);
      RETURN OLD;
    END $$;";

/// `children`, whose rows a cascade deletes, logged by `log_child`.
const CHILDREN: &str = "CREATE TABLE children (id INT PRIMARY KEY,
        parent_id INT REFERENCES parents (id) ON DELETE CASCADE);
    CREATE TRIGGER children_log AFTER DELETE ON children
        FOR EACH ROW EXECUTE FUNCTION log_child();";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("key_owner_exempt")
        .with_acting_role("app")
}

fn apply(connection: &mut SqliteConnection, statements: Vec<String>) {
    for statement in statements {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
}

/// `pg` translated and applied to a fresh connection, holding `delivered` as
/// the server delivers it, with the schema it leaves.
fn replica(pg: &str, delivered: &str) -> (SqliteConnection, ParserDB) {
    let script = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&options())
        .unwrap_or_else(|error| panic!("{error}\n{pg}"));
    let mut connection = SqliteConnection::establish(":memory:").expect("open an in-memory SQLite");
    script.register_functions(&mut connection).expect("register the functions");
    key_owner_exempt_utils::register_nondeterministic_impl(&mut connection, || true)
        .expect("exempt the delivery");
    apply(&mut connection, script.into_vec());
    connection.batch_execute(delivered).unwrap_or_else(|error| panic!("{error}\n{delivered}"));
    key_owner_exempt_utils::register_nondeterministic_impl(&mut connection, || false)
        .expect("end the exemption");
    let schema = Pg2Sqlite::default().sql(pg).expect("parse").build_schema().expect("schema");
    (connection, schema)
}

/// Applies `migration`, translated against `schema`, the schema the replica
/// holds.
fn migrate(connection: &mut SqliteConnection, schema: &ParserDB, migration: &str) {
    let statements = Pg2Sqlite::default()
        .sql(migration)
        .expect("parse")
        .translate_to_sql_with_schema(schema, &options())
        .unwrap_or_else(|error| panic!("{error}\n{migration}"))
        .into_vec();
    apply(connection, statements);
}

fn count(connection: &mut SqliteConnection, table: &str) -> i64 {
    diesel::sql_query(format!("SELECT count(*) AS count FROM {table}"))
        .get_result::<Count>(connection)
        .unwrap_or_else(|error| panic!("count {table}: {error}"))
        .count
}

/// Asserts the replica refuses `sql` as PostgreSQL refuses a write to
/// `table` by a role without the privilege.
#[track_caller]
fn assert_denied(connection: &mut SqliteConnection, sql: &str, table: &str) {
    let error = connection.batch_execute(sql).expect_err("the write is refused").to_string();
    let refused = postgres_error(&error).unwrap_or_else(|| panic!("no PostgreSQL error: {error}"));
    assert_eq!(refused.sqlstate().to_string(), "42501", "{error}");
    assert!(
        refused
            .message()
            .split(|character: char| !character.is_alphanumeric())
            .any(|word| word == table),
        "{error}"
    );
}

/// The replica of `children` in `situation`, holding parents 1 and 2 and a
/// child of each.
fn children_replica(situation: &str) -> (SqliteConnection, ParserDB) {
    replica(
        &format!("{COMMON} {CHILDREN} {situation}"),
        "INSERT INTO parents VALUES (1), (2); INSERT INTO children VALUES (1, 1), (2, 2);",
    )
}

/// Each way `app` may hold `children` that leaves the cascade's trigger
/// running as a role that may write `log`, with whether `app` may delete
/// from `children` itself.
const OWNED_ELSEWHERE: [(&str, &str, bool); 5] = [
    (
        "read-only, owned by keeper",
        "ALTER TABLE children OWNER TO keeper; GRANT SELECT ON children TO app;",
        false,
    ),
    ("read-only, no declared owner", "GRANT SELECT ON children TO app;", false),
    (
        "read-only under row security, owned by keeper",
        "ALTER TABLE children OWNER TO keeper;
         ALTER TABLE children ENABLE ROW LEVEL SECURITY;
         GRANT SELECT ON children TO app;
         CREATE POLICY children_select ON children FOR SELECT TO app USING (true);",
        false,
    ),
    (
        "writable, owned by keeper",
        "ALTER TABLE children OWNER TO keeper; GRANT ALL ON children TO app;",
        true,
    ),
    ("writable, no declared owner", "GRANT ALL ON children TO app;", true),
];

#[test]
fn a_cascade_runs_the_triggers_it_fires_as_the_children_owner() {
    for (name, situation, writable) in OWNED_ELSEWHERE {
        let (mut connection, _) = children_replica(situation);
        connection
            .batch_execute("DELETE FROM parents WHERE id = 1")
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(count(&mut connection, "children"), 1, "{name}");
        assert_eq!(count(&mut connection, "log"), 1, "{name}");
        if writable {
            assert_denied(&mut connection, "DELETE FROM children WHERE id = 2", "log");
        }
        assert_eq!(count(&mut connection, "log"), 1, "{name}");
    }
}

#[test]
fn a_cascade_into_children_the_role_owns_runs_their_triggers_as_the_role() {
    let (mut connection, _) = children_replica("ALTER TABLE children OWNER TO app;");
    assert_denied(&mut connection, "DELETE FROM parents WHERE id = 1", "log");
    assert_eq!(count(&mut connection, "children"), 2);
}

#[test]
fn an_owner_lacking_the_privilege_is_refused() {
    let (mut connection, _) = children_replica(
        "CREATE ROLE outsider;
         ALTER TABLE children OWNER TO outsider;
         GRANT ALL ON children TO app;",
    );
    assert_denied(&mut connection, "DELETE FROM parents WHERE id = 1", "log");
    assert_eq!(count(&mut connection, "children"), 2);
    assert_eq!(count(&mut connection, "log"), 0);
}

#[test]
fn a_set_null_runs_the_update_triggers_it_fires_as_the_children_owner() {
    let (mut connection, _) = replica(
        &format!(
            "{COMMON}
            CREATE TABLE children (id INT PRIMARY KEY,
                parent_id INT REFERENCES parents (id) ON DELETE SET NULL);
            ALTER TABLE children OWNER TO keeper;
            GRANT ALL ON children TO app;
            CREATE FUNCTION log_update() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
              INSERT INTO log VALUES (NEW.id);
              RETURN NEW;
            END $$;
            CREATE TRIGGER children_log AFTER UPDATE ON children
                FOR EACH ROW EXECUTE FUNCTION log_update();"
        ),
        "INSERT INTO parents VALUES (1), (2); INSERT INTO children VALUES (1, 1), (2, 2);",
    );
    connection.batch_execute("DELETE FROM parents WHERE id = 1").expect("the set null runs");
    assert_eq!(count(&mut connection, "log"), 1);
    assert_denied(&mut connection, "UPDATE children SET id = 3 WHERE id = 2", "log");
}

#[test]
fn the_triggers_a_cascaded_trigger_fires_run_as_the_children_owner() {
    for (name, owner) in
        [("owned by keeper", "ALTER TABLE children OWNER TO keeper;"), ("no declared owner", "")]
    {
        let (mut connection, _) = replica(
            &format!(
                "{COMMON}
                CREATE TABLE hops (id INT);
                GRANT SELECT ON hops TO app;
                GRANT SELECT, INSERT ON hops TO keeper;
                CREATE FUNCTION hop_child() RETURNS trigger LANGUAGE plpgsql AS $$
                BEGIN
                  INSERT INTO hops VALUES (OLD.id + 100);
                  RETURN OLD;
                END $$;
                CREATE FUNCTION log_hop() RETURNS trigger LANGUAGE plpgsql AS $$
                BEGIN
                  INSERT INTO log VALUES (NEW.id);
                  RETURN NEW;
                END $$;
                CREATE TABLE children (id INT PRIMARY KEY,
                    parent_id INT REFERENCES parents (id) ON DELETE CASCADE);
                {owner}
                GRANT ALL ON children TO app;
                CREATE TRIGGER children_hop AFTER DELETE ON children
                    FOR EACH ROW EXECUTE FUNCTION hop_child();
                CREATE TRIGGER hops_log AFTER INSERT ON hops
                    FOR EACH ROW EXECUTE FUNCTION log_hop();"
            ),
            "INSERT INTO parents VALUES (1), (2); INSERT INTO children VALUES (1, 1), (2, 2);",
        );
        connection
            .batch_execute("DELETE FROM parents WHERE id = 1")
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(count(&mut connection, "hops"), 1, "{name}");
        assert_eq!(count(&mut connection, "log"), 1, "{name}");
        assert_denied(&mut connection, "DELETE FROM children WHERE id = 2", "hops");
    }
}

#[test]
fn a_trigger_created_later_runs_as_the_children_owner() {
    let (mut connection, schema) = replica(
        &format!(
            "{COMMON}
            CREATE TABLE children (id INT PRIMARY KEY,
                parent_id INT REFERENCES parents (id) ON DELETE CASCADE);
            ALTER TABLE children OWNER TO keeper;
            GRANT ALL ON children TO app;"
        ),
        "INSERT INTO parents VALUES (1), (2), (3);
         INSERT INTO children VALUES (1, 1), (2, 2), (3, 3);",
    );
    migrate(
        &mut connection,
        &schema,
        "CREATE TRIGGER children_log AFTER DELETE ON children
             FOR EACH ROW EXECUTE FUNCTION log_child();",
    );
    connection.batch_execute("DELETE FROM parents WHERE id = 1").expect("the cascade runs");
    assert_eq!(count(&mut connection, "children"), 2);
    assert_eq!(count(&mut connection, "log"), 1);

    let schema = Pg2Sqlite::default()
        .sql(&format!(
            "{COMMON}
            CREATE TABLE children (id INT PRIMARY KEY,
                parent_id INT REFERENCES parents (id) ON DELETE CASCADE);
            ALTER TABLE children OWNER TO keeper;
            GRANT ALL ON children TO app;
            CREATE TRIGGER children_log AFTER DELETE ON children
                FOR EACH ROW EXECUTE FUNCTION log_child();"
        ))
        .expect("parse")
        .build_schema()
        .expect("schema");
    migrate(&mut connection, &schema, "DROP TRIGGER children_log ON children;");
    connection.batch_execute("DELETE FROM parents WHERE id = 2").expect("the cascade runs");
    assert_eq!(count(&mut connection, "children"), 1);
    assert_eq!(count(&mut connection, "log"), 1);
}

#[test]
fn a_new_owner_moves_the_triggers_a_cascade_fires() {
    let (mut connection, schema) =
        children_replica("ALTER TABLE children OWNER TO keeper; GRANT ALL ON children TO app;");
    migrate(&mut connection, &schema, "ALTER TABLE children OWNER TO app;");
    assert_denied(&mut connection, "DELETE FROM parents WHERE id = 1", "log");
    assert_eq!(count(&mut connection, "children"), 2);
}
