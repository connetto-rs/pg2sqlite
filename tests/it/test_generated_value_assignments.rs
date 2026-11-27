use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};

use crate::helpers::{Replica, deployment::deployment, establish_connection};

diesel::table! {
    inputs (id) {
        id -> Integer,
    }
}

diesel::table! {
    audit (id) {
        id -> Integer,
    }
}

diesel::table! {
    entities (id) {
        id -> Binary,
    }
}

diesel::table! {
    ownables (id) {
        id -> Binary,
    }
}

diesel::table! {
    observations (id) {
        id -> Integer,
        first_value -> Nullable<Binary>,
        second_value -> Nullable<Binary>,
        note -> Nullable<Text>,
    }
}

diesel::table! {
    items (id) {
        id -> Integer,
        token -> Nullable<Binary>,
        copy -> Nullable<Binary>,
    }
}

mod text_schema {
    diesel::table! {
        observations (id) {
            id -> Integer,
            first_value -> Nullable<Text>,
            second_value -> Nullable<Text>,
            note -> Nullable<Text>,
        }
    }
}

#[diesel::declare_sql_function]
extern "SQL" {
    fn counted_uuid_blob() -> diesel::sql_types::Binary;
    fn counted_uuid_text() -> diesel::sql_types::Text;
}

const SCHEMA: &str = "
    CREATE TABLE inputs (id INTEGER PRIMARY KEY);
    CREATE TABLE audit (id SERIAL PRIMARY KEY, note TEXT);
    CREATE TABLE entities (id UUID PRIMARY KEY);
    CREATE TABLE ownables (id UUID PRIMARY KEY REFERENCES entities(id));
    CREATE TABLE observations (
        id INTEGER PRIMARY KEY,
        first_value UUID,
        second_value UUID,
        note TEXT
    );
";

fn binary_value(sequence: usize) -> Vec<u8> {
    u128::try_from(sequence).unwrap().to_be_bytes().to_vec()
}

fn text_value(sequence: usize) -> String {
    format!("00000000-0000-0000-0000-{sequence:012x}")
}

fn options(representation: UuidRepresentation) -> Pg2SqliteOptions {
    let function = match representation {
        UuidRepresentation::Blob => "counted_uuid_blob",
        UuidRepresentation::Text => "counted_uuid_text",
    };
    Pg2SqliteOptions::default()
        .with_uuid_representation(representation)
        .with_uuid_function_name(function)
        .with_uuid_v7_function_name(function)
}

fn replica(
    declarations: &str,
    body: &str,
    helpers: &str,
    representation: UuidRepresentation,
) -> (Replica, Arc<AtomicUsize>) {
    let source = format!(
        "{SCHEMA}
        {helpers}
        CREATE FUNCTION capture_values() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE {declarations}
        BEGIN
            {body}
            RETURN NEW;
        END $$;
        CREATE TRIGGER capture_values AFTER INSERT ON inputs
        FOR EACH ROW EXECUTE FUNCTION capture_values();"
    );
    let translated = Pg2Sqlite::default()
        .sql(&source)
        .unwrap()
        .translate_to_sql(&options(representation))
        .unwrap();
    let mut connection = establish_connection();
    translated.register_managed(&mut connection).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let blob_calls = Arc::clone(&calls);
    counted_uuid_blob_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        move || binary_value(blob_calls.fetch_add(1, Ordering::Relaxed) + 1),
    )
    .unwrap();
    let text_calls = Arc::clone(&calls);
    counted_uuid_text_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        move || text_value(text_calls.fetch_add(1, Ordering::Relaxed) + 1),
    )
    .unwrap();
    for statement in translated.into_vec() {
        connection.batch_execute(&statement).unwrap();
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    (connection, calls)
}

fn insert_input(connection: &mut Replica, identifier: i32) {
    diesel::insert_into(inputs::table)
        .values(inputs::id.eq(identifier))
        .execute(connection)
        .unwrap();
}

type ObservedPair = (Option<Vec<u8>>, Option<Vec<u8>>);

fn observed_values(connection: &mut Replica) -> Vec<ObservedPair> {
    observations::table
        .order(observations::id)
        .select((observations::first_value, observations::second_value))
        .load(connection)
        .unwrap()
}

fn assert_audit_intervention(intervention: &str, preseeded: bool, inserted: bool) {
    let body = format!(
        "generated_id := uuidv7();
        INSERT INTO entities (id) VALUES (generated_id);
        {intervention}
        INSERT INTO ownables (id) VALUES (generated_id);"
    );
    let (mut connection, calls) =
        replica("generated_id UUID;", &body, "", UuidRepresentation::Blob);
    if preseeded {
        diesel::insert_into(audit::table)
            .values(audit::id.eq(100))
            .execute(&mut connection)
            .unwrap();
    }
    insert_input(&mut connection, 1);
    let entity_values =
        entities::table.select(entities::id).load::<Vec<u8>>(&mut connection).unwrap();
    let ownable_values =
        ownables::table.select(ownables::id).load::<Vec<u8>>(&mut connection).unwrap();
    assert_eq!(entity_values, vec![binary_value(1)]);
    assert_eq!(ownable_values, entity_values);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        audit::table.count().get_result::<i64>(&mut connection).unwrap(),
        i64::from(preseeded) + i64::from(inserted)
    );
}

#[test]
fn generated_value_survives_unconditional_audit_insert_empty() {
    assert_audit_intervention("INSERT INTO audit (note) VALUES ('x');", false, true);
}

#[test]
fn generated_value_survives_unconditional_audit_insert_preseeded() {
    assert_audit_intervention("INSERT INTO audit (note) VALUES ('x');", true, true);
}

#[test]
fn generated_value_survives_taken_audit_insert_empty() {
    assert_audit_intervention(
        "IF NEW.id > 0 THEN INSERT INTO audit (note) VALUES ('x'); END IF;",
        false,
        true,
    );
}

#[test]
fn generated_value_survives_taken_audit_insert_preseeded() {
    assert_audit_intervention(
        "IF NEW.id > 0 THEN INSERT INTO audit (note) VALUES ('x'); END IF;",
        true,
        true,
    );
}

#[test]
fn generated_value_survives_untaken_audit_insert_empty() {
    assert_audit_intervention(
        "IF NEW.id < 0 THEN INSERT INTO audit (note) VALUES ('x'); END IF;",
        false,
        false,
    );
}

#[test]
fn generated_value_survives_untaken_audit_insert_preseeded() {
    assert_audit_intervention(
        "IF NEW.id < 0 THEN INSERT INTO audit (note) VALUES ('x'); END IF;",
        true,
        false,
    );
}

fn assert_generator_spellings(representation: UuidRepresentation) {
    for generator in [
        "uuidv7",
        "uuidv4",
        "uuid_generate_v4",
        "gen_random_uuid",
        "public.uuidv7",
        "public.uuidv4",
        "public.uuid_generate_v4",
        "public.gen_random_uuid",
        "UUIDV7",
        "public.UUID_GENERATE_V4",
    ] {
        let body = format!(
            "generated_id := {generator}();
            INSERT INTO observations (id, first_value, second_value)
            VALUES (NEW.id, generated_id, generated_id);"
        );
        let (mut connection, calls) = replica("generated_id UUID;", &body, "", representation);
        insert_input(&mut connection, 1);
        insert_input(&mut connection, 2);
        assert_eq!(calls.load(Ordering::Relaxed), 2, "{generator}");
        match representation {
            UuidRepresentation::Blob => {
                assert_eq!(
                    observed_values(&mut connection),
                    vec![
                        (Some(binary_value(1)), Some(binary_value(1))),
                        (Some(binary_value(2)), Some(binary_value(2))),
                    ],
                    "{generator}"
                );
            }
            UuidRepresentation::Text => {
                let rows = text_schema::observations::table
                    .order(text_schema::observations::id)
                    .select((
                        text_schema::observations::first_value,
                        text_schema::observations::second_value,
                    ))
                    .load::<(Option<String>, Option<String>)>(&mut connection)
                    .unwrap();
                assert_eq!(
                    rows,
                    vec![
                        (Some(text_value(1)), Some(text_value(1))),
                        (Some(text_value(2)), Some(text_value(2))),
                    ],
                    "{generator}"
                );
            }
        }
    }
}

#[test]
fn supported_generators_capture_blob_values_once() {
    assert_generator_spellings(UuidRepresentation::Blob);
}

#[test]
fn supported_generators_capture_text_values_once() {
    assert_generator_spellings(UuidRepresentation::Text);
}

#[test]
fn untaken_assignment_does_not_generate_and_taken_assignment_generates_once() {
    let (mut connection, calls) = replica(
        "generated_id UUID;",
        "
        IF NEW.id > 0 THEN generated_id := uuidv7(); END IF;
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id, generated_id, generated_id);
    ",
        "",
        UuidRepresentation::Blob,
    );
    insert_input(&mut connection, -1);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(observed_values(&mut connection), vec![(None, None)]);
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        observed_values(&mut connection),
        vec![(None, None), (Some(binary_value(1)), Some(binary_value(1)))]
    );
}

#[test]
fn unused_assignment_is_eager() {
    let (mut connection, calls) = replica(
        "generated_id UUID;",
        "
        generated_id := uuidv7();
        INSERT INTO audit (id) VALUES (NEW.id);
    ",
        "",
        UuidRepresentation::Blob,
    );
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(audit::table.select(audit::id).load::<i32>(&mut connection).unwrap(), vec![1]);
}

#[test]
fn unused_assignment_runs_before_a_later_exception() {
    let (mut connection, calls) = replica(
        "generated_id UUID;",
        "
        generated_id := uuidv7();
        RAISE EXCEPTION 'after assignment';
    ",
        "",
        UuidRepresentation::Blob,
    );
    let error = diesel::insert_into(inputs::table)
        .values(inputs::id.eq(1))
        .execute(&mut connection)
        .unwrap_err();
    assert!(error.to_string().contains("after assignment"), "{error}");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn reassignment_preserves_a_copy_of_the_prior_value() {
    let (mut connection, calls) = replica(
        "generated_id UUID; copied_id UUID;",
        "
        generated_id := uuidv7();
        copied_id := generated_id;
        generated_id := uuidv7();
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id, copied_id, generated_id);
    ",
        "",
        UuidRepresentation::Blob,
    );
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        observed_values(&mut connection),
        vec![(Some(binary_value(1)), Some(binary_value(2)))]
    );
}

#[test]
fn deterministic_reassignment_preserves_a_generated_copy() {
    let (mut connection, calls) = replica(
        "generated_id UUID; copied_id UUID;",
        "
        generated_id := uuidv7();
        copied_id := generated_id;
        generated_id := NULL;
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id, copied_id, generated_id);
    ",
        "",
        UuidRepresentation::Blob,
    );
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(observed_values(&mut connection), vec![(Some(binary_value(1)), None)]);
}

#[test]
fn declaration_defaults_execute_once_in_declaration_order() {
    let (mut connection, calls) = replica(
        "first_id UUID DEFAULT uuidv7(); second_id UUID := uuidv7(); unused_id UUID DEFAULT uuidv7();",
        "
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id, first_id, second_id);
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id + 1, first_id, second_id);
    ",
        "",
        UuidRepresentation::Blob,
    );
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_eq!(
        observed_values(&mut connection),
        vec![
            (Some(binary_value(1)), Some(binary_value(2))),
            (Some(binary_value(1)), Some(binary_value(2))),
        ]
    );
}

#[test]
fn select_into_captures_all_targets_from_one_source_evaluation() {
    let (mut connection, calls) = replica(
        "first_id UUID; second_id UUID;",
        "
        SELECT uuidv7(), uuidv7() INTO first_id, second_id FROM audit
        WHERE id = NEW.id AND uuidv7() IS NOT NULL;
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id, first_id, second_id);
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id + 1, first_id, second_id);
    ",
        "",
        UuidRepresentation::Blob,
    );
    diesel::insert_into(audit::table).values(audit::id.eq(1)).execute(&mut connection).unwrap();
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_eq!(
        observed_values(&mut connection),
        vec![
            (Some(binary_value(2)), Some(binary_value(3))),
            (Some(binary_value(2)), Some(binary_value(3))),
        ]
    );
}

#[test]
fn generated_select_preserves_projection_alias_ordering() {
    let (mut connection, calls) = replica(
        "selected_id UUID;",
        "
        SELECT uuidv7() AS token INTO selected_id FROM audit ORDER BY token LIMIT 1;
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id, selected_id, selected_id);
        ",
        "",
        UuidRepresentation::Blob,
    );
    diesel::insert_into(audit::table).values(audit::id.eq(1)).execute(&mut connection).unwrap();
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        observed_values(&mut connection),
        vec![(Some(binary_value(1)), Some(binary_value(1)))]
    );
}

#[test]
fn generated_select_captures_a_grouping_sets_result() {
    let (mut connection, calls) = replica(
        "selected_id UUID; selected_count BIGINT;",
        "
        SELECT min(uuidv7()), count(*) INTO selected_id, selected_count
        FROM audit GROUP BY GROUPING SETS ((id), ());
        INSERT INTO observations (id, first_value, second_value, note)
        VALUES (NEW.id, selected_id, selected_id, selected_count::text);
        ",
        "",
        UuidRepresentation::Blob,
    );
    diesel::insert_into(audit::table).values(audit::id.eq(1)).execute(&mut connection).unwrap();
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        observed_values(&mut connection),
        vec![(Some(binary_value(1)), Some(binary_value(1)))]
    );
    let note: Option<String> =
        observations::table.select(observations::note).first(&mut connection).unwrap();
    assert_eq!(note.as_deref(), Some("1"));
}

#[test]
fn select_into_without_a_row_keeps_null_targets_and_skips_generation() {
    let (mut connection, calls) = replica(
        "first_id UUID; second_id UUID;",
        "
        SELECT uuidv7(), uuidv7() INTO first_id, second_id FROM audit WHERE id = NEW.id;
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id, first_id, second_id);
    ",
        "",
        UuidRepresentation::Blob,
    );
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(observed_values(&mut connection), vec![(None, None)]);
}

#[test]
fn conditional_generated_select_preserves_found_and_skips_untaken_reads() {
    let (mut connection, calls) = replica(
        "first_id UUID; second_id UUID;",
        "
        IF NEW.id > 1 THEN
            SELECT uuidv7() AS generated, uuidv7() INTO first_id, second_id
            FROM audit WHERE id = NEW.id ORDER BY id LIMIT 1;
            IF FOUND THEN
                INSERT INTO observations VALUES (NEW.id, first_id, second_id, 'found');
            ELSE
                INSERT INTO observations VALUES (NEW.id, first_id, second_id, 'missing');
            END IF;
        END IF;
        ",
        "",
        UuidRepresentation::Blob,
    );
    diesel::insert_into(audit::table).values(audit::id.eq(2)).execute(&mut connection).unwrap();
    for identifier in [1, 2, 3] {
        insert_input(&mut connection, identifier);
    }
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        observed_values(&mut connection),
        [(Some(binary_value(1)), Some(binary_value(2))), (None, None)]
    );
    let notes: Vec<Option<String>> = observations::table
        .order(observations::id)
        .select(observations::note)
        .load(&mut connection)
        .unwrap();
    assert_eq!(notes, [Some("found".to_owned()), Some("missing".to_owned())]);
}

#[test]
fn insert_select_reuses_the_assignment_for_every_output_row() {
    let (mut connection, calls) = replica(
        "generated_id UUID;",
        "
        generated_id := uuidv7();
        INSERT INTO observations (id, first_value, second_value)
        SELECT id, generated_id, generated_id FROM audit;
    ",
        "",
        UuidRepresentation::Blob,
    );
    diesel::insert_into(audit::table)
        .values(vec![audit::id.eq(1), audit::id.eq(2)])
        .execute(&mut connection)
        .unwrap();
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        observed_values(&mut connection),
        vec![
            (Some(binary_value(1)), Some(binary_value(1))),
            (Some(binary_value(1)), Some(binary_value(1))),
        ]
    );
}

#[test]
fn update_set_and_where_read_the_captured_value() {
    let (mut connection, calls) = replica(
        "generated_id UUID;",
        "
        generated_id := uuidv7();
        INSERT INTO observations (id, first_value) VALUES (NEW.id, generated_id);
        INSERT INTO audit (id) VALUES (NEW.id);
        UPDATE observations SET second_value = generated_id, note = 'matched'
        WHERE first_value = generated_id;
    ",
        "",
        UuidRepresentation::Blob,
    );
    diesel::insert_into(observations::table)
        .values((observations::id.eq(99), observations::first_value.eq(binary_value(99))))
        .execute(&mut connection)
        .unwrap();
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        observed_values(&mut connection),
        vec![(Some(binary_value(1)), Some(binary_value(1))), (Some(binary_value(99)), None),]
    );
    let notes = observations::table
        .order(observations::id)
        .select(observations::note)
        .load::<Option<String>>(&mut connection)
        .unwrap();
    assert_eq!(notes, vec![Some("matched".into()), None]);
}

#[test]
fn function_arguments_read_the_captured_value() {
    let (mut connection, calls) = replica(
        "generated_id UUID;",
        "
        generated_id := uuidv7();
        INSERT INTO observations (id, first_value, second_value, note)
        VALUES (NEW.id, generated_id, generated_id, replace(generated_id::text, '-', ''));
    ",
        "",
        UuidRepresentation::Text,
    );
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let values = text_schema::observations::table
        .select((
            text_schema::observations::first_value,
            text_schema::observations::second_value,
            text_schema::observations::note,
        ))
        .load::<(Option<String>, Option<String>, Option<String>)>(&mut connection)
        .unwrap();
    assert_eq!(
        values,
        vec![(Some(text_value(1)), Some(text_value(1)), Some(text_value(1).replace('-', "")))]
    );
}

#[test]
fn exception_message_reuses_the_captured_value() {
    let (mut connection, calls) = replica(
        "generated_id UUID;",
        "
        generated_id := uuidv7();
        RAISE EXCEPTION 'generated % and %', generated_id, generated_id;
    ",
        "",
        UuidRepresentation::Text,
    );
    let error = diesel::insert_into(inputs::table)
        .values(inputs::id.eq(1))
        .execute(&mut connection)
        .unwrap_err();
    let expected = format!("generated {} and {}", text_value(1), text_value(1));
    assert!(error.to_string().contains(&expected), "{error}");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(inputs::table.count().get_result::<i64>(&mut connection).unwrap(), 0);
}

#[test]
fn two_calls_to_one_scalar_helper_keep_distinct_returned_values() {
    let (mut connection, calls) = replica(
        "first_id UUID; second_id UUID;",
        "
        first_id := make_generated_id();
        second_id := make_generated_id();
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id, first_id, second_id);
        INSERT INTO audit (id) VALUES (NEW.id);
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id + 1, first_id, second_id);
    ",
        "
        CREATE FUNCTION make_generated_id() RETURNS UUID LANGUAGE plpgsql AS $$
        DECLARE generated_id UUID;
        BEGIN
            generated_id := uuidv7();
            RETURN generated_id;
        END $$;
    ",
        UuidRepresentation::Blob,
    );
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        observed_values(&mut connection),
        vec![
            (Some(binary_value(1)), Some(binary_value(2))),
            (Some(binary_value(1)), Some(binary_value(2))),
        ]
    );
}

const STANDALONE_FUNCTION: &str = "
    CREATE FUNCTION generated_pair() RETURNS TEXT LANGUAGE plpgsql AS $$
    DECLARE generated_id UUID;
    BEGIN
        generated_id := uuidv7();
        RETURN generated_id::text || generated_id::text;
    END $$;
";

#[test]
fn standalone_generated_scalar_function_is_refused() {
    let source = format!("{STANDALONE_FUNCTION} SELECT generated_pair();");
    let error = Pg2Sqlite::default()
        .sql(&source)
        .unwrap()
        .translate(&options(UuidRepresentation::Text))
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("generated_pair"), "{message}");
    assert!(message.contains("not a SQLite function"), "{message}");
}

#[test]
fn deployed_standalone_generated_table_function_is_refused() {
    let source =
        format!("CREATE ROLE postgres; {STANDALONE_FUNCTION} SELECT * FROM generated_pair();");
    let configured = options(UuidRepresentation::Text)
        .with_deployment(deployment(["postgres"], []))
        .with_acting_role("postgres");
    let error = Pg2Sqlite::default().sql(&source).unwrap().translate(&configured).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("generated_pair"), "{message}");
    assert!(message.contains("cannot be translated as the set it returns"), "{message}");
}

#[test]
fn local_generated_variable_assigned_to_new_remains_refused() {
    let source = "
        CREATE TABLE items (id UUID PRIMARY KEY);
        CREATE FUNCTION assign_generated_id() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE generated_id UUID;
        BEGIN
            generated_id := uuidv7();
            NEW.id := generated_id;
            RETURN NEW;
        END $$;
        CREATE TRIGGER assign_generated_id BEFORE INSERT ON items
        FOR EACH ROW EXECUTE FUNCTION assign_generated_id();
    ";
    assert!(
        Pg2Sqlite::default()
            .sql(source)
            .unwrap()
            .translate(&options(UuidRepresentation::Blob))
            .is_err()
    );
}

#[test]
fn transformed_copy_captures_its_complete_assignment_time_value() {
    let (mut connection, calls) = replica(
        "generated_id UUID; copied_text TEXT;",
        "
        generated_id := uuidv7();
        copied_text := generated_id::text || (SELECT count(*)::text FROM audit);
        INSERT INTO audit (id) VALUES (NEW.id);
        generated_id := uuidv7();
        INSERT INTO observations (id, first_value, second_value, note)
        VALUES (NEW.id, generated_id, generated_id, copied_text);
    ",
        "",
        UuidRepresentation::Text,
    );
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    let values = text_schema::observations::table
        .select((text_schema::observations::first_value, text_schema::observations::note))
        .load::<(Option<String>, Option<String>)>(&mut connection)
        .unwrap();
    assert_eq!(values, vec![(Some(text_value(2)), Some(format!("{}0", text_value(1))))]);
}

#[test]
fn generated_expression_preserves_short_circuit_evaluation() {
    let (mut connection, calls) = replica(
        "generated_id UUID;",
        "
        generated_id := CASE WHEN NEW.id > 0 THEN uuidv7() ELSE NULL END;
        INSERT INTO observations (id, first_value, second_value)
        VALUES (NEW.id, generated_id, generated_id);
    ",
        "",
        UuidRepresentation::Blob,
    );
    insert_input(&mut connection, -1);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        observed_values(&mut connection),
        vec![(None, None), (Some(binary_value(1)), Some(binary_value(1))),]
    );
}

#[test]
fn maintenance_generated_assignment_is_shared_by_later_new_reads() {
    let helpers = "
        CREATE FUNCTION maintain_values() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            NEW.first_value := uuidv7();
            NEW.second_value := NEW.first_value;
            RETURN NEW;
        END $$;
        CREATE TRIGGER maintain_values BEFORE INSERT ON observations
        FOR EACH ROW EXECUTE FUNCTION maintain_values();
    ";
    let (mut connection, calls) = replica(
        "",
        "INSERT INTO observations (id) VALUES (NEW.id);",
        helpers,
        UuidRepresentation::Blob,
    );
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        observed_values(&mut connection),
        vec![(Some(binary_value(1)), Some(binary_value(1)))],
    );
}

#[test]
fn maintenance_captures_follow_source_branches_and_reassignments() {
    for statements in [
        "IF NEW.id > 0 THEN NEW.first_value := uuidv7(); END IF;
         NEW.second_value := NEW.first_value;",
        "IF NEW.id < 0 THEN NEW.first_value := NULL; ELSIF NEW.id > 0 THEN NEW.first_value := uuidv7(); END IF;
         NEW.second_value := NEW.first_value;",
        "IF NEW.id > 0 THEN
             NEW.first_value := uuidv7();
             NEW.first_value := uuidv7();
         END IF;
         NEW.second_value := NEW.first_value;",
    ] {
        let helpers = format!("
            CREATE FUNCTION maintain_values() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN {statements} RETURN NEW; END $$;
            CREATE TRIGGER maintain_values BEFORE INSERT ON observations
            FOR EACH ROW EXECUTE FUNCTION maintain_values();
        ");
        let (mut connection, calls) = replica(
            "", "INSERT INTO observations (id) VALUES (NEW.id);",
            &helpers, UuidRepresentation::Blob,
        );
        insert_input(&mut connection, -1);
        assert_eq!(calls.load(Ordering::Relaxed), 0, "{statements}");
        insert_input(&mut connection, 1);
        let expected = statements.matches("uuidv7()").count();
        assert_eq!(calls.load(Ordering::Relaxed), expected, "{statements}");
        let last_value = binary_value(expected);
        assert_eq!(observed_values(&mut connection), vec![
            (None, None), (Some(last_value.clone()), Some(last_value)),
        ], "{statements}");
    }
}

fn mixed_case_maintenance(statements: &str) {
    let helpers = format!(
        "
        CREATE TABLE items (id INTEGER PRIMARY KEY, token UUID, copy UUID);
        CREATE FUNCTION maintain_items() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN {statements} RETURN NEW; END $$;
        CREATE TRIGGER maintain_items BEFORE INSERT ON items
        FOR EACH ROW EXECUTE FUNCTION maintain_items();
    "
    );
    let (mut connection, calls) =
        replica("", "INSERT INTO items (id) VALUES (NEW.id);", &helpers, UuidRepresentation::Blob);
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let rows = items::table
        .select((items::token, items::copy))
        .load::<ObservedPair>(&mut connection)
        .unwrap();
    assert_eq!(rows, vec![(Some(binary_value(1)), Some(binary_value(1)))]);
}

#[test]
fn mixed_case_generated_maintenance_target() {
    mixed_case_maintenance("NEW.Token := uuidv7(); NEW.copy := NEW.Token;");
}

#[test]
fn mixed_case_copy_maintenance_target() {
    mixed_case_maintenance("NEW.token := uuidv7(); NEW.Copy := NEW.token;");
}

fn assert_stale_generated_guard_refused(body: &str) {
    let source = format!(
        "{SCHEMA}
        CREATE TABLE seen (k INTEGER PRIMARY KEY);
        CREATE FUNCTION identity_uuid(value UUID) RETURNS UUID LANGUAGE plpgsql AS $$
        BEGIN RETURN value; END $$;
        CREATE FUNCTION stale_guard() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE duplicate_id UUID;
        BEGIN {body} RETURN NEW; END $$;
        CREATE TRIGGER stale_guard AFTER INSERT ON inputs
        FOR EACH ROW EXECUTE FUNCTION stale_guard();
    "
    );
    let parsed = Pg2Sqlite::default().sql(&source).unwrap();
    let error = parsed
        .translate_to_sql(&options(UuidRepresentation::Blob))
        .expect_err("a generated capture under a rewritten guard must be refused");
    assert!(error.to_string().contains("an IF condition reads `seen`"), "{error}");
}

#[test]
fn stale_guard_refuses_generated_assignment_in_else() {
    assert_stale_generated_guard_refused(
        "
        IF NOT EXISTS (SELECT 1 FROM seen WHERE k = NEW.id) THEN
            INSERT INTO seen (k) VALUES (NEW.id);
        ELSE duplicate_id := uuidv7();
        END IF;
    ",
    );
}

#[test]
fn stale_guard_refuses_generated_assignment_after_guarded_write() {
    assert_stale_generated_guard_refused(
        "
        IF NOT EXISTS (SELECT 1 FROM seen WHERE k = NEW.id) THEN
            INSERT INTO seen (k) VALUES (NEW.id);
            duplicate_id := uuidv7();
        END IF;
    ",
    );
}

#[test]
fn stale_guard_refuses_generated_select_into() {
    assert_stale_generated_guard_refused(
        "
        IF NOT EXISTS (SELECT 1 FROM seen WHERE k = NEW.id) THEN
            INSERT INTO seen (k) VALUES (NEW.id);
            SELECT uuidv7() INTO duplicate_id;
        END IF;
    ",
    );
}

#[test]
fn stale_guard_refuses_generated_helper_argument() {
    assert_stale_generated_guard_refused(
        "
        IF NOT EXISTS (SELECT 1 FROM seen WHERE k = NEW.id) THEN
            INSERT INTO seen (k) VALUES (NEW.id);
            duplicate_id := identity_uuid(uuidv7());
        END IF;
    ",
    );
}

#[test]
fn cancellation_after_perform_keeps_completed_writes_and_generated_values() {
    for (perform, expected_calls) in [
        ("PERFORM 1 / NEW.id;", 1),
        ("PERFORM uuidv7();", 2),
        ("PERFORM uuidv7() FROM audit;", 2),
        ("PERFORM ceil(1.0 / NEW.id);", 1),
        ("PERFORM uuidv7() UNION ALL SELECT uuidv7();", 3),
    ] {
        let statements = format!(
            "generated := uuidv7();
            INSERT INTO observations VALUES (NEW.id, generated, generated, 'before');
            {perform}
            RETURN NULL;
            INSERT INTO observations VALUES (NEW.id + 1, generated, generated, 'after');"
        );
        let (mut connection, calls) =
            replica("generated UUID;", &statements, "", UuidRepresentation::Blob);
        diesel::insert_into(audit::table).values(audit::id.eq(1)).execute(&mut connection).unwrap();
        insert_input(&mut connection, 1);
        assert_eq!(calls.load(Ordering::Relaxed), expected_calls, "{perform}");
        assert_eq!(
            observed_values(&mut connection),
            [(Some(binary_value(1)), Some(binary_value(1)))],
            "{perform}"
        );
        assert_eq!(inputs::table.count().get_result::<i64>(&mut connection).unwrap(), 1);
    }
}

#[test]
fn captured_values_preserve_source_names_matching_private_bindings() {
    let (mut connection, calls) = replica(
        "pg2sqlite_stored_0 UUID; pg2sqlite_stored_1 UUID; generated UUID;",
        "pg2sqlite_stored_0 := uuidv7();
         pg2sqlite_stored_1 := uuidv7();
         generated := pg2sqlite_stored_0;
         INSERT INTO observations VALUES (NEW.id, generated, pg2sqlite_stored_1, 'reserved');",
        "",
        UuidRepresentation::Blob,
    );
    insert_input(&mut connection, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(observed_values(&mut connection), [(Some(binary_value(1)), Some(binary_value(2)))]);
}
