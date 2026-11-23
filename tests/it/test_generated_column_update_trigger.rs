use crate::helpers::{run_translated::run_translated_default, translate::assert_refused};

/// Generated-column trigger gates follow PostgreSQL's timing and assignment
/// matrix.
#[test]
fn generated_column_update_trigger_matches_assignment_matrix() {
    for storage in ["STORED", "VIRTUAL"] {
        for timing in ["BEFORE", "AFTER"] {
            for assignments in ["b = DEFAULT, c = c", "a = 2", "a = 2, b = DEFAULT", "c = 1"] {
                let sql = format!(
                    "CREATE TABLE subject (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) {storage}, c INTEGER);
                     CREATE TABLE audit (fired INTEGER);
                     CREATE FUNCTION record_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
                     BEGIN
                         INSERT INTO audit VALUES (1);
                         RETURN NEW;
                     END;
                     $$;
                     CREATE TRIGGER generated_update {timing} UPDATE OF b ON subject
                     FOR EACH ROW EXECUTE FUNCTION record_trigger();
                     INSERT INTO subject (a, c) VALUES (1, 0);
                     UPDATE subject SET {assignments};
                     SELECT count(*) FROM audit;"
                );
                if timing == "AFTER" && assignments == "b = DEFAULT, c = c" {
                    assert_refused(&sql, "cannot preserve AFTER UPDATE OF trigger");
                    continue;
                }
                let expected = if timing == "AFTER" && assignments == "c = 1" { "0" } else { "1" };
                assert_eq!(
                    run_translated_default(&sql),
                    vec![Some(expected.into())],
                    "{storage} {timing} {assignments}"
                );
            }
        }
    }
}

/// An `AFTER` generated-column gate excludes unrelated updates.
#[test]
fn generated_column_in_mixed_update_list_excludes_unrelated_columns() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE subject (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) STORED, c INTEGER);
             CREATE TABLE audit (fired INTEGER);
             CREATE FUNCTION record_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN INSERT INTO audit VALUES (1); RETURN NEW; END;
             $$;
             CREATE TRIGGER generated_update AFTER UPDATE OF a, b ON subject
             FOR EACH ROW EXECUTE FUNCTION record_trigger();
             INSERT INTO subject (a, c) VALUES (1, 0);
             UPDATE subject SET c = 1;
             SELECT count(*) FROM audit;"
        ),
        vec![Some("0".into())]
    );
}

/// Generated and ordinary columns keep separate trigger gates.
#[test]
fn generated_and_ordinary_update_triggers_keep_separate_gates() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE subject (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) STORED, c INTEGER);
             CREATE TABLE audit (fired INTEGER);
             CREATE FUNCTION record_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN INSERT INTO audit VALUES (1); RETURN NEW; END;
             $$;
             CREATE TRIGGER generated_update AFTER UPDATE OF b ON subject
             FOR EACH ROW EXECUTE FUNCTION record_trigger();
             CREATE TRIGGER ordinary_update AFTER UPDATE OF a ON subject
             FOR EACH ROW EXECUTE FUNCTION record_trigger();
             INSERT INTO subject (a, c) VALUES (1, 0);
             UPDATE subject SET c = 1;
             UPDATE subject SET a = 2;
             SELECT count(*) FROM audit;"
        ),
        vec![Some("2".into())]
    );
}

/// Creating and dropping a row-level `BEFORE UPDATE` trigger changes generated
/// `AFTER` gates.
#[test]
fn before_update_presence_rebuilds_generated_after_gate() {
    for storage in ["STORED", "VIRTUAL"] {
        for event in ["UPDATE", "UPDATE OF a", "UPDATE OF b", "UPDATE OF c"] {
            for condition in ["", "WHEN (FALSE)"] {
                let sql = format!(
                    "CREATE TABLE subject (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) {storage}, c INTEGER);
                     CREATE TABLE audit (fired INTEGER);
                     CREATE TABLE before_audit (fired INTEGER);
                     CREATE FUNCTION record_after() RETURNS trigger LANGUAGE plpgsql AS $$
                     BEGIN INSERT INTO audit VALUES (1); RETURN NEW; END; $$;
                     CREATE FUNCTION record_before() RETURNS trigger LANGUAGE plpgsql AS $$
                     BEGIN INSERT INTO before_audit VALUES (1); RETURN NEW; END; $$;
                     CREATE TRIGGER generated_update AFTER UPDATE OF b ON subject
                     FOR EACH ROW EXECUTE FUNCTION record_after();
                     INSERT INTO subject (a, c) VALUES (1, 0);
                     UPDATE subject SET c = 1;
                     CREATE TRIGGER before_update BEFORE {event} ON subject
                     FOR EACH ROW {condition} EXECUTE FUNCTION record_before();
                     UPDATE subject SET c = 2;
                     DROP TRIGGER before_update ON subject;
                     UPDATE subject SET c = 3;
                     SELECT count(*) FROM audit;"
                );
                assert_eq!(
                    run_translated_default(&sql),
                    vec![Some("1".into())],
                    "{storage} {event} {condition}"
                );
            }
        }
    }
}

/// A preexisting `BEFORE UPDATE` trigger permits generated defaults without
/// input assignments.
#[test]
fn before_update_allows_generated_defaults_with_unrelated_assignments() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE subject (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) STORED, c INTEGER);
             CREATE TABLE audit (fired INTEGER);
             CREATE FUNCTION recorder() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN INSERT INTO audit VALUES (1); RETURN NEW; END; $$;
             CREATE TRIGGER before_update BEFORE UPDATE OF a ON subject
             FOR EACH ROW WHEN (FALSE) EXECUTE FUNCTION recorder();
             CREATE TRIGGER generated_update AFTER UPDATE OF b ON subject
             FOR EACH ROW EXECUTE FUNCTION recorder();
             INSERT INTO subject (a, c) VALUES (1, 0);
             UPDATE subject SET b = DEFAULT, c = c;
             SELECT count(*) FROM audit;"
        ),
        vec![Some("1".into())]
    );
}

/// A constant generated expression has no input-column update gate.
#[test]
fn constant_generated_column_does_not_fire_for_unrelated_updates() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE subject (a INTEGER, b INTEGER GENERATED ALWAYS AS (42) STORED);
             CREATE TABLE audit (fired INTEGER);
             CREATE FUNCTION recorder() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN INSERT INTO audit VALUES (1); RETURN NEW; END; $$;
             CREATE TRIGGER generated_update AFTER UPDATE OF b ON subject
             FOR EACH ROW EXECUTE FUNCTION recorder();
             INSERT INTO subject (a) VALUES (1);
             UPDATE subject SET a = 2;
             SELECT count(*) FROM audit;"
        ),
        vec![Some("0".into())]
    );
}

/// Statement triggers and disabling triggers retain their translation refusals.
#[test]
fn unsupported_trigger_states_are_refused() {
    for event in ["UPDATE", "UPDATE OF a", "UPDATE OF b", "UPDATE OF c", "INSERT"] {
        assert_refused(
            &format!(
                "CREATE TABLE subject (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) STORED, c INTEGER);
                 CREATE FUNCTION recorder() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END; $$;
                 CREATE TRIGGER before_update BEFORE {event} ON subject
                 FOR EACH STATEMENT EXECUTE FUNCTION recorder();"
            ),
            "statement trigger has no SQLite equivalent",
        );
    }
    assert_refused(
        "CREATE TABLE subject (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) STORED, c INTEGER);
         CREATE FUNCTION recorder() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END; $$;
         CREATE TRIGGER before_update BEFORE UPDATE ON subject
         FOR EACH ROW EXECUTE FUNCTION recorder();
         ALTER TABLE subject DISABLE TRIGGER before_update;",
        "does not exist",
    );
}

/// A `BEFORE INSERT` trigger does not widen an `AFTER UPDATE` gate.
#[test]
fn before_insert_does_not_widen_generated_update_gate() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE subject (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) STORED, c INTEGER);
             CREATE TABLE audit (fired INTEGER);
             CREATE FUNCTION recorder() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN INSERT INTO audit VALUES (1); RETURN NEW; END; $$;
             CREATE TRIGGER before_insert BEFORE INSERT ON subject
             FOR EACH ROW EXECUTE FUNCTION recorder();
             CREATE TRIGGER generated_update AFTER UPDATE OF b ON subject
             FOR EACH ROW EXECUTE FUNCTION recorder();
             INSERT INTO subject (a, c) VALUES (1, 0);
             DELETE FROM audit;
             UPDATE subject SET c = 1;
             SELECT count(*) FROM audit;"
        ),
        vec![Some("0".into())]
    );
}

/// Tuple assignments and upserts cannot discard a generated-column trigger
/// gate.
#[test]
fn generated_default_gate_loss_is_refused_for_tuple_and_upsert() {
    for write in [
        "UPDATE subject SET (b, c) = (DEFAULT, 1)",
        "INSERT INTO subject (a, c) VALUES (1, 1) ON CONFLICT (a) DO UPDATE SET b = DEFAULT, c = 1",
    ] {
        assert_refused(
            &format!(
                "CREATE TABLE subject (a INTEGER PRIMARY KEY, b INTEGER GENERATED ALWAYS AS (a * 2) STORED, c INTEGER);
                 CREATE TABLE audit (fired INTEGER);
                 CREATE FUNCTION recorder() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN INSERT INTO audit VALUES (1); RETURN NEW; END; $$;
                 CREATE TRIGGER generated_update AFTER UPDATE OF b ON subject
                 FOR EACH ROW EXECUTE FUNCTION recorder();
                 INSERT INTO subject (a, c) VALUES (1, 0);
                 {write};"
            ),
            "cannot preserve AFTER UPDATE OF trigger",
        );
    }
}

/// Seeded trigger catalogs preserve gate changes across translation batches.
#[test]
fn seeded_generated_gate_rebuilds_when_before_trigger_changes() {
    use diesel::{
        Connection, QueryDsl, RunQueryDsl, SqliteConnection, connection::SimpleConnection,
    };
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

    diesel::table! {
        audit (fired) {
            fired -> Integer,
        }
    }

    let setup =
        "CREATE TABLE subject (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) STORED, c INTEGER);
         CREATE TABLE audit (fired INTEGER);
         CREATE FUNCTION recorder() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN INSERT INTO audit VALUES (1); RETURN NEW; END; $$;
         CREATE TRIGGER generated_update AFTER UPDATE OF b ON subject
         FOR EACH ROW EXECUTE FUNCTION recorder();
         INSERT INTO subject (a, c) VALUES (1, 0);";
    let added = "CREATE TRIGGER before_update BEFORE UPDATE OF a ON subject
         FOR EACH ROW WHEN (FALSE) EXECUTE FUNCTION recorder();
         UPDATE subject SET c = 1;";
    let dropped = "DROP TRIGGER before_update ON subject;
         UPDATE subject SET c = 2;";
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    let mut written = String::new();
    for batch in [setup, added, dropped] {
        let parsed = Pg2Sqlite::default().sql(batch).unwrap();
        let options = Pg2SqliteOptions::default();
        let translated = if written.is_empty() {
            parsed.translate_to_sql(&options)
        } else {
            let schema = Pg2Sqlite::default().sql(&written).unwrap().build_schema().unwrap();
            parsed.translate_to_sql_with_schema(&schema, &options)
        }
        .unwrap();
        connection.batch_execute(&translated.into_vec().join(";\n")).unwrap();
        written.push_str(batch);
    }
    assert_eq!(audit::table.count().get_result::<i64>(&mut connection).unwrap(), 1);
}
