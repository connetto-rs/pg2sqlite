use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};

use crate::helpers::{Replica, establish_connection};

diesel::table! {
    inputs (id) {
        id -> Integer,
    }
}

diesel::table! {
    children (id) {
        id -> Integer,
    }
}

diesel::table! {
    observations (event, phase) {
        event -> Integer,
        phase -> Integer,
        token -> Binary,
    }
}

diesel::table! {
    candidates (id) {
        id -> Integer,
    }
}

diesel::table! {
    input_view (id) {
        id -> Integer,
    }
}

diesel::table! {
    pg2sqlite_definer_frames (id) {
        id -> BigInt,
        owner -> Text,
    }
}

diesel::table! {
    pg2sqlite_body_values (frame, iteration_site, iteration, slot) {
        frame -> BigInt,
        iteration_site -> BigInt,
        iteration -> BigInt,
        slot -> BigInt,
        value -> Nullable<Binary>,
    }
}

diesel::table! {
    maintenance_values (id) {
        id -> Integer,
        __pg2sqlite_capture_0 -> Binary,
        __pg2sqlite_reached_1 -> Binary,
        copied -> Binary,
    }
}

const TABLES: &str = "
CREATE TABLE inputs (id INTEGER PRIMARY KEY, tag INTEGER NOT NULL DEFAULT 0);
CREATE TABLE children (id INTEGER PRIMARY KEY);
CREATE TABLE observations (
    event INTEGER NOT NULL,
    phase INTEGER NOT NULL,
    token UUID NOT NULL,
    PRIMARY KEY (event, phase)
);
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
}

fn generated_value(sequence: usize) -> Vec<u8> {
    u128::try_from(sequence).unwrap().to_be_bytes().to_vec()
}

fn replica(postgres: &str) -> (Replica, Arc<AtomicUsize>) {
    let translated = Pg2Sqlite::default().sql(postgres).unwrap().translate(&options()).unwrap();
    let mut connection = establish_connection();
    let calls = Arc::new(AtomicUsize::new(0));
    let function_calls = Arc::clone(&calls);
    connection
        .registration()
        .expect("registration")
        .sqlite()
        .register_noarg_sql_function::<diesel::sql_types::Binary, Vec<u8>, _>(
            "uuidv7",
            diesel::sqlite::SqliteFunctionBehavior::empty(),
            move || generated_value(function_calls.fetch_add(1, Ordering::Relaxed) + 1),
        )
        .unwrap();
    // SQLite trigger reentry requires a vendor pragma.
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection).unwrap();
    for statement in translated.into_vec() {
        let statement = statement.to_string();
        assert!(!statement.contains("pg2sqlite_body_frames"));
        // The translated schema contains DDL unavailable through the query DSL.
        diesel::sql_query(statement).execute(&mut connection).unwrap();
    }
    (connection, calls)
}

fn insert_input(connection: &mut Replica, identifier: i32) -> QueryResult<usize> {
    diesel::insert_into(inputs::table).values(inputs::id.eq(identifier)).execute(connection)
}

fn observed(connection: &mut Replica) -> Vec<(i32, i32, Vec<u8>)> {
    observations::table
        .order((observations::event, observations::phase))
        .select((observations::event, observations::phase, observations::token))
        .load(connection)
        .unwrap()
}

fn store_counts(connection: &mut Replica) -> (i64, i64) {
    (
        pg2sqlite_definer_frames::table.count().get_result(connection).unwrap(),
        pg2sqlite_body_values::table.count().get_result(connection).unwrap(),
    )
}

fn assert_empty_store(connection: &mut Replica) {
    assert_eq!(store_counts(connection), (0, 0));
}

fn nested_schema(child_exit: &str) -> String {
    format!(
        "CREATE TABLE inputs (id INTEGER PRIMARY KEY, tag INTEGER NOT NULL DEFAULT 0);
CREATE TABLE candidates (id INTEGER PRIMARY KEY);
CREATE TABLE children (id INTEGER PRIMARY KEY REFERENCES candidates(id));
CREATE TABLE observations (
    event INTEGER NOT NULL,
    phase INTEGER NOT NULL,
    token UUID NOT NULL,
    PRIMARY KEY (event, phase)
);
INSERT INTO candidates VALUES (1), (2), (3);
CREATE FUNCTION child_body() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    INSERT INTO observations VALUES (NEW.id, 2, generated_id);
    {child_exit}
    RETURN NEW;
END $$;
CREATE TRIGGER child_body BEFORE INSERT ON children
FOR EACH ROW EXECUTE FUNCTION child_body();
CREATE FUNCTION parent_body() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    INSERT INTO observations VALUES (NEW.id, 1, generated_id);
    INSERT INTO children VALUES (NEW.id);
    INSERT INTO observations VALUES (NEW.id, 3, generated_id);
    RETURN NEW;
END $$;
CREATE TRIGGER parent_body BEFORE INSERT ON inputs
FOR EACH ROW EXECUTE FUNCTION parent_body();"
    )
}

#[test]
fn nested_triggers_isolate_identical_variable_names() {
    let (mut connection, calls) = replica(&nested_schema(""));
    insert_input(&mut connection, 1).unwrap();
    assert_eq!(
        observed(&mut connection),
        vec![(1, 1, generated_value(1)), (1, 2, generated_value(2)), (1, 3, generated_value(1))]
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_empty_store(&mut connection);
}

#[test]
fn child_cancellation_preserves_the_resuming_callers_value() {
    let (mut connection, calls) =
        replica(&nested_schema("IF generated_id IS NOT NULL THEN RETURN NULL; END IF;"));
    for identifier in 1..=2 {
        insert_input(&mut connection, identifier).unwrap();
        assert_empty_store(&mut connection);
    }
    assert_eq!(children::table.count().get_result::<i64>(&mut connection).unwrap(), 0);
    assert_eq!(inputs::table.count().get_result::<i64>(&mut connection).unwrap(), 2);
    assert_eq!(
        observed(&mut connection),
        vec![
            (1, 1, generated_value(1)),
            (1, 2, generated_value(2)),
            (1, 3, generated_value(1)),
            (2, 1, generated_value(3)),
            (2, 2, generated_value(4)),
            (2, 3, generated_value(3)),
        ]
    );
    assert_eq!(calls.load(Ordering::Relaxed), 4);
}

#[test]
fn invalid_nested_foreign_key_is_checked_before_parent_statement_resumes() {
    let (mut connection, calls) =
        replica(&nested_schema("IF NEW.id = 1 THEN INSERT INTO children VALUES (99); END IF;"));
    let error = insert_input(&mut connection, 1).unwrap_err();
    assert!(error.to_string().to_uppercase().contains("FOREIGN KEY"), "{error}");
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_eq!(observed(&mut connection), []);
    assert_eq!(inputs::table.count().get_result::<i64>(&mut connection).unwrap(), 0);
    assert_eq!(children::table.count().get_result::<i64>(&mut connection).unwrap(), 0);
    assert_empty_store(&mut connection);

    insert_input(&mut connection, 2).unwrap();
    assert_eq!(
        observed(&mut connection),
        vec![(2, 1, generated_value(4)), (2, 2, generated_value(5)), (2, 3, generated_value(4))]
    );
    assert_eq!(children::table.select(children::id).load::<i32>(&mut connection).unwrap(), vec![2]);
    assert_eq!(calls.load(Ordering::Relaxed), 5);
    assert_empty_store(&mut connection);
}

#[test]
fn recursive_trigger_invocations_restore_each_callers_value() {
    let postgres = format!(
        "{TABLES}
CREATE FUNCTION recurse() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    INSERT INTO observations VALUES (NEW.id, 1, generated_id);
    IF NEW.id < 3 THEN INSERT INTO inputs VALUES (NEW.id + 1); END IF;
    INSERT INTO observations VALUES (NEW.id, 2, generated_id);
    RETURN NEW;
END $$;
CREATE TRIGGER recurse AFTER INSERT ON inputs FOR EACH ROW EXECUTE FUNCTION recurse();"
    );
    let (mut connection, calls) = replica(&postgres);
    insert_input(&mut connection, 1).unwrap();
    assert_eq!(
        observed(&mut connection),
        vec![
            (1, 1, generated_value(1)),
            (1, 2, generated_value(1)),
            (2, 1, generated_value(2)),
            (2, 2, generated_value(2)),
            (3, 1, generated_value(3)),
            (3, 2, generated_value(3)),
        ]
    );
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_empty_store(&mut connection);
}

#[test]
fn false_generated_return_guard_keeps_values_live() {
    let postgres = format!(
        "{TABLES}
CREATE FUNCTION keep_row() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    INSERT INTO observations VALUES (NEW.id, 1, generated_id);
    IF generated_id IS NULL THEN RETURN NULL; END IF;
    INSERT INTO observations VALUES (NEW.id, 2, generated_id);
    RETURN NEW;
END $$;
CREATE TRIGGER keep_row BEFORE INSERT ON inputs FOR EACH ROW EXECUTE FUNCTION keep_row();"
    );
    let (mut connection, calls) = replica(&postgres);
    insert_input(&mut connection, 1).unwrap();
    assert_eq!(
        observed(&mut connection),
        vec![(1, 1, generated_value(1)), (1, 2, generated_value(1))]
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(inputs::table.count().get_result::<i64>(&mut connection).unwrap(), 1);
    assert_empty_store(&mut connection);
}

#[test]
fn success_cancellation_and_abort_leave_the_next_invocation_clean() {
    let postgres = format!(
        "{TABLES}
CREATE FUNCTION lifecycle() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    IF NEW.id = 2 AND generated_id IS NOT NULL THEN RETURN NULL; END IF;
    INSERT INTO observations VALUES (NEW.id, 1, generated_id);
    IF NEW.id = 3 THEN RAISE EXCEPTION 'abort invocation'; END IF;
    INSERT INTO observations VALUES (NEW.id, 2, generated_id);
    RETURN NEW;
END $$;
CREATE TRIGGER lifecycle BEFORE INSERT ON inputs FOR EACH ROW EXECUTE FUNCTION lifecycle();"
    );
    let (mut connection, calls) = replica(&postgres);
    insert_input(&mut connection, 1).unwrap();
    assert_empty_store(&mut connection);
    insert_input(&mut connection, 2).unwrap();
    assert_empty_store(&mut connection);
    let error = insert_input(&mut connection, 3).unwrap_err();
    assert!(error.to_string().contains("abort invocation"));
    assert_empty_store(&mut connection);
    insert_input(&mut connection, 4).unwrap();
    assert_empty_store(&mut connection);
    assert_eq!(
        observed(&mut connection),
        vec![
            (1, 1, generated_value(1)),
            (1, 2, generated_value(1)),
            (4, 1, generated_value(4)),
            (4, 2, generated_value(4))
        ]
    );
    assert_eq!(
        inputs::table.select(inputs::id).order(inputs::id).load::<i32>(&mut connection).unwrap(),
        vec![1, 4]
    );
    assert_eq!(calls.load(Ordering::Relaxed), 4);
}

#[test]
fn transaction_and_savepoint_rollbacks_leave_no_generated_residue() {
    let (mut connection, calls) = replica(&nested_schema(""));
    let rolled_back = connection.transaction::<(), diesel::result::Error, _>(|connection| {
        insert_input(connection, 1)?;
        assert_empty_store(connection);
        Err(diesel::result::Error::RollbackTransaction)
    });
    assert!(matches!(rolled_back, Err(diesel::result::Error::RollbackTransaction)));
    assert_eq!(observed(&mut connection), []);
    assert_empty_store(&mut connection);
    connection
        .transaction::<(), diesel::result::Error, _>(|connection| {
            let rolled_back =
                connection.transaction::<(), diesel::result::Error, _>(|connection| {
                    insert_input(connection, 2)?;
                    Err(diesel::result::Error::RollbackTransaction)
                });
            assert!(matches!(rolled_back, Err(diesel::result::Error::RollbackTransaction)));
            assert_empty_store(connection);
            insert_input(connection, 3)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        observed(&mut connection),
        vec![(3, 1, generated_value(5)), (3, 2, generated_value(6)), (3, 3, generated_value(5))]
    );
    assert_empty_store(&mut connection);
    assert_eq!(calls.load(Ordering::Relaxed), 6);
}

#[test]
fn bodies_without_generated_assignments_emit_no_storage() {
    let postgres = format!(
        "{TABLES}
CREATE FUNCTION no_storage() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE unassigned_id UUID; ordinary_number INTEGER;
BEGIN
    ordinary_number := 7;
    IF unassigned_id IS NOT NULL OR ordinary_number <> 7 THEN
        RAISE EXCEPTION 'unexpected local value';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER no_storage BEFORE INSERT ON inputs FOR EACH ROW EXECUTE FUNCTION no_storage();"
    );
    let translated = Pg2Sqlite::default().sql(&postgres).unwrap().translate(&options()).unwrap();
    let emitted =
        translated.into_vec().iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(!emitted.contains("pg2sqlite_body_frames"));
    assert!(!emitted.contains("pg2sqlite_body_values"));
    let (mut connection, calls) = replica(&postgres);
    insert_input(&mut connection, 1).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn generated_storage_names_reject_source_table_collisions() {
    for name in ["pg2sqlite_definer_frames", "pg2sqlite_body_values"] {
        let postgres =
            format!("CREATE TABLE {name} (id INTEGER PRIMARY KEY); {}", nested_schema(""));
        let parsed = Pg2Sqlite::default().sql(&postgres).unwrap();
        let Err(error) = parsed.translate(&options()) else {
            panic!("generated name collision must fail");
        };
        assert!(error.to_string().contains(name));
    }
}

#[test]
fn maintenance_capture_names_preserve_colliding_columns() {
    let postgres = "
CREATE TABLE maintenance_values (
    id INTEGER PRIMARY KEY,
    __pg2sqlite_capture_0 UUID,
    __pg2sqlite_reached_1 UUID,
    copied UUID
);
CREATE FUNCTION maintain_values() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id > 0 THEN
        NEW.__pg2sqlite_capture_0 := uuidv7();
    ELSE
        NEW.__pg2sqlite_capture_0 := uuidv7();
    END IF;
    NEW.__pg2sqlite_reached_1 := NEW.__pg2sqlite_capture_0;
    NEW.copied := NEW.__pg2sqlite_reached_1;
    RETURN NEW;
END $$;
CREATE TRIGGER maintain_values BEFORE INSERT ON maintenance_values
FOR EACH ROW EXECUTE FUNCTION maintain_values();";
    let (mut connection, calls) = replica(postgres);
    for (sequence, identifier) in [1, -1].into_iter().enumerate() {
        diesel::insert_into(maintenance_values::table)
            .values(maintenance_values::id.eq(identifier))
            .execute(&mut connection)
            .unwrap();
        let maintained = maintenance_values::table
            .find(identifier)
            .select((
                maintenance_values::__pg2sqlite_capture_0,
                maintenance_values::__pg2sqlite_reached_1,
                maintenance_values::copied,
            ))
            .get_result::<(Vec<u8>, Vec<u8>, Vec<u8>)>(&mut connection)
            .unwrap();
        let expected = generated_value(sequence + 1);
        assert_eq!(maintained.0, expected);
        assert_eq!(maintained.1, expected);
        assert_eq!(maintained.2, expected);
        assert_eq!(calls.load(Ordering::Relaxed), sequence + 1);
        assert_empty_store(&mut connection);
    }
}

#[test]
fn maintenance_capture_refuses_whole_new_reads_after_instrumentation() {
    let postgres = "
CREATE TABLE inputs (id INTEGER PRIMARY KEY, tag TEXT);
CREATE FUNCTION maintain_row_text() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.tag := NEW::text;
    RETURN NEW;
END $$;
CREATE TRIGGER maintain_row_text BEFORE INSERT ON inputs
FOR EACH ROW EXECUTE FUNCTION maintain_row_text();";
    let parsed = Pg2Sqlite::default().sql(postgres).unwrap();
    let Err(error) = parsed.translate(&options()) else {
        panic!("whole-row maintenance capture must refuse translation");
    };
    assert!(error.to_string().contains("reads NEW after a field of it is assigned"));
}

#[test]
fn generated_values_cover_before_after_and_delete_timing() {
    for timing in ["BEFORE", "AFTER"] {
        for event in ["INSERT", "DELETE"] {
            let row = if event == "DELETE" { "OLD" } else { "NEW" };
            let postgres = format!(
                "{TABLES}
CREATE FUNCTION timed_body() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    INSERT INTO observations VALUES ({row}.id, 1, generated_id);
    INSERT INTO observations VALUES ({row}.id, 2, generated_id);
    RETURN {row};
END $$;
CREATE TRIGGER timed_body {timing} {event} ON inputs FOR EACH ROW EXECUTE FUNCTION timed_body();"
            );
            let (mut connection, calls) = replica(&postgres);
            insert_input(&mut connection, 1).unwrap();
            if event == "DELETE" {
                diesel::delete(inputs::table.filter(inputs::id.eq(1)))
                    .execute(&mut connection)
                    .unwrap();
            }
            assert_eq!(
                observed(&mut connection),
                vec![(1, 1, generated_value(1)), (1, 2, generated_value(1))]
            );
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert_empty_store(&mut connection);
        }
    }
}

#[test]
fn instead_of_trigger_owns_its_generated_values() {
    let postgres = format!(
        "{TABLES}
CREATE VIEW input_view AS SELECT id FROM inputs;
CREATE FUNCTION view_body() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    INSERT INTO observations VALUES (NEW.id, 1, generated_id);
    INSERT INTO inputs VALUES (NEW.id);
    INSERT INTO observations VALUES (NEW.id, 2, generated_id);
    RETURN NEW;
END $$;
CREATE TRIGGER view_body INSTEAD OF INSERT ON input_view FOR EACH ROW EXECUTE FUNCTION view_body();"
    );
    let (mut connection, calls) = replica(&postgres);
    diesel::insert_into(input_view::table)
        .values(input_view::id.eq(1))
        .execute(&mut connection)
        .unwrap();
    assert_eq!(
        observed(&mut connection),
        vec![(1, 1, generated_value(1)), (1, 2, generated_value(1))]
    );
    assert_eq!(inputs::table.count().get_result::<i64>(&mut connection).unwrap(), 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_empty_store(&mut connection);
}

#[test]
fn framed_after_members_keep_separate_generated_slots() {
    let postgres = format!(
        "{TABLES}
CREATE FUNCTION maintain() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN NEW.tag := NEW.tag + 10; RETURN NEW; END $$;
CREATE TRIGGER maintain BEFORE INSERT ON inputs FOR EACH ROW EXECUTE FUNCTION maintain();
CREATE FUNCTION first_member() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    INSERT INTO observations VALUES (NEW.tag, 1, generated_id);
    INSERT INTO observations VALUES (NEW.tag, 2, generated_id);
    RETURN NULL;
END $$;
CREATE FUNCTION second_member() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    INSERT INTO observations VALUES (NEW.tag, 3, generated_id);
    INSERT INTO observations VALUES (NEW.tag, 4, generated_id);
    RETURN NEW;
END $$;
CREATE TRIGGER first_member AFTER INSERT ON inputs FOR EACH ROW EXECUTE FUNCTION first_member();
CREATE TRIGGER second_member AFTER INSERT ON inputs FOR EACH ROW EXECUTE FUNCTION second_member();"
    );
    let (mut connection, calls) = replica(&postgres);
    insert_input(&mut connection, 1).unwrap();
    assert_eq!(
        observed(&mut connection),
        vec![
            (10, 1, generated_value(1)),
            (10, 2, generated_value(1)),
            (10, 3, generated_value(2)),
            (10, 4, generated_value(2)),
        ]
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_empty_store(&mut connection);
}

fn loop_schema(body: &str, helpers: &str) -> String {
    format!(
        "{TABLES}
CREATE TABLE candidates (id INTEGER PRIMARY KEY);
{helpers}
CREATE FUNCTION loop_body() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE candidate_id INTEGER;
BEGIN
    {body}
    RETURN NEW;
END $$;
CREATE TRIGGER loop_body BEFORE INSERT ON inputs FOR EACH ROW EXECUTE FUNCTION loop_body();"
    )
}

fn seed_candidates(connection: &mut Replica) {
    for identifier in 1..=3 {
        diesel::insert_into(candidates::table)
            .values(candidates::id.eq(identifier))
            .execute(connection)
            .unwrap();
    }
}

#[test]
fn query_loop_local_generation_is_shared_by_two_checks_per_iteration() {
    let postgres = loop_schema(
        "FOR candidate_id IN SELECT candidate.id FROM candidates candidate ORDER BY candidate.id LOOP
            DECLARE generated_id UUID; BEGIN
                generated_id := uuidv7();
                IF generated_id IS NULL THEN RAISE EXCEPTION 'missing generated value'; END IF;
                IF generated_id <> generated_id THEN RAISE EXCEPTION 'changed generated value'; END IF;
            END;
        END LOOP;",
        "",
    );
    let (mut connection, calls) = replica(&postgres);
    seed_candidates(&mut connection);
    insert_input(&mut connection, 1).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_empty_store(&mut connection);
    insert_input(&mut connection, 2).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 6);
    assert_empty_store(&mut connection);
}

const PER_ROW_HELPER: &str = "
CREATE FUNCTION check_generated(candidate_id INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    IF generated_id IS NULL THEN RAISE EXCEPTION 'missing generated value'; END IF;
    IF generated_id <> generated_id THEN RAISE EXCEPTION 'changed generated value'; END IF;
END $$;
";

#[test]
fn repeated_perform_from_sites_generate_once_for_each_row() {
    let postgres = loop_schema(
        "PERFORM check_generated(candidate.id) FROM candidates candidate ORDER BY candidate.id;
         PERFORM check_generated(candidate.id) FROM candidates candidate ORDER BY candidate.id;",
        PER_ROW_HELPER,
    );
    let (mut connection, calls) = replica(&postgres);
    seed_candidates(&mut connection);
    insert_input(&mut connection, 1).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 6);
    assert_empty_store(&mut connection);
}

#[test]
fn nested_loop_sites_preserve_outer_iteration_values() {
    let postgres = loop_schema(
        "FOR candidate_id IN SELECT candidate.id FROM candidates candidate ORDER BY candidate.id LOOP
            DECLARE outer_id UUID; inner_candidate INTEGER; BEGIN
                outer_id := uuidv7();
                FOR inner_candidate IN SELECT candidate.id FROM candidates candidate ORDER BY candidate.id LOOP
                    DECLARE inner_id UUID; BEGIN
                        inner_id := uuidv7();
                        IF outer_id IS NULL OR inner_id IS NULL OR outer_id = inner_id THEN
                            RAISE EXCEPTION 'aliased iteration values';
                        END IF;
                        IF outer_id <> outer_id OR inner_id <> inner_id THEN
                            RAISE EXCEPTION 'unstable iteration values';
                        END IF;
                    END;
                END LOOP;
                IF outer_id IS NULL OR outer_id <> outer_id THEN RAISE EXCEPTION 'lost outer value'; END IF;
            END;
        END LOOP;
        PERFORM check_generated(candidate.id) FROM candidates candidate ORDER BY candidate.id;",
        PER_ROW_HELPER,
    );
    let (mut connection, calls) = replica(&postgres);
    seed_candidates(&mut connection);
    insert_input(&mut connection, 1).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 15);
    assert_empty_store(&mut connection);
}

#[test]
fn early_iteration_error_prevents_later_generators() {
    let postgres = loop_schema(
        "FOR candidate_id IN SELECT candidate.id FROM candidates candidate ORDER BY candidate.id LOOP
            DECLARE generated_id UUID; BEGIN
                generated_id := uuidv7();
                IF candidate_id = 1 AND NEW.id = 1 AND generated_id IS NOT NULL THEN
                    RAISE EXCEPTION 'first iteration stopped';
                END IF;
                IF generated_id <> generated_id THEN RAISE EXCEPTION 'changed generated value'; END IF;
            END;
        END LOOP;",
        "",
    );
    let (mut connection, calls) = replica(&postgres);
    seed_candidates(&mut connection);
    let error = insert_input(&mut connection, 1).unwrap_err();
    assert!(error.to_string().contains("first iteration stopped"));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_empty_store(&mut connection);
    insert_input(&mut connection, 2).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 4);
    assert_empty_store(&mut connection);
}

#[test]
fn external_raise_fail_retained_values_cannot_leak_into_next_nested_call() {
    let (mut connection, calls) = replica(&nested_schema(""));
    // RAISE(FAIL) is a SQLite-only partial-statement failure.
    diesel::sql_query(
        "CREATE TRIGGER external_failure BEFORE INSERT ON children
         WHEN NEW.id < 3 BEGIN SELECT RAISE(FAIL, 'external failure'); END",
    )
    .execute(&mut connection)
    .unwrap();
    for identifier in 1..=2 {
        let error = insert_input(&mut connection, identifier).unwrap_err();
        assert!(error.to_string().contains("external failure"));
    }
    let retained_frames = pg2sqlite_definer_frames::table
        .order(pg2sqlite_definer_frames::id)
        .select((pg2sqlite_definer_frames::id, pg2sqlite_definer_frames::owner))
        .load::<(i64, String)>(&mut connection)
        .unwrap();
    let retained_values = pg2sqlite_body_values::table
        .order((
            pg2sqlite_body_values::frame,
            pg2sqlite_body_values::iteration_site,
            pg2sqlite_body_values::iteration,
            pg2sqlite_body_values::slot,
        ))
        .select((
            pg2sqlite_body_values::frame,
            pg2sqlite_body_values::iteration_site,
            pg2sqlite_body_values::iteration,
            pg2sqlite_body_values::slot,
            pg2sqlite_body_values::value,
        ))
        .load::<(i64, i64, i64, i64, Option<Vec<u8>>)>(&mut connection)
        .unwrap();
    assert_ne!(retained_frames, []);
    assert_ne!(retained_values, []);
    let calls_before_success = calls.load(Ordering::Relaxed);
    insert_input(&mut connection, 3).unwrap();
    let successful_rows = observations::table
        .filter(observations::event.eq(3))
        .order(observations::phase)
        .select(observations::token)
        .load::<Vec<u8>>(&mut connection)
        .unwrap();
    assert_eq!(
        successful_rows,
        vec![
            generated_value(calls_before_success + 1),
            generated_value(calls_before_success + 2),
            generated_value(calls_before_success + 1)
        ]
    );
    assert_eq!(
        pg2sqlite_definer_frames::table
            .order(pg2sqlite_definer_frames::id)
            .select((pg2sqlite_definer_frames::id, pg2sqlite_definer_frames::owner))
            .load::<(i64, String)>(&mut connection)
            .unwrap(),
        retained_frames
    );
    assert_eq!(
        pg2sqlite_body_values::table
            .order((
                pg2sqlite_body_values::frame,
                pg2sqlite_body_values::iteration_site,
                pg2sqlite_body_values::iteration,
                pg2sqlite_body_values::slot
            ))
            .select((
                pg2sqlite_body_values::frame,
                pg2sqlite_body_values::iteration_site,
                pg2sqlite_body_values::iteration,
                pg2sqlite_body_values::slot,
                pg2sqlite_body_values::value
            ))
            .load::<(i64, i64, i64, i64, Option<Vec<u8>>)>(&mut connection)
            .unwrap(),
        retained_values
    );
}

#[test]
fn external_or_fail_keeps_retained_slots_isolated_through_success_and_rollback() {
    let postgres = format!(
        "{TABLES}
CREATE FUNCTION partial_failure() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE generated_id UUID;
BEGIN
    generated_id := uuidv7();
    INSERT INTO observations VALUES (NEW.id, 1, generated_id);
    INSERT INTO observations VALUES (NEW.id, 2, generated_id);
    RETURN NEW;
END $$;
CREATE TRIGGER partial_failure BEFORE INSERT ON inputs
FOR EACH ROW EXECUTE FUNCTION partial_failure();"
    );
    let (mut connection, calls) = replica(&postgres);
    for identifier in 1..=2 {
        diesel::insert_into(observations::table)
            .values((
                observations::event.eq(identifier),
                observations::phase.eq(2),
                observations::token.eq(generated_value(99)),
            ))
            .execute(&mut connection)
            .unwrap();
        // OR FAIL retains writes and has no Diesel insert builder.
        let error = diesel::sql_query("INSERT OR FAIL INTO inputs (id) VALUES (?)")
            .bind::<diesel::sql_types::Integer, _>(identifier)
            .execute(&mut connection)
            .unwrap_err();
        assert!(matches!(
            error,
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _
            )
        ));
    }
    let retained_counts = store_counts(&mut connection);
    assert!(retained_counts.0 > 0);
    assert!(retained_counts.1 > 0);
    let retained_values = pg2sqlite_body_values::table
        .order((
            pg2sqlite_body_values::frame,
            pg2sqlite_body_values::iteration_site,
            pg2sqlite_body_values::iteration,
            pg2sqlite_body_values::slot,
        ))
        .select((
            pg2sqlite_body_values::frame,
            pg2sqlite_body_values::iteration_site,
            pg2sqlite_body_values::iteration,
            pg2sqlite_body_values::slot,
            pg2sqlite_body_values::value,
        ))
        .load::<(i64, i64, i64, i64, Option<Vec<u8>>)>(&mut connection)
        .unwrap();
    insert_input(&mut connection, 3).unwrap();
    assert_eq!(store_counts(&mut connection), retained_counts);
    let rolled_back = connection.transaction::<(), diesel::result::Error, _>(|connection| {
        insert_input(connection, 4)?;
        Err(diesel::result::Error::RollbackTransaction)
    });
    assert!(matches!(rolled_back, Err(diesel::result::Error::RollbackTransaction)));
    insert_input(&mut connection, 5).unwrap();
    assert_eq!(store_counts(&mut connection), retained_counts);
    assert_eq!(
        observed(&mut connection),
        vec![
            (1, 1, generated_value(1)),
            (1, 2, generated_value(99)),
            (2, 1, generated_value(2)),
            (2, 2, generated_value(99)),
            (3, 1, generated_value(3)),
            (3, 2, generated_value(3)),
            (5, 1, generated_value(5)),
            (5, 2, generated_value(5)),
        ]
    );
    assert_eq!(calls.load(Ordering::Relaxed), 5);
    assert_eq!(
        pg2sqlite_body_values::table
            .order((
                pg2sqlite_body_values::frame,
                pg2sqlite_body_values::iteration_site,
                pg2sqlite_body_values::iteration,
                pg2sqlite_body_values::slot,
            ))
            .select((
                pg2sqlite_body_values::frame,
                pg2sqlite_body_values::iteration_site,
                pg2sqlite_body_values::iteration,
                pg2sqlite_body_values::slot,
                pg2sqlite_body_values::value,
            ))
            .load::<(i64, i64, i64, i64, Option<Vec<u8>>)>(&mut connection)
            .unwrap(),
        retained_values
    );
}
