extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::Mutex;

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, Placement, UuidRepresentation};
use sql_traits::structs::Deployment;

use crate::helpers::establish_connection;

diesel::table! {
    inputs (id) {
        id -> Integer,
    }
}

diesel::table! {
    outputs (id) {
        id -> Integer,
        token -> Binary,
        note -> Text,
    }
}

diesel::table! {
    trail (id) {
        id -> Integer,
        token -> Binary,
        parent_token -> Binary,
        actor -> Text,
    }
}

diesel::table! {
    observed (id) {
        id -> Integer,
        depth -> Integer,
    }
}

diesel::table! {
    pg2sqlite_definer_frames (id) {
        id -> BigInt,
    }
}

diesel::table! {
    pg2sqlite_body_values (frame, slot) {
        frame -> BigInt,
        slot -> BigInt,
    }
}

mod main_catalogue {
    diesel::table! {
        main.sqlite_master (name) {
            name -> Text,
        }
    }
}

mod attached_catalogue {
    diesel::table! {
        shared.sqlite_master (name) {
            name -> Text,
        }
    }

    diesel::table! {
        shared.outputs_pg2sqlite_shared (id) {
            id -> Integer,
            token -> Binary,
            note -> Text,
        }
    }
}

#[diesel::declare_sql_function]
extern "SQL" {
    fn counted_policy_uuid() -> diesel::sql_types::Binary;
    fn derived_policy(step: diesel::sql_types::Integer) -> diesel::sql_types::Integer;
    fn policy_depth() -> diesel::sql_types::Integer;
    fn policy_write_exempt() -> diesel::sql_types::Bool;
}

const TABLES: &str = "
    CREATE TABLE inputs (id INTEGER PRIMARY KEY);
    CREATE TABLE outputs (id INTEGER PRIMARY KEY, token UUID NOT NULL, note TEXT NOT NULL);
";

fn value(sequence: usize) -> Vec<u8> {
    u128::try_from(sequence).unwrap().to_be_bytes().to_vec()
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("counted_policy_uuid")
}

struct Replica {
    connection: SqliteConnection,
    calls: Arc<AtomicUsize>,
    depth: Arc<AtomicI32>,
    marks: Arc<Mutex<Vec<i32>>>,
    generation_depths: Arc<Mutex<Vec<i32>>>,
}

impl Replica {
    fn new(source: &str, options: &Pg2SqliteOptions, attached: bool) -> Self {
        let script = Pg2Sqlite::default().sql(source).unwrap().translate_to_sql(options).unwrap();
        let mut connection = establish_connection();
        if attached {
            connection.batch_execute("ATTACH ':memory:' AS shared").unwrap();
        }
        script.register_functions(&mut connection).unwrap();
        policy_write_exempt_utils::register_nondeterministic_impl(&mut connection, || false)
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let depth = Arc::new(AtomicI32::new(0));
        let marks = Arc::new(Mutex::new(Vec::new()));
        let generation_depths = Arc::new(Mutex::new(Vec::new()));
        {
            let calls = Arc::clone(&calls);
            let depth = Arc::clone(&depth);
            let generation_depths = Arc::clone(&generation_depths);
            counted_policy_uuid_utils::register_nondeterministic_impl(&mut connection, move || {
                generation_depths.lock().unwrap().push(depth.load(Ordering::Relaxed));
                value(calls.fetch_add(1, Ordering::Relaxed) + 1)
            })
            .unwrap();
        }
        {
            let depth = Arc::clone(&depth);
            let marks = Arc::clone(&marks);
            derived_policy_utils::register_nondeterministic_impl(
                &mut connection,
                move |step: i32| {
                    marks.lock().unwrap().push(step);
                    depth.fetch_add(step, Ordering::Relaxed) + step
                },
            )
            .unwrap();
        }
        {
            let depth = Arc::clone(&depth);
            policy_depth_utils::register_nondeterministic_impl(&mut connection, move || {
                depth.load(Ordering::Relaxed)
            })
            .unwrap();
        }
        for statement in script.into_vec() {
            connection.batch_execute(&statement).unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        Self { connection, calls, depth, marks, generation_depths }
    }

    fn insert(&mut self, identifier: i32) {
        diesel::insert_into(inputs::table)
            .values(inputs::id.eq(identifier))
            .execute(&mut self.connection)
            .unwrap();
    }

    fn outputs(&mut self) -> Vec<(i32, Vec<u8>, String)> {
        outputs::table.order(outputs::id).load(&mut self.connection).unwrap()
    }
}

fn capture_source(body: &str) -> String {
    format!(
        "{TABLES}
        CREATE FUNCTION capture_policy_value() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE generated UUID;
        BEGIN
            {body}
            RETURN NULL;
        END $$;
        CREATE TRIGGER capture_policy_value AFTER INSERT ON inputs
            FOR EACH ROW EXECUTE FUNCTION capture_policy_value();"
    )
}

#[test]
fn definer_and_firing_owner_dispatchers_store_generated_values_with_owner_privileges() {
    let source = format!(
        "CREATE ROLE app;
        CREATE ROLE writer BYPASSRLS;
        {TABLES}
        CREATE TABLE trail (
            id INTEGER PRIMARY KEY, token UUID NOT NULL,
            parent_token UUID NOT NULL, actor TEXT NOT NULL
        );
        ALTER TABLE outputs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY outputs_read ON outputs FOR SELECT USING (true);
        ALTER TABLE trail ENABLE ROW LEVEL SECURITY;
        CREATE POLICY trail_read ON trail FOR SELECT USING (true);
        CREATE FUNCTION derive_output() RETURNS trigger
        LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
        DECLARE generated UUID;
        BEGIN
            generated := uuidv7();
            INSERT INTO public.outputs VALUES (NEW.id, generated, current_user);
            RETURN NULL;
        END $$;
        ALTER FUNCTION derive_output() OWNER TO writer;
        CREATE TRIGGER inputs_derive AFTER INSERT ON inputs
            FOR EACH ROW EXECUTE FUNCTION derive_output();
        CREATE FUNCTION derive_trail() RETURNS trigger
        LANGUAGE plpgsql SET search_path = '' AS $$
        DECLARE generated UUID; selected INTEGER;
        BEGIN
            generated := uuidv7();
            FOR selected IN SELECT id FROM public.inputs WHERE id = NEW.id LOOP
                DECLARE loop_generated UUID := uuidv7(); BEGIN
                    IF loop_generated IS NULL THEN RAISE EXCEPTION 'missing'; END IF;
                END;
            END LOOP;
            INSERT INTO public.trail VALUES (NEW.id, generated, NEW.token, current_user);
            RETURN NULL;
        END $$;
        CREATE TRIGGER outputs_trail AFTER INSERT ON outputs
            FOR EACH ROW EXECUTE FUNCTION derive_trail();
        GRANT SELECT, INSERT ON inputs TO app;
        GRANT SELECT ON outputs, trail TO app;
        GRANT SELECT ON inputs TO writer;
        GRANT INSERT ON outputs, trail TO writer;"
    );
    let options = options()
        .with_deployment(Deployment::new("app", ["app"], []).unwrap())
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("policy_write_exempt");
    let mut replica = Replica::new(&source, &options, false);
    replica.insert(1);
    assert_eq!(
        pg2sqlite_definer_frames::table.count().get_result::<i64>(&mut replica.connection).unwrap(),
        0
    );
    assert_eq!(
        pg2sqlite_body_values::table.count().get_result::<i64>(&mut replica.connection).unwrap(),
        0
    );
    replica.insert(2);
    assert_eq!(
        replica.outputs(),
        [(1, value(1), "writer".to_owned()), (2, value(4), "writer".to_owned())]
    );
    let rows: Vec<(i32, Vec<u8>, Vec<u8>, String)> =
        trail::table.order(trail::id).load(&mut replica.connection).unwrap();
    assert_eq!(
        rows,
        [
            (1, value(2), value(1), "writer".to_owned()),
            (2, value(5), value(4), "writer".to_owned())
        ]
    );
    assert_eq!(replica.calls.load(Ordering::Relaxed), 6);
    assert!(
        diesel::insert_into(outputs::table)
            .values((outputs::id.eq(3), outputs::token.eq(value(9)), outputs::note.eq("app")))
            .execute(&mut replica.connection)
            .is_err()
    );
    assert_eq!(replica.calls.load(Ordering::Relaxed), 6);
    assert_eq!(
        pg2sqlite_definer_frames::table.count().get_result::<i64>(&mut replica.connection).unwrap(),
        0
    );
    assert_eq!(
        pg2sqlite_body_values::table.count().get_result::<i64>(&mut replica.connection).unwrap(),
        0
    );
    assert_eq!(
        main_catalogue::sqlite_master::table
            .filter(main_catalogue::sqlite_master::name.eq("pg2sqlite_body_frames"))
            .count()
            .get_result::<i64>(&mut replica.connection)
            .unwrap(),
        0
    );
}

#[test]
fn internal_generated_storage_does_not_add_derived_write_marks() {
    let source = capture_source(
        "generated := uuidv7();
        INSERT INTO outputs VALUES (NEW.id, generated, 'derived');
        generated := uuidv7();",
    );
    let mut replica = Replica::new(
        &source,
        &options().with_server_derived_write_function("derived_policy"),
        false,
    );
    replica
        .connection
        .batch_execute(
            "CREATE TABLE observed (id INTEGER PRIMARY KEY, depth INTEGER NOT NULL);
        CREATE TRIGGER outputs_observed AFTER INSERT ON outputs BEGIN
            INSERT INTO observed VALUES (NEW.id, policy_depth());
        END;",
        )
        .unwrap();
    replica.insert(1);
    assert_eq!(replica.outputs(), [(1, value(1), "derived".to_owned())]);
    assert_eq!(replica.calls.load(Ordering::Relaxed), 2);
    assert_eq!(*replica.marks.lock().unwrap(), [1, -1]);
    assert_eq!(*replica.generation_depths.lock().unwrap(), [0, 0]);
    assert_eq!(replica.depth.load(Ordering::Relaxed), 0);
    diesel::insert_into(outputs::table)
        .values((outputs::id.eq(2), outputs::token.eq(value(3)), outputs::note.eq("application")))
        .execute(&mut replica.connection)
        .unwrap();
    let rows: Vec<(i32, i32)> =
        observed::table.order(observed::id).load(&mut replica.connection).unwrap();
    assert_eq!(rows, [(1, 1), (2, 0)]);
    assert_eq!(*replica.marks.lock().unwrap(), [1, -1]);
}

#[test]
fn generated_assignment_preserves_true_and_false_found() {
    let source = capture_source(
        "PERFORM 1 FROM inputs WHERE id = NEW.id AND id = 1;
        generated := uuidv7();
        IF FOUND THEN
            INSERT INTO outputs VALUES (NEW.id, generated, 'found');
        ELSE
            INSERT INTO outputs VALUES (NEW.id, generated, 'missing');
        END IF;",
    );
    let mut replica = Replica::new(&source, &options(), false);
    replica.insert(1);
    replica.insert(2);
    assert_eq!(
        replica.outputs(),
        [(1, value(1), "found".to_owned()), (2, value(2), "missing".to_owned())]
    );
    assert_eq!(replica.calls.load(Ordering::Relaxed), 2);
}

#[test]
fn attached_generated_body_storage_stays_in_main_and_out_of_the_manifest() {
    let source = capture_source(
        "generated := uuidv7();
        INSERT INTO outputs VALUES (NEW.id, generated, 'derived');",
    );
    let options = options()
        .with_server_derived_write_function("derived_policy")
        .with_attached_tables("shared", ["outputs"]);
    let manifest =
        Pg2Sqlite::default().sql(&source).unwrap().translation_manifest(&options).unwrap();
    let mut logical_names: Vec<&str> =
        manifest.iter().map(|entry| entry.logical.as_str()).collect();
    logical_names.sort_unstable();
    assert_eq!(logical_names, ["inputs", "outputs"]);
    let output = manifest.iter().find(|entry| entry.logical == "outputs").unwrap();
    assert_eq!(output.logical_placement, Placement::Temp);
    assert_eq!(output.physical_placement, Placement::Attached);
    assert_eq!(output.overlay.as_deref(), Some("outputs_pg2sqlite_overlay"));

    let mut replica = Replica::new(&source, &options, true);
    let storage_names = ["pg2sqlite_body_values", "pg2sqlite_definer_frames"];
    let stored_in_main: Vec<String> = main_catalogue::sqlite_master::table
        .select(main_catalogue::sqlite_master::name)
        .filter(main_catalogue::sqlite_master::name.eq_any(storage_names))
        .order(main_catalogue::sqlite_master::name)
        .load(&mut replica.connection)
        .unwrap();
    assert_eq!(stored_in_main, storage_names);
    let stored_in_attached: Vec<String> = attached_catalogue::sqlite_master::table
        .select(attached_catalogue::sqlite_master::name)
        .filter(attached_catalogue::sqlite_master::name.eq_any(storage_names))
        .load(&mut replica.connection)
        .unwrap();
    assert_eq!(stored_in_attached, Vec::<String>::new());
    assert_eq!(
        main_catalogue::sqlite_master::table
            .filter(main_catalogue::sqlite_master::name.eq("pg2sqlite_body_frames"))
            .count()
            .get_result::<i64>(&mut replica.connection)
            .unwrap(),
        0
    );
    replica.insert(1);
    replica.insert(2);
    assert_eq!(
        replica.outputs(),
        [(1, value(1), "derived".to_owned()), (2, value(2), "derived".to_owned())]
    );
    let shared_count: i64 = attached_catalogue::outputs_pg2sqlite_shared::table
        .count()
        .get_result(&mut replica.connection)
        .unwrap();
    assert_eq!(shared_count, 0);
    assert_eq!(replica.calls.load(Ordering::Relaxed), 2);
    assert_eq!(replica.depth.load(Ordering::Relaxed), 0);
}

#[test]
fn generated_helper_arguments_are_evaluated_once_and_survive_return() {
    for (assignment, argument) in [("", "uuidv7()"), ("generated := uuidv7();", "generated")] {
        let source = format!(
            "{}
            CREATE FUNCTION store_generated_argument(identifier INTEGER, token_value UUID)
            RETURNS UUID LANGUAGE plpgsql AS $$
            BEGIN
                INSERT INTO outputs VALUES (identifier * 10, token_value, 'first');
                INSERT INTO outputs VALUES (identifier * 10 + 1, token_value, 'second');
                RETURN token_value;
            END $$;",
            capture_source(&format!(
                "{assignment}
                generated := store_generated_argument(NEW.id, {argument});
                INSERT INTO outputs VALUES (NEW.id * 10 + 2, generated, 'returned');"
            ))
        );
        let mut replica = Replica::new(&source, &options(), false);
        replica.insert(1);
        replica.insert(2);
        assert_eq!(
            replica.outputs(),
            [
                (10, value(1), "first".to_owned()),
                (11, value(1), "second".to_owned()),
                (12, value(1), "returned".to_owned()),
                (20, value(2), "first".to_owned()),
                (21, value(2), "second".to_owned()),
                (22, value(2), "returned".to_owned()),
            ],
            "{argument}"
        );
        assert_eq!(replica.calls.load(Ordering::Relaxed), 2, "{argument}");
    }
}

#[test]
fn helper_unnamed_parameter_is_refused() {
    let source = format!(
        "{}
        CREATE FUNCTION unnamed_argument(INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
        BEGIN RETURN; END $$;",
        capture_source("PERFORM unnamed_argument(1);")
    );
    let error =
        Pg2Sqlite::default().sql(&source).unwrap().translate_to_sql(&options()).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("unnamed_argument"), "{message}");
    assert!(message.contains("declares a parameter only $n reaches"), "{message}");
}

#[test]
fn helper_implicit_output_parameter_is_refused() {
    let source = format!(
        "{}
        CREATE FUNCTION implicit_output(OUT token_value UUID) LANGUAGE plpgsql AS $$
        BEGIN token_value := uuidv7(); RETURN; END $$;",
        capture_source("PERFORM implicit_output();")
    );
    let error =
        Pg2Sqlite::default().sql(&source).unwrap().translate_to_sql(&options()).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("implicit_output"), "{message}");
    assert!(message.contains("only IN parameters bind"), "{message}");
}

#[test]
fn generated_before_trigger_cancellation_balances_derived_marks() {
    let source = format!(
        "{TABLES}
        CREATE FUNCTION conditional_policy_value() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE generated UUID;
        BEGIN
            generated := uuidv7();
            IF NEW.id = 2 AND generated IS NOT NULL THEN RETURN NULL; END IF;
            INSERT INTO outputs VALUES (NEW.id, generated, 'derived');
            RETURN NEW;
        END $$;
        CREATE TRIGGER conditional_policy_value BEFORE INSERT OR UPDATE ON inputs
            FOR EACH ROW EXECUTE FUNCTION conditional_policy_value();"
    );
    let mut replica = Replica::new(
        &source,
        &options().with_server_derived_write_function("derived_policy"),
        false,
    );
    for identifier in [1, 2, 3] {
        if identifier == 1 {
            replica.insert(identifier);
        } else {
            diesel::update(inputs::table.filter(inputs::id.eq(1)))
                .set(inputs::id.eq(identifier))
                .execute(&mut replica.connection)
                .unwrap();
        }
        assert_eq!(replica.depth.load(Ordering::Relaxed), 0);
        assert_eq!(
            pg2sqlite_body_values::table
                .count()
                .get_result::<i64>(&mut replica.connection)
                .unwrap(),
            0
        );
        assert_eq!(
            pg2sqlite_definer_frames::table
                .count()
                .get_result::<i64>(&mut replica.connection)
                .unwrap(),
            0
        );
    }
    assert_eq!(
        replica.outputs(),
        [(1, value(1), "derived".to_owned()), (3, value(3), "derived".to_owned())]
    );
    assert_eq!(
        inputs::table
            .select(inputs::id)
            .order(inputs::id)
            .load::<i32>(&mut replica.connection)
            .unwrap(),
        [3]
    );
    assert_eq!(replica.calls.load(Ordering::Relaxed), 3);
    assert_eq!(*replica.generation_depths.lock().unwrap(), [0, 0, 0]);
    let marks = replica.marks.lock().unwrap().clone();
    assert_ne!(marks, [0_i32; 0]);
    assert_eq!(marks.iter().sum::<i32>(), 0);
    diesel::insert_into(outputs::table)
        .values((outputs::id.eq(4), outputs::token.eq(value(4)), outputs::note.eq("application")))
        .execute(&mut replica.connection)
        .unwrap();
    assert_eq!(
        replica.outputs(),
        [
            (1, value(1), "derived".to_owned()),
            (3, value(3), "derived".to_owned()),
            (4, value(4), "application".to_owned()),
        ]
    );
    assert_eq!(*replica.marks.lock().unwrap(), marks);
    assert_eq!(replica.depth.load(Ordering::Relaxed), 0);
}

#[test]
fn guarded_cancellation_preserves_completed_reads_and_writes() {
    for (perform, expected_calls) in [
        ("PERFORM 1;", 1),
        ("PERFORM id FROM outputs WHERE id = NEW.id;", 1),
        ("PERFORM coalesce(NULL, 3);", 1),
        ("PERFORM abs(-1);", 1),
        ("PERFORM uuidv7() FROM outputs WHERE id = NEW.id;", 2),
    ] {
        let source = format!(
            "{TABLES}
            CREATE FUNCTION cancel_input() RETURNS trigger LANGUAGE plpgsql AS $$
            DECLARE generated UUID;
            BEGIN
                generated := uuidv7();
                INSERT INTO outputs VALUES (NEW.id, generated, 'before');
                {perform}
                RETURN NULL;
                INSERT INTO outputs VALUES (NEW.id + 1, generated, 'after');
            END $$;
            CREATE TRIGGER cancel_input BEFORE INSERT ON inputs
                FOR EACH ROW WHEN (NEW.id > 0) EXECUTE FUNCTION cancel_input();
            CREATE FUNCTION check_input() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                IF NEW.id < 0 THEN RAISE EXCEPTION 'negative'; END IF;
                RETURN NEW;
            END $$;
            CREATE TRIGGER check_input BEFORE INSERT ON inputs
                FOR EACH ROW EXECUTE FUNCTION check_input();"
        );
        let mut replica = Replica::new(
            &source,
            &options().with_server_derived_write_function("derived_policy"),
            false,
        );
        replica.insert(1);
        assert_eq!(replica.outputs(), [(1, value(1), "before".to_owned())], "{perform}");
        assert_eq!(replica.calls.load(Ordering::Relaxed), expected_calls, "{perform}");
        assert_eq!(inputs::table.count().get_result::<i64>(&mut replica.connection).unwrap(), 0);
        assert_eq!(
            pg2sqlite_body_values::table
                .count()
                .get_result::<i64>(&mut replica.connection)
                .unwrap(),
            0
        );
        assert_eq!(
            pg2sqlite_definer_frames::table
                .count()
                .get_result::<i64>(&mut replica.connection)
                .unwrap(),
            0
        );
        assert_eq!(replica.depth.load(Ordering::Relaxed), 0);
        assert_eq!(replica.marks.lock().unwrap().iter().sum::<i32>(), 0);
    }
}
