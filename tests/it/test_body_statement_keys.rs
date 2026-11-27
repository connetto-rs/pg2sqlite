//! Foreign keys are checked at each PL/pgSQL statement boundary.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, postgres_error};
use sqlstate::pg;

fn translated(schema: &str, body: &str, options: &Pg2SqliteOptions) -> Vec<String> {
    let source = format!(
        "{schema}
         CREATE TABLE requests (id INT PRIMARY KEY);
         GRANT ALL ON requests TO PUBLIC;
         CREATE FUNCTION run_request() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN {body} RETURN NEW; END $$;
         CREATE TRIGGER run_request AFTER INSERT ON requests
         FOR EACH ROW EXECUTE FUNCTION run_request();"
    );
    Pg2Sqlite::default()
        .sql(&source)
        .expect("parse schema")
        .translate_to_sql(options)
        .expect("translate schema")
        .into_vec()
}

fn replica(schema: &str, body: &str) -> crate::helpers::Replica {
    let statements = translated(schema, body, &Pg2SqliteOptions::default());
    let mut connection = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    connection.batch_execute("PRAGMA foreign_keys = ON;").expect("enable foreign keys");
    for statement in &statements {
        connection.batch_execute(statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

fn refusal(connection: &mut crate::helpers::Replica) -> sqlstate::SqlState {
    let error = connection
        .batch_execute("INSERT INTO requests VALUES (1)")
        .expect_err("the first orphaning statement must fail")
        .to_string();
    postgres_error(&error).unwrap_or_else(|| panic!("missing SQLSTATE in {error}")).sqlstate()
}

fn schema(action: &str, self_reference: bool) -> String {
    if self_reference {
        format!(
            "CREATE TABLE t (id INT PRIMARY KEY, parent INT REFERENCES t(id) {action});
             INSERT INTO t VALUES (1, NULL), (2, 1);"
        )
    } else {
        format!(
            "CREATE TABLE p (id INT PRIMARY KEY);
             CREATE TABLE t (id INT PRIMARY KEY, parent INT REFERENCES p(id) {action});
             INSERT INTO p VALUES (1);
             INSERT INTO t VALUES (2, 1);"
        )
    }
}

#[test]
fn parent_delete_is_checked_before_a_later_repair() {
    for self_reference in [false, true] {
        for action in ["", "ON DELETE NO ACTION", "DEFERRABLE INITIALLY IMMEDIATE"] {
            let parent = if self_reference { "t" } else { "p" };
            for predicate in ["id = NEW.id", "id < 2"] {
                let body = format!(
                    "DELETE FROM {parent} WHERE {predicate};
                     DELETE FROM t WHERE parent = NEW.id;"
                );
                let mut connection = replica(&schema(action, self_reference), &body);
                assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
            }
        }
    }
}

#[test]
fn converted_self_restrict_keeps_its_sqlstate() {
    let mut connection = replica(
        &schema("ON DELETE RESTRICT", true),
        "DELETE FROM t WHERE id = NEW.id; DELETE FROM t WHERE parent = NEW.id;",
    );
    assert_eq!(refusal(&mut connection), pg::RESTRICT_VIOLATION);
}

#[test]
fn referencing_insert_and_update_are_checked_before_repair() {
    for action in [
        "",
        "ON DELETE RESTRICT",
        "ON DELETE CASCADE",
        "ON DELETE SET NULL",
        "ON DELETE SET DEFAULT",
    ] {
        for body in [
            "INSERT INTO t VALUES (3, 9); INSERT INTO p VALUES (9);",
            "UPDATE t SET parent = 9; INSERT INTO p VALUES (9);",
        ] {
            let mut connection = replica(&schema(action, false), body);
            assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
        }
    }
}

#[test]
fn referenced_update_is_checked_before_repair() {
    for self_reference in [false, true] {
        for action in ["", "ON UPDATE NO ACTION", "ON UPDATE RESTRICT"] {
            if !self_reference && action == "ON UPDATE RESTRICT" {
                continue;
            }
            let parent = if self_reference { "t" } else { "p" };
            let body = format!(
                "UPDATE {parent} SET id = 3 WHERE id = NEW.id;
                 UPDATE t SET parent = 3 WHERE parent = NEW.id;"
            );
            let mut connection = replica(&schema(action, self_reference), &body);
            let expected = if action == "ON UPDATE RESTRICT" {
                pg::RESTRICT_VIOLATION
            } else {
                pg::FOREIGN_KEY_VIOLATION
            };
            assert_eq!(refusal(&mut connection), expected);
        }
    }
}

#[test]
fn untaken_branches_and_non_orphaning_bodies_run() {
    for body in [
        "IF NEW.id = 9 THEN DELETE FROM p WHERE id = 1; END IF;",
        "DELETE FROM t WHERE parent = NEW.id; DELETE FROM p WHERE id = NEW.id;",
        "UPDATE t SET id = 3; PERFORM 1;",
    ] {
        let mut connection = replica(&schema("", false), body);
        connection.batch_execute("INSERT INTO requests VALUES (1)").expect("valid body");
    }
}

#[test]
fn deferred_keys_allow_a_later_repair() {
    let mut connection = replica(
        &schema("DEFERRABLE INITIALLY DEFERRED", false),
        "DELETE FROM p WHERE id = NEW.id; DELETE FROM t WHERE parent = NEW.id;",
    );
    connection.batch_execute("INSERT INTO requests VALUES (1)").expect("deferred repair");
}

#[test]
fn an_unrepaired_body_reports_the_statement_error() {
    let mut connection = replica(&schema("", false), "DELETE FROM p WHERE id = NEW.id; PERFORM 1;");
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn self_update_reports_which_side_left_the_orphan() {
    for (body, expected) in [
        (
            "UPDATE t SET id = id, parent = 999 WHERE id = 2; DELETE FROM t;",
            pg::FOREIGN_KEY_VIOLATION,
        ),
        ("UPDATE t SET id = id + 10, parent = parent; DELETE FROM t;", pg::RESTRICT_VIOLATION),
    ] {
        let mut connection = replica(&schema("ON UPDATE RESTRICT", true), body);
        assert_eq!(refusal(&mut connection), expected);
    }
}

#[test]
fn native_parent_actions_run_without_boundary_refusals() {
    for action in ["ON DELETE CASCADE", "ON DELETE SET NULL", "ON DELETE SET DEFAULT"] {
        let mut connection =
            replica(&schema(action, false), "DELETE FROM p WHERE id = NEW.id; PERFORM 1;");
        connection.batch_execute("INSERT INTO requests VALUES (1)").expect("key action");
    }
}

#[test]
fn native_restrict_still_refuses_before_repair() {
    let mut connection = replica(
        &schema("ON DELETE RESTRICT", false),
        "DELETE FROM p WHERE id = NEW.id; DELETE FROM t WHERE parent = NEW.id;",
    );
    let error = connection
        .batch_execute("INSERT INTO requests VALUES (1)")
        .expect_err("native RESTRICT refuses")
        .to_string();
    assert!(error.contains("FOREIGN KEY constraint failed"), "{error}");
}

#[test]
fn truncation_of_both_sides_needs_no_boundary_check() {
    let mut connection = replica(&schema("", false), "TRUNCATE p, t;");
    connection.batch_execute("INSERT INTO requests VALUES (1)").expect("truncate both sides");
}

#[test]
fn a_fired_trigger_checks_its_own_statements() {
    let schema = format!(
        "{}
         CREATE TABLE nested_requests (id INT PRIMARY KEY);
         CREATE FUNCTION nested_request() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           DELETE FROM p WHERE id = NEW.id;
           DELETE FROM t WHERE parent = NEW.id;
           RETURN NEW;
         END $$;
         CREATE TRIGGER nested_request AFTER INSERT ON nested_requests
         FOR EACH ROW EXECUTE FUNCTION nested_request();",
        schema("", false)
    );
    let mut connection = replica(&schema, "INSERT INTO nested_requests VALUES (NEW.id);");
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn an_outer_multirow_update_can_temporarily_leave_an_unwritten_reference() {
    let mut connection = replica(
        "CREATE TABLE t (id INT PRIMARY KEY, parent INT REFERENCES t(id));
         INSERT INTO t VALUES (1, NULL), (2, 1);
         CREATE FUNCTION touch_child() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           UPDATE t SET parent = parent WHERE id = NEW.id;
           RETURN NEW;
         END $$;
         CREATE TRIGGER touch_child AFTER UPDATE OF id ON t
         FOR EACH ROW EXECUTE FUNCTION touch_child();",
        "",
    );
    connection
        .batch_execute("UPDATE t SET id = id + 10, parent = parent + 10")
        .expect("the outer statement restores every reference");
}

#[test]
fn omitted_keys_have_no_body_check() {
    let statements = translated(
        &schema("", false),
        "DELETE FROM p WHERE id = NEW.id; INSERT INTO t VALUES (3, 999);",
        &Pg2SqliteOptions::default().with_foreign_keys_omitted(),
    );
    let trigger = statements
        .iter()
        .find(|statement| statement.starts_with("CREATE TRIGGER"))
        .expect("body trigger");
    assert!(!trigger.contains("violates foreign key constraint"), "{trigger}");
}

#[test]
fn hidden_referenced_rows_have_no_body_check() {
    let options = Pg2SqliteOptions::default()
        .with_deployment(crate::helpers::deployment::deployment(["app"], []))
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("key_write_exempt")
        .with_acting_role("app");
    let statements = translated(
        "CREATE ROLE app;
         CREATE TABLE p (id INT PRIMARY KEY);
         ALTER TABLE p ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p_read ON p FOR SELECT TO app USING (id > 1);
         CREATE POLICY p_delete ON p FOR DELETE TO app USING (true);
         CREATE TABLE t (id INT PRIMARY KEY, parent INT REFERENCES p(id));
         GRANT ALL ON p, t TO app;",
        "DELETE FROM p WHERE id = NEW.id; INSERT INTO t VALUES (3, 999);",
        &options,
    );
    let triggers: Vec<_> = statements
        .iter()
        .filter(|statement| {
            statement.starts_with("CREATE TRIGGER")
                && (statement.contains(" ON requests ") || statement.contains(" ON \"requests\" "))
        })
        .collect();
    assert!(!triggers.is_empty(), "the request trigger must be present");
    for trigger in triggers {
        assert!(!trigger.contains("_body_records"), "{trigger}");
    }
}

#[test]
fn an_inlined_helper_checks_each_statement() {
    let schema = format!(
        "{}
         CREATE FUNCTION remove_parent(target INT) RETURNS void LANGUAGE plpgsql AS $$
         BEGIN
           DELETE FROM p WHERE id = target;
           DELETE FROM t WHERE parent = target;
         END $$;",
        schema("", false)
    );
    let mut connection = replica(&schema, "PERFORM remove_parent(NEW.id);");
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn a_composite_key_checks_only_wholly_nonnull_references() {
    for parent in ["1, 2", "NULL, 2"] {
        let schema = format!(
            "CREATE TABLE p (first INT, second INT, PRIMARY KEY (first, second));
             CREATE TABLE t (id INT PRIMARY KEY, first INT, second INT,
               CONSTRAINT composite_key FOREIGN KEY (first, second) REFERENCES p(first, second));
             INSERT INTO p VALUES (1, 2);
             INSERT INTO t VALUES (1, {parent});"
        );
        let mut connection =
            replica(&schema, "DELETE FROM p WHERE first = NEW.id AND second = 2; DELETE FROM t;");
        if parent == "1, 2" {
            assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
        } else {
            connection.batch_execute("INSERT INTO requests VALUES (1)").expect("null key");
        }
    }
}

#[test]
fn referencing_updates_keep_track_of_a_changed_row_identity() {
    let mut connection = replica(
        &schema("ON UPDATE RESTRICT", true),
        "UPDATE t SET id = id + 10, parent = 999 WHERE id = 2; DELETE FROM t;",
    );
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn insert_select_checks_its_inserted_rows_before_repair() {
    let mut connection = replica(
        &schema("", false),
        "INSERT INTO t SELECT id + 10, 999 FROM p; INSERT INTO p VALUES (999);",
    );
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn an_upsert_checks_its_referencing_update_before_repair() {
    let mut connection = replica(
        &schema("", false),
        "INSERT INTO t VALUES (2, 999) ON CONFLICT (id) DO UPDATE SET parent = EXCLUDED.parent;
         INSERT INTO p VALUES (999);",
    );
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn a_definer_helper_checks_each_statement() {
    let schema = format!(
        "{}
         CREATE ROLE cleaner;
         CREATE FUNCTION remove_parent(target INT) RETURNS void
         LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
         BEGIN
           DELETE FROM public.p WHERE id = target;
           DELETE FROM public.t WHERE parent = target;
         END $$;
         ALTER FUNCTION remove_parent(INT) OWNER TO cleaner;
         GRANT ALL ON p, t TO cleaner;",
        schema("", false)
    );
    let mut connection = replica(&schema, "PERFORM remove_parent(NEW.id);");
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

diesel::define_sql_function! {
    /// Whether a write passes the guards, which no write in these tests does.
    fn key_write_exempt() -> diesel::sql_types::Bool;
}

#[test]
fn a_definer_body_checks_keys_whose_policies_call_what_its_owner_may_not() {
    let options = Pg2SqliteOptions::default()
        .with_deployment(crate::helpers::deployment::deployment(["app"], []))
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("key_write_exempt")
        .with_acting_role("app");
    let mut connection = crate::helpers::apply_with_options(
        "CREATE ROLE app;
         CREATE ROLE cleaner;
         CREATE FUNCTION visible(target INT) RETURNS boolean
         LANGUAGE sql STABLE AS $$ SELECT target > 0 $$;
         REVOKE EXECUTE ON FUNCTION visible(INT) FROM PUBLIC;
         GRANT EXECUTE ON FUNCTION visible(INT) TO app;
         CREATE TABLE hidden (id INT PRIMARY KEY);
         ALTER TABLE hidden ENABLE ROW LEVEL SECURITY;
         CREATE POLICY hidden_read ON hidden FOR SELECT TO app USING (visible(id));
         CREATE TABLE hiding (id INT PRIMARY KEY, hidden INT REFERENCES hidden(id));
         CREATE TABLE p (id INT PRIMARY KEY);
         CREATE TABLE t (id INT PRIMARY KEY, parent INT REFERENCES p(id));
         CREATE TABLE requests (id INT PRIMARY KEY);
         CREATE FUNCTION remove_parent() RETURNS trigger
         LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
         BEGIN
           DELETE FROM public.p WHERE id = NEW.id;
           DELETE FROM public.t WHERE parent = NEW.id;
           RETURN NEW;
         END $$;
         ALTER FUNCTION remove_parent() OWNER TO cleaner;
         CREATE TRIGGER remove_parent AFTER INSERT ON requests
         FOR EACH ROW EXECUTE FUNCTION remove_parent();
         GRANT ALL ON hidden, hiding, p, t, requests TO app;
         GRANT ALL ON p, t TO cleaner;",
        &options,
    )
    .expect("the owner's missing EXECUTE on visible() decides nothing about the keys");
    key_write_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        || false,
    )
    .expect("register the exemption");
    connection.batch_execute("INSERT INTO p VALUES (1); INSERT INTO t VALUES (2, 1);").unwrap();
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn an_unrelated_column_update_leaves_the_unchanged_key_to_the_outer_statement() {
    let mut connection = replica(
        "CREATE TABLE p (id INT PRIMARY KEY);
         CREATE TABLE t (id INT PRIMARY KEY, parent INT REFERENCES p(id), payload INT);
         CREATE FUNCTION repair_parent() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           UPDATE t SET payload = 1 WHERE id = NEW.id;
           INSERT INTO p VALUES (NEW.parent);
           RETURN NEW;
         END $$;
         CREATE TRIGGER \"A_repair_parent\" AFTER INSERT ON t
         FOR EACH ROW EXECUTE FUNCTION repair_parent();",
        "",
    );
    connection
        .batch_execute("INSERT INTO t VALUES (1, 999, 0)")
        .expect("the replica leaves the unchanged key to the outer statement");
}

#[test]
fn an_unchanged_committed_reference_can_be_repaired_later_in_the_body() {
    let mut connection = replica(
        "CREATE TABLE p (id INT PRIMARY KEY);
         CREATE TABLE t (id INT PRIMARY KEY, parent INT REFERENCES p(id), payload INT);
         INSERT INTO p VALUES (1);
         INSERT INTO t VALUES (1, 1, 0);
         CREATE FUNCTION repair_child() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           UPDATE t SET payload = 1 WHERE parent = OLD.id;
           UPDATE t SET parent = NEW.id WHERE parent = OLD.id;
           RETURN NEW;
         END $$;
         CREATE TRIGGER \"A_repair_child\" AFTER UPDATE ON p
         FOR EACH ROW EXECUTE FUNCTION repair_child();",
        "",
    );
    connection
        .batch_execute("UPDATE p SET id = id + 10")
        .expect("the unchanged committed key is checked after its repair");
}

#[test]
fn a_skipped_nested_statement_keeps_the_enclosing_statement_check() {
    let schema = format!(
        "{}
         CREATE FUNCTION skip_inner_write() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF false THEN INSERT INTO t VALUES (3, 999); END IF;
           RETURN OLD;
         END $$;
         CREATE TRIGGER skip_inner_write AFTER DELETE ON p
         FOR EACH ROW EXECUTE FUNCTION skip_inner_write();",
        schema("", false)
    );
    let mut connection =
        replica(&schema, "DELETE FROM p WHERE id = NEW.id; DELETE FROM t WHERE parent = NEW.id;");
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn a_skipped_nested_statement_does_not_check_the_enclosing_statement_early() {
    let schema = format!(
        "{}
         CREATE FUNCTION restore_parent() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF false THEN INSERT INTO t VALUES (3, 999); END IF;
           INSERT INTO p VALUES (OLD.id);
           RETURN OLD;
         END $$;
         CREATE TRIGGER restore_parent AFTER DELETE ON p
         FOR EACH ROW EXECUTE FUNCTION restore_parent();",
        schema("", false)
    );
    let mut connection = replica(&schema, "DELETE FROM p WHERE id = NEW.id;");
    connection
        .batch_execute("INSERT INTO requests VALUES (1)")
        .expect("the enclosing statement restores its removed key");
}

#[test]
fn key_action_writes_are_checked_with_the_body_statement() {
    let mut connection = replica(
        "CREATE TABLE p (id INT PRIMARY KEY);
         CREATE TABLE t (id INT PRIMARY KEY,
           parent INT DEFAULT 999 REFERENCES p(id) ON UPDATE SET DEFAULT);
         INSERT INTO p VALUES (1);
         INSERT INTO t VALUES (2, 1);",
        "UPDATE p SET id = id + 10 WHERE id = NEW.id; INSERT INTO p VALUES (999);",
    );
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn a_table_without_an_accessible_row_identity_remains_unchecked() {
    let mut connection = replica(
        "CREATE TABLE p (id INT PRIMARY KEY);
         CREATE TABLE t (rowid INT, _rowid_ INT, oid INT, parent INT REFERENCES p(id));",
        "INSERT INTO t VALUES (1, 2, 3, 999); INSERT INTO p VALUES (999);",
    );
    connection
        .batch_execute("INSERT INTO requests VALUES (1)")
        .expect("the later statement repairs the unchecked reference");
}

diesel::table! {
    #[sql_name = "t_t_parent_fkey_pg2sqlite_key_body_records"]
    body_records (frame, side) {
        frame -> BigInt,
        side -> Integer,
    }
}

diesel::table! {
    pg2sqlite_definer_frames (id) {
        id -> BigInt,
        owner -> Text,
    }
}

fn captures_are_empty(connection: &mut crate::helpers::Replica) {
    assert_eq!(body_records::table.count().get_result::<i64>(connection).unwrap(), 0);
    assert_eq!(pg2sqlite_definer_frames::table.count().get_result::<i64>(connection).unwrap(), 0);
}

#[test]
fn successful_and_aborted_bodies_leave_no_captured_events() {
    let mut success =
        replica(&schema("", false), "INSERT INTO t VALUES (3, 1); DELETE FROM t WHERE id = 3;");
    success.batch_execute("INSERT INTO requests VALUES (1)").expect("valid reference");
    captures_are_empty(&mut success);
    let mut abort =
        replica(&schema("", false), "INSERT INTO t VALUES (3, 999); INSERT INTO p VALUES (999);");
    assert_eq!(refusal(&mut abort), pg::FOREIGN_KEY_VIOLATION);
    captures_are_empty(&mut abort);
}

#[test]
fn renaming_a_child_keeps_existing_body_checks_connected() {
    let source = format!(
        "{}
         CREATE TABLE commands (id INT PRIMARY KEY);
         CREATE FUNCTION remove_parent() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN DELETE FROM p WHERE id = NEW.id; RETURN NEW; END $$;
         CREATE TRIGGER remove_parent AFTER INSERT ON commands
         FOR EACH ROW EXECUTE FUNCTION remove_parent();
         ALTER TABLE t RENAME TO children;",
        schema("", false),
    );
    let mut connection = replica(&source, "INSERT INTO commands VALUES (NEW.id);");
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn a_nested_repair_does_not_check_an_untouched_outer_row() {
    for repair in [
        "UPDATE t SET id = 101, parent = 11 WHERE id = NEW.id;",
        "DELETE FROM t WHERE id = NEW.id;",
    ] {
        let source = format!(
            "CREATE TABLE t (id INT PRIMARY KEY, parent INT REFERENCES t(id));
             INSERT INTO t VALUES (1, NULL), (2, 1);
             CREATE FUNCTION repair_insert() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN IF NEW.id = 100 THEN {repair} END IF; RETURN NEW; END $$;
             CREATE TRIGGER repair_insert AFTER INSERT ON t
             FOR EACH ROW EXECUTE FUNCTION repair_insert();
             CREATE FUNCTION write_child() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
               IF NEW.id = 11 THEN INSERT INTO t VALUES (100, 1); END IF;
               RETURN NEW;
             END $$;
             CREATE TRIGGER write_child AFTER UPDATE OF id ON t
             FOR EACH ROW EXECUTE FUNCTION write_child();"
        );
        let mut connection = replica(&source, "");
        connection
            .batch_execute("UPDATE t SET id = id + 10, parent = parent + 10 WHERE id < 10")
            .expect("the nested statement repairs only its written row");
        captures_are_empty(&mut connection);
    }
}

#[test]
fn a_nested_identity_change_keeps_the_enclosing_insert_event() {
    let source = format!(
        "{}
         CREATE FUNCTION move_insert() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN UPDATE t SET id = 101 WHERE id = NEW.id; RETURN NEW; END $$;
         CREATE TRIGGER move_insert AFTER INSERT ON t
         FOR EACH ROW EXECUTE FUNCTION move_insert();",
        schema("", false),
    );
    let mut connection = replica(
        &source,
        "INSERT INTO t VALUES (100, 999); UPDATE t SET parent = 1 WHERE id = 101;",
    );
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn removing_a_key_removes_it_from_existing_body_checks() {
    for removal in ["ALTER TABLE t DROP COLUMN parent", "DROP TABLE t"] {
        let source = format!(
            "CREATE TABLE p (id INT PRIMARY KEY);
             CREATE TABLE t (id INT PRIMARY KEY, parent INT,
               CONSTRAINT child_parent FOREIGN KEY (parent) REFERENCES p(id));
             INSERT INTO p VALUES (1);
             INSERT INTO t VALUES (2, 1);
             CREATE TABLE commands (id INT PRIMARY KEY);
             CREATE FUNCTION remove_parent() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN DELETE FROM p WHERE id = NEW.id; RETURN NEW; END $$;
             CREATE TRIGGER remove_parent AFTER INSERT ON commands
             FOR EACH ROW EXECUTE FUNCTION remove_parent();
             {removal};"
        );
        let mut connection = replica(&source, "INSERT INTO commands VALUES (NEW.id);");
        connection
            .batch_execute("INSERT INTO requests VALUES (1)")
            .unwrap_or_else(|error| panic!("{removal} left a stale body check: {error}"));
    }
}

diesel::table! {
    sqlite_master (name) {
        name -> Text,
    }
}

#[test]
fn temporary_keys_keep_their_captures_in_temporary_storage() {
    let mut connection = replica(
        "CREATE TEMP TABLE p (id INT PRIMARY KEY);
         CREATE TEMP TABLE t (id INT PRIMARY KEY, parent INT REFERENCES p(id));
         INSERT INTO p VALUES (1);
         INSERT INTO t VALUES (2, 1);",
        "DELETE FROM p WHERE id = NEW.id; DELETE FROM t;",
    );
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
    captures_are_empty(&mut connection);
    assert_eq!(
        sqlite_master::table
            .filter(sqlite_master::name.eq("t_t_parent_fkey_pg2sqlite_key_body_records"))
            .count()
            .get_result::<i64>(&mut connection)
            .unwrap(),
        0,
    );
}

fn migrated_replica(schema: &str, body: &str, migration: &str) -> crate::helpers::Replica {
    replica(
        &format!(
            "{schema}
             CREATE TABLE commands (id INT PRIMARY KEY);
             CREATE FUNCTION migrate_body() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN {body} RETURN NEW; END $$;
             CREATE TRIGGER migrate_body AFTER INSERT ON commands
             FOR EACH ROW EXECUTE FUNCTION migrate_body();
             {migration};"
        ),
        "INSERT INTO commands VALUES (NEW.id);",
    )
}

#[test]
fn a_key_added_later_checks_an_existing_body() {
    let mut connection = migrated_replica(
        "CREATE TABLE p (id INT PRIMARY KEY);
         CREATE TABLE t (id INT PRIMARY KEY);
         INSERT INTO p VALUES (1);
         INSERT INTO t VALUES (2);",
        "DELETE FROM p WHERE id = NEW.id; DELETE FROM t;",
        "ALTER TABLE t ADD COLUMN parent INT REFERENCES p(id);
         UPDATE t SET parent = 1",
    );
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn changing_key_deferral_reemits_existing_bodies() {
    for (before, after, deferred) in
        [("", "DEFERRABLE INITIALLY DEFERRED", true), ("DEFERRABLE INITIALLY DEFERRED", "", false)]
    {
        let mut connection = migrated_replica(
            &schema(before, false),
            "DELETE FROM p WHERE id = NEW.id; DELETE FROM t;",
            &format!(
                "ALTER TABLE t DROP COLUMN parent;
                 ALTER TABLE t ADD COLUMN parent INT REFERENCES p(id) {after};
                 UPDATE t SET parent = 1"
            ),
        );
        if deferred {
            connection.batch_execute("INSERT INTO requests VALUES (1)").unwrap();
        } else {
            assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
        }
    }
}

#[test]
fn a_renamed_key_column_keeps_existing_body_checks_connected() {
    let mut connection = migrated_replica(
        &schema("", false),
        "DELETE FROM p WHERE id = NEW.id; DELETE FROM t;",
        "ALTER TABLE t RENAME COLUMN parent TO ancestor",
    );
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn a_replaced_body_gets_the_current_keys_checks() {
    let mut connection = migrated_replica(
        &schema("", false),
        "PERFORM 1;",
        "CREATE OR REPLACE FUNCTION migrate_body() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN DELETE FROM p WHERE id = NEW.id; DELETE FROM t; RETURN NEW; END $$",
    );
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn dropping_the_last_body_removes_its_capture_storage() {
    for migration in
        ["DROP TRIGGER migrate_body ON commands", "DROP FUNCTION migrate_body() CASCADE"]
    {
        let mut connection =
            migrated_replica(&schema("", false), "DELETE FROM p WHERE id = NEW.id;", migration);
        connection.batch_execute("INSERT INTO requests VALUES (1)").unwrap();
        assert_eq!(
            sqlite_master::table
                .filter(sqlite_master::name.like("%_body_records"))
                .count()
                .get_result::<i64>(&mut connection)
                .unwrap(),
            0,
            "{migration}",
        );
    }
}

#[test]
fn losing_a_row_identity_removes_existing_body_checks() {
    let mut connection = migrated_replica(
        "CREATE TABLE p (id INT PRIMARY KEY);
         CREATE TABLE t (id INT PRIMARY KEY DEFAULT 10, _rowid_ INT, oid INT, rowid INT, parent INT REFERENCES p(id));
         INSERT INTO t (oid, rowid, parent) VALUES (1, 2, NULL);",
        "UPDATE t SET parent = 999; INSERT INTO p VALUES (999);",
        "ALTER TABLE t DROP COLUMN id",
    );
    connection.batch_execute("INSERT INTO requests VALUES (1)").unwrap();
}

#[test]
fn a_body_naming_a_later_table_gets_its_keys_checks() {
    let source = "CREATE TABLE commands (id INT PRIMARY KEY);
         CREATE FUNCTION write_missing() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN DELETE FROM p WHERE id = NEW.id; DELETE FROM t; RETURN NEW; END $$;
         CREATE TRIGGER write_missing AFTER INSERT ON commands
         FOR EACH ROW EXECUTE FUNCTION write_missing();
         CREATE TABLE p (id INT PRIMARY KEY);
         CREATE TABLE t (id INT PRIMARY KEY, parent INT REFERENCES p(id));
         INSERT INTO p VALUES (1);
         INSERT INTO t VALUES (2, 1);";
    let mut connection = replica(source, "INSERT INTO commands VALUES (NEW.id);");
    assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
}

#[test]
fn body_frames_preserve_generated_values_shared_by_statements() {
    let source = "CREATE TABLE p (id UUID PRIMARY KEY);
         CREATE TABLE t (id UUID PRIMARY KEY REFERENCES p(id));
         CREATE TABLE commands (id INT PRIMARY KEY);
         CREATE FUNCTION insert_pair() RETURNS trigger LANGUAGE plpgsql AS $$
         DECLARE generated_id UUID;
         BEGIN
             generated_id := uuidv7();
             INSERT INTO p (id) VALUES (generated_id);
             INSERT INTO t (id) VALUES (generated_id);
             RETURN NEW;
         END $$;
         CREATE TRIGGER insert_pair AFTER INSERT ON commands
         FOR EACH ROW EXECUTE FUNCTION insert_pair();";
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(pg2sqlite::prelude::UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7");
    let statements = Pg2Sqlite::default().sql(source).unwrap().translate_to_sql(&options).unwrap();
    let mut connection = crate::helpers::establish_connection();
    for statement in statements.into_vec() {
        connection.batch_execute(&statement).unwrap();
    }
    connection.batch_execute("INSERT INTO commands VALUES (1), (2)").unwrap();
}

#[test]
fn body_checks_run_with_an_existing_rowid_frame_table() {
    for (body, fails) in [
        ("DELETE FROM p WHERE id = NEW.id; DELETE FROM t;", true),
        ("DELETE FROM t; DELETE FROM p WHERE id = NEW.id;", false),
    ] {
        let mut connection = crate::helpers::Replica::establish(":memory:").unwrap();
        connection.batch_execute("CREATE TABLE pg2sqlite_definer_frames (id INTEGER PRIMARY KEY, owner TEXT NOT NULL CHECK (owner <> '' AND length(owner) <= 63)) STRICT").unwrap();
        for statement in translated(&schema("", false), body, &Pg2SqliteOptions::default()) {
            connection.batch_execute(&statement).unwrap();
        }
        if fails {
            assert_eq!(refusal(&mut connection), pg::FOREIGN_KEY_VIOLATION);
        } else {
            connection.batch_execute("INSERT INTO requests VALUES (1)").unwrap();
        }
        captures_are_empty(&mut connection);
    }
}

#[test]
fn recording_triggers_skip_writes_outside_body_frames() {
    let statements = translated(
        &schema("", false),
        "DELETE FROM t; DELETE FROM p;",
        &Pg2SqliteOptions::default(),
    );
    let mut observers = 0;
    for statement in &statements {
        if statement.starts_with("CREATE TRIGGER t_t_parent_fkey_pg2sqlite_key_body_") {
            observers += 1;
            assert!(statement.contains("FOR EACH ROW WHEN EXISTS (SELECT 1 FROM pg2sqlite_definer_frames WHERE owner = 'pg_body_statement') BEGIN"), "{statement}");
        }
    }
    assert_eq!(observers, 5);
}
