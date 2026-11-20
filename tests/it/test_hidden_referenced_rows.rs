//! Foreign keys into a table whose rows a `SELECT` policy may hide from the
//! acting role.
//!
//! PostgreSQL checks a key without row security, so the replica, holding only
//! the rows the role reads, leaves the referencing check to the server and
//! keeps what deleting or rekeying a referenced row does to the referencing
//! rows it holds.

use diesel::{connection::SimpleConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, postgres_error},
    warnings::TranslationWarning,
};
use sqlstate::pg;

use crate::{helpers, support::transaction::transaction};

diesel::table! {
    /// The referencing table.
    memberships (id) {
        /// Primary key.
        id -> Integer,
        /// The key into `users`.
        user_id -> Nullable<Integer>,
    }
}

diesel::table! {
    /// The backing table of `nodes`, holding every row the server delivered.
    nodes_rls (id) {
        /// Primary key.
        id -> Integer,
    }
}

/// `memberships` after its key column is renamed.
mod renamed {
    diesel::table! {
        /// The referencing table.
        memberships (id) {
            /// Primary key.
            id -> Integer,
            /// The key into `users`, renamed from `user_id`.
            member_id -> Nullable<Integer>,
        }
    }
}

diesel::define_sql_function! {
    /// Whether the connection writes past the generated guards.
    fn key_write_exempt() -> diesel::sql_types::Bool;
}

const REFUSAL: &str = "update or delete on table \"users\" violates foreign key constraint \
                       \"memberships_user_id_fkey\" on table \"memberships\"";

/// `users`, whose rows the acting role reads only when `policy` holds, and
/// `memberships`, whose `user_id` column declares `key`, followed by `extra`.
fn script_with(policy: &str, key: &str, extra: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL);
         ALTER TABLE users ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, UPDATE, DELETE ON users TO app;
         CREATE POLICY users_select ON users FOR SELECT TO app USING ({policy});
         CREATE POLICY users_update ON users FOR UPDATE TO app USING (true);
         CREATE POLICY users_delete ON users FOR DELETE TO app USING (true);
         CREATE TABLE memberships (id INT PRIMARY KEY, user_id INT DEFAULT 1 {key});
         GRANT SELECT, INSERT, UPDATE, DELETE ON memberships TO app;
         {extra}"
    )
}

fn script(key: &str) -> String {
    script_with("name = 'visible'", key, "")
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_rls_audit_table_name("rls_audit")
        .with_write_exemption_function("key_write_exempt")
        .with_acting_role("app")
}

/// Runs `sql` as the server applies its rows, exempt from the generated
/// guards.
fn deliver(connection: &mut SqliteConnection, sql: &str) {
    key_write_exempt_utils::register_nondeterministic_impl(connection, || true)
        .expect("exempt the delivery");
    connection.batch_execute(sql).unwrap_or_else(|error| panic!("{error}\n{sql}"));
    key_write_exempt_utils::register_nondeterministic_impl(connection, || false)
        .expect("end the exemption");
}

/// `pg` translated with `options` and applied to a fresh connection holding
/// the visible user 1, with the statements a `COMMIT` has to run first.
fn replica_with(pg: &str, options: &Pg2SqliteOptions) -> (SqliteConnection, Vec<String>) {
    let script =
        Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(options).expect("translate");
    let before_commit = script.before_commit.clone();
    let mut connection = SqliteConnection::establish(":memory:").expect("open an in-memory SQLite");
    key_write_exempt_utils::register_nondeterministic_impl(&mut connection, || false)
        .expect("register the exemption");
    for statement in script.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    deliver(&mut connection, "INSERT INTO users_rls (id, name) VALUES (1, 'visible')");
    (connection, before_commit)
}

fn replica(key: &str) -> SqliteConnection {
    replica_with(&script(key), &options()).0
}

/// The message the replica refuses `sql` with as PostgreSQL's foreign key
/// violation.
fn refusal(connection: &mut SqliteConnection, sql: &str) -> String {
    let error = connection.batch_execute(sql).expect_err("the write is refused").to_string();
    let refused = postgres_error(&error).unwrap_or_else(|| panic!("no PostgreSQL error: {error}"));
    assert_eq!(refused.sqlstate(), pg::FOREIGN_KEY_VIOLATION, "{error}");
    refused.message().to_owned()
}

fn memberships(connection: &mut SqliteConnection) -> Vec<(i32, Option<i32>)> {
    memberships::table
        .select((memberships::id, memberships::user_id))
        .order(memberships::id)
        .load(connection)
        .expect("read memberships")
}

#[test]
fn a_write_referencing_a_row_the_role_cannot_read_commits() {
    let mut connection = replica("REFERENCES users (id)");
    diesel::insert_into(memberships::table)
        .values((memberships::id.eq(1), memberships::user_id.eq(2)))
        .execute(&mut connection)
        .expect("user 2 exists on the server");
    assert_eq!(memberships(&mut connection), [(1, Some(2))]);
}

#[test]
fn deleting_a_referenced_row_is_refused_while_a_referencing_row_remains() {
    for key in
        ["REFERENCES users (id)", "REFERENCES users", "REFERENCES users (id) ON DELETE RESTRICT"]
    {
        let mut connection = replica(key);
        connection.batch_execute("INSERT INTO memberships VALUES (1, 1)").expect("membership");
        assert_eq!(refusal(&mut connection, "DELETE FROM users WHERE id = 1"), REFUSAL, "{key}");
        connection.batch_execute("DELETE FROM memberships").expect("drop the membership");
        connection.batch_execute("DELETE FROM users WHERE id = 1").expect("nothing references it");
    }
}

#[test]
fn rekeying_a_referenced_row_is_refused_while_a_referencing_row_remains() {
    let mut connection = replica("REFERENCES users (id)");
    connection.batch_execute("INSERT INTO memberships VALUES (1, 1)").expect("membership");
    assert_eq!(refusal(&mut connection, "UPDATE users SET id = 3 WHERE id = 1"), REFUSAL);
    connection
        .batch_execute("UPDATE users SET name = 'visible' WHERE id = 1")
        .expect("an update keeping the key checks nothing");
}

#[test]
fn cascade_deletes_and_rekeys_the_referencing_rows() {
    let mut connection = replica("REFERENCES users (id) ON DELETE CASCADE ON UPDATE CASCADE");
    connection.batch_execute("INSERT INTO memberships VALUES (1, 1), (2, 2)").expect("memberships");
    connection.batch_execute("UPDATE users SET id = 3 WHERE id = 1").expect("rekey");
    assert_eq!(memberships(&mut connection), [(1, Some(3)), (2, Some(2))]);
    connection.batch_execute("DELETE FROM users WHERE id = 3").expect("delete");
    assert_eq!(memberships(&mut connection), [(2, Some(2))]);
}

#[test]
fn set_null_and_set_default_rewrite_the_referencing_rows() {
    let mut connection = replica("REFERENCES users (id) ON DELETE SET NULL ON UPDATE SET DEFAULT");
    deliver(&mut connection, "INSERT INTO users_rls (id, name) VALUES (4, 'visible')");
    connection.batch_execute("INSERT INTO memberships VALUES (1, 4), (2, 1)").expect("memberships");
    connection.batch_execute("DELETE FROM users WHERE id = 1").expect("delete");
    connection.batch_execute("UPDATE users SET id = 5 WHERE id = 4").expect("rekey");
    assert_eq!(memberships(&mut connection), [(1, Some(1)), (2, None)]);
}

#[test]
fn set_default_without_a_default_clears_the_referencing_rows() {
    let pg = script_with("name = 'visible'", "REFERENCES users (id) ON DELETE SET DEFAULT", "")
        .replace("DEFAULT 1 ", "");
    let (mut connection, _) = replica_with(&pg, &options());
    connection.batch_execute("INSERT INTO memberships VALUES (1, 1)").expect("membership");
    connection.batch_execute("DELETE FROM users WHERE id = 1").expect("delete");
    assert_eq!(memberships(&mut connection), [(1, None)]);
}

#[test]
fn a_deferred_key_checks_the_referencing_rows_at_commit() {
    let (mut connection, before_commit) =
        replica_with(&script("REFERENCES users (id) DEFERRABLE INITIALLY DEFERRED"), &options());
    connection.batch_execute("INSERT INTO memberships VALUES (1, 1)").expect("membership");
    let error = transaction(&mut connection, &before_commit, "DELETE FROM users WHERE id = 1")
        .expect_err("the membership still references the user at commit");
    assert!(error.to_string().contains(REFUSAL), "{error}");
    transaction(
        &mut connection,
        &before_commit,
        "DELETE FROM users WHERE id = 1; DELETE FROM memberships;",
    )
    .expect("nothing references the user by the commit");
    assert_eq!(memberships(&mut connection), Vec::new());
}

#[test]
fn an_exempt_write_fires_no_check() {
    let mut connection = replica("REFERENCES users (id)");
    connection.batch_execute("INSERT INTO memberships VALUES (1, 1)").expect("membership");
    deliver(&mut connection, "DELETE FROM users_rls WHERE id = 1");
}

#[test]
fn a_replica_omitting_server_derived_writes_keeps_only_the_refusals() {
    let options = options().with_server_derived_writes_omitted();
    let (mut connection, _) = replica_with(
        &script("REFERENCES users (id) ON DELETE CASCADE ON UPDATE RESTRICT"),
        &options,
    );
    connection.batch_execute("INSERT INTO memberships VALUES (1, 1)").expect("membership");
    assert_eq!(refusal(&mut connection, "UPDATE users SET id = 3 WHERE id = 1"), REFUSAL);
    connection.batch_execute("DELETE FROM users WHERE id = 1").expect("the server cascades");
    assert_eq!(memberships(&mut connection), [(1, Some(1))]);
}

#[test]
fn a_key_added_later_keeps_its_referenced_side() {
    let pg = script_with(
        "name = 'visible'",
        "",
        "ALTER TABLE memberships ADD CONSTRAINT member FOREIGN KEY (user_id) REFERENCES users (id);",
    );
    let (mut connection, _) = replica_with(&pg, &options());
    connection.batch_execute("INSERT INTO memberships VALUES (1, 1), (2, 2)").expect("memberships");
    assert_eq!(
        refusal(&mut connection, "DELETE FROM users WHERE id = 1"),
        "update or delete on table \"users\" violates foreign key constraint \"member\" on table \
         \"memberships\""
    );
}

#[test]
fn dropping_the_referencing_table_or_column_drops_the_referenced_side() {
    for key in ["REFERENCES users (id)", "REFERENCES users (id) DEFERRABLE INITIALLY DEFERRED"] {
        for extra in [
            "DROP TABLE memberships;",
            "ALTER TABLE memberships DROP COLUMN user_id;",
            "ALTER TABLE memberships RENAME TO members; DROP TABLE members;",
        ] {
            let pg = script_with("name = 'visible'", key, extra);
            let (mut connection, before_commit) = replica_with(&pg, &options());
            transaction(&mut connection, &before_commit, "DELETE FROM users WHERE id = 1")
                .unwrap_or_else(|error| panic!("{key} {extra}: {error}"));
        }
    }
}

#[test]
fn a_key_into_rows_the_role_reads_in_full_stays_a_sqlite_key() {
    let pg = script_with("true", "REFERENCES users (id)", "");
    let (mut connection, _) = replica_with(&pg, &options());
    let error = connection
        .batch_execute("INSERT INTO memberships VALUES (1, 2)")
        .expect_err("user 2 is not on the replica");
    assert!(error.to_string().contains("FOREIGN KEY constraint failed"), "{error}");
}

#[test]
fn the_report_names_the_key_and_the_hiding_policy() {
    let report = Pg2Sqlite::default()
        .sql(&script("REFERENCES users (id)"))
        .expect("parse")
        .translate_with_report(&options())
        .expect("translate");
    let hidden: Vec<&TranslationWarning> = report
        .warnings
        .iter()
        .filter(|warning| matches!(warning, TranslationWarning::HiddenReferencedRows { .. }))
        .collect();
    assert_eq!(
        hidden,
        [&TranslationWarning::HiddenReferencedRows {
            name: "memberships_user_id_fkey".to_owned(),
            table: "memberships".to_owned(),
            referenced: "users".to_owned(),
            policies: vec!["users_select".to_owned()],
        }]
    );
    assert_eq!(
        hidden[0].to_string(),
        "the replica leaves the check of foreign key memberships_user_id_fkey on memberships to \
         the server, since users_select may hide rows of users from the acting role"
    );
}

#[test]
fn the_report_names_no_policy_when_none_lets_the_role_read() {
    let pg = script_with("true", "REFERENCES users (id)", "")
        .replace("CREATE POLICY users_select ON users FOR SELECT TO app USING (true);", "");
    let report = Pg2Sqlite::default()
        .sql(&pg)
        .expect("parse")
        .translate_with_report(&options())
        .expect("translate");
    let hidden: Vec<String> = report
        .warnings
        .iter()
        .filter(|warning| matches!(warning, TranslationWarning::HiddenReferencedRows { .. }))
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        hidden,
        ["the replica leaves the check of foreign key memberships_user_id_fkey on memberships to \
          the server, since no policy lets the acting role read users"]
    );
}

#[test]
fn renaming_the_referencing_table_moves_the_referenced_side() {
    let pg = script_with(
        "name = 'visible'",
        "REFERENCES users (id)",
        "CREATE TABLE teams (id INT PRIMARY KEY);
         GRANT SELECT ON teams TO app;
         ALTER TABLE memberships ADD COLUMN team_id INT REFERENCES teams (id);
         ALTER TABLE memberships RENAME TO members;",
    );
    let (mut connection, _) = replica_with(&pg, &options());
    connection
        .batch_execute("INSERT INTO members (id, user_id) VALUES (1, 1)")
        .expect("membership");
    assert_eq!(
        refusal(&mut connection, "DELETE FROM users WHERE id = 1"),
        "update or delete on table \"users\" violates foreign key constraint \
         \"memberships_user_id_fkey\" on table \"members\""
    );
}

#[test]
fn renaming_a_key_column_keeps_the_referenced_side() {
    let pg = script_with(
        "name = 'visible'",
        "REFERENCES users (id) ON DELETE SET DEFAULT",
        "ALTER TABLE memberships RENAME COLUMN user_id TO member_id;",
    );
    let (mut connection, _) = replica_with(&pg, &options());
    deliver(&mut connection, "INSERT INTO users_rls (id, name) VALUES (4, 'visible')");
    connection.batch_execute("INSERT INTO memberships VALUES (1, 4)").expect("membership");
    connection.batch_execute("DELETE FROM users WHERE id = 4").expect("delete");
    let members: Vec<Option<i32>> = renamed::memberships::table
        .select(renamed::memberships::member_id)
        .load(&mut connection)
        .expect("read memberships");
    assert_eq!(members, [Some(1)]);

    for drop in ["DROP TABLE memberships;", "ALTER TABLE memberships DROP COLUMN member_id;"] {
        let pg = script_with(
            "name = 'visible'",
            "REFERENCES users (id)",
            &format!("ALTER TABLE memberships RENAME COLUMN user_id TO member_id; {drop}"),
        );
        let (mut connection, _) = replica_with(&pg, &options());
        connection.batch_execute("DELETE FROM users WHERE id = 1").expect(drop);
    }
}

#[test]
fn two_unnamed_keys_on_one_column_keep_their_own_names() {
    let pg = script_with(
        "name = 'visible'",
        "REFERENCES users (id)",
        "CREATE TABLE admins (id INT PRIMARY KEY, name TEXT NOT NULL);
         ALTER TABLE admins ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, DELETE ON admins TO app;
         CREATE POLICY admins_select ON admins FOR SELECT TO app USING (name = 'visible');
         CREATE POLICY admins_delete ON admins FOR DELETE TO app USING (true);
         ALTER TABLE memberships ADD FOREIGN KEY (user_id) REFERENCES admins (id);",
    );
    let (mut connection, _) = replica_with(&pg, &options());
    deliver(&mut connection, "INSERT INTO admins_rls (id, name) VALUES (1, 'visible')");
    connection.batch_execute("INSERT INTO memberships VALUES (1, 1)").expect("membership");
    assert_eq!(refusal(&mut connection, "DELETE FROM users WHERE id = 1"), REFUSAL);
    assert_eq!(
        refusal(&mut connection, "DELETE FROM admins WHERE id = 1"),
        "update or delete on table \"admins\" violates foreign key constraint \
         \"memberships_user_id_fkey1\" on table \"memberships\""
    );
}

#[test]
fn a_key_into_its_own_table_cascades_through_every_descendant() {
    let pg = "CREATE ROLE app;
         CREATE TABLE nodes (id INT PRIMARY KEY, parent INT REFERENCES nodes (id) ON DELETE CASCADE,
             name TEXT NOT NULL);
         CREATE TABLE leaves (id INT PRIMARY KEY, node INT REFERENCES nodes (id) ON DELETE CASCADE,
             first_node INT REFERENCES nodes (id) ON DELETE CASCADE);
         CREATE TABLE tags (id INT PRIMARY KEY, leaf INT REFERENCES leaves (id));
         ALTER TABLE nodes ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, DELETE ON nodes TO app;
         GRANT SELECT ON leaves, tags TO app;
         CREATE POLICY nodes_select ON nodes FOR SELECT TO app USING (name = 'visible');
         CREATE POLICY nodes_delete ON nodes FOR DELETE TO app USING (true);";
    let script = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&options())
        .expect("translate");
    let mut connection = SqliteConnection::establish(":memory:").expect("open an in-memory SQLite");
    key_write_exempt_utils::register_nondeterministic_impl(&mut connection, || false)
        .expect("register the exemption");
    for statement in script.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    deliver(
        &mut connection,
        "INSERT INTO nodes_rls (id, parent, name) VALUES
             (1, NULL, 'visible'), (2, 1, 'hidden'), (3, 2, 'hidden'), (4, NULL, 'visible')",
    );
    connection.batch_execute("DELETE FROM nodes WHERE id = 1").expect("delete the root");
    let left: Vec<i32> = nodes_rls::table
        .select(nodes_rls::id)
        .order(nodes_rls::id)
        .load(&mut connection)
        .expect("read");
    assert_eq!(left, [4]);
}

#[test]
fn a_cycle_of_actions_through_hidden_rows_is_refused() {
    let deletes = "CREATE TABLE a (id INT PRIMARY KEY, name TEXT NOT NULL, b_id INT);
         CREATE TABLE b (id INT PRIMARY KEY, name TEXT NOT NULL,
             a_id INT REFERENCES a (id) ON DELETE CASCADE);
         ALTER TABLE a ADD FOREIGN KEY (b_id) REFERENCES b (id) ON DELETE CASCADE;
         CREATE TABLE secret (id INT PRIMARY KEY);
         ALTER TABLE b ADD COLUMN secret_id INT REFERENCES secret (id);";
    let updates = "CREATE TABLE a (id INT PRIMARY KEY, name TEXT NOT NULL);
         CREATE TABLE b (id INT PRIMARY KEY, name TEXT NOT NULL,
             a_id INT UNIQUE REFERENCES a (id) ON UPDATE CASCADE);
         ALTER TABLE a ADD FOREIGN KEY (id) REFERENCES b (a_id) ON UPDATE CASCADE;";
    for (tables, action) in [(deletes, "ON DELETE"), (updates, "ON UPDATE")] {
        let pg = format!(
            "CREATE ROLE app;
             {tables}
             ALTER TABLE a ENABLE ROW LEVEL SECURITY;
             ALTER TABLE b ENABLE ROW LEVEL SECURITY;
             GRANT SELECT ON a, b TO app;
             CREATE POLICY a_select ON a FOR SELECT TO app USING (name = 'visible');
             CREATE POLICY b_select ON b FOR SELECT TO app USING (name = 'visible');"
        );
        let refusal = Pg2Sqlite::default()
            .sql(&pg)
            .expect("parse")
            .translate_to_sql(&options())
            .expect_err("the cycle is refused")
            .to_string();
        assert!(refusal.contains(&format!("its {action} action leads back")), "{refusal}");
    }
}

#[test]
fn rebuilding_the_referenced_table_keeps_the_referenced_side() {
    let pg = "CREATE ROLE app;
         CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL, email TEXT UNIQUE);
         ALTER TABLE users ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, DELETE ON users TO app;
         CREATE POLICY users_select ON users FOR SELECT TO app USING (name = 'visible');
         CREATE POLICY users_delete ON users FOR DELETE TO app USING (true);
         CREATE TABLE memberships (id INT PRIMARY KEY, user_id INT REFERENCES users (id));
         GRANT SELECT, INSERT ON memberships TO app;
         ALTER TABLE users DROP COLUMN email;";
    let (mut connection, _) = replica_with(pg, &options());
    connection.batch_execute("INSERT INTO memberships VALUES (1, 1)").expect("membership");
    assert_eq!(refusal(&mut connection, "DELETE FROM users WHERE id = 1"), REFUSAL);
}

#[test]
fn a_deferred_key_of_the_starting_schema_survives_a_rebuild_of_the_referenced_table() {
    let pg = "CREATE ROLE app;
         CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL, email TEXT UNIQUE);
         ALTER TABLE users ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, DELETE ON users TO app;
         CREATE POLICY users_select ON users FOR SELECT TO app USING (name = 'visible');
         CREATE POLICY users_delete ON users FOR DELETE TO app USING (true);
         CREATE TABLE memberships (id INT PRIMARY KEY,
             user_id INT REFERENCES users (id) DEFERRABLE INITIALLY DEFERRED);
         GRANT SELECT, INSERT ON memberships TO app;";
    let starting = Pg2Sqlite::default().sql(pg).expect("parse").build_schema().expect("schema");
    let (mut connection, _) = replica_with(pg, &options());
    let migration = Pg2Sqlite::default()
        .sql("ALTER TABLE users DROP COLUMN email;")
        .expect("parse")
        .translate_to_sql_with_schema(&starting, &options())
        .expect("translate");
    let before_commit = migration.before_commit.clone();
    for statement in migration.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection.batch_execute("INSERT INTO memberships VALUES (1, 1)").expect("membership");
    // The view built with the starting schema still reads the dropped column.
    let error = transaction(&mut connection, &before_commit, "DELETE FROM users_rls WHERE id = 1")
        .expect_err("the membership still references the user at commit");
    assert!(error.to_string().contains(REFUSAL), "{error}");
}

#[test]
fn dropping_a_key_column_before_the_policy_arrives_drops_the_referenced_side() {
    let pg = "CREATE ROLE app;
         CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL);
         CREATE TABLE memberships (id INT PRIMARY KEY, user_id INT REFERENCES users (id));
         ALTER TABLE memberships DROP COLUMN user_id;
         ALTER TABLE users ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, DELETE ON users TO app;
         GRANT SELECT, INSERT ON memberships TO app;
         CREATE POLICY users_select ON users FOR SELECT TO app USING (name = 'visible');
         CREATE POLICY users_delete ON users FOR DELETE TO app USING (true);";
    let (mut connection, _) = replica_with(pg, &options());
    connection
        .batch_execute("DELETE FROM users WHERE id = 1")
        .expect("nothing references the user");
}
