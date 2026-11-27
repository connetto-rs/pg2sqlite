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
    candidates (id) {
        id -> Integer,
    }
}

#[diesel::declare_sql_function]
extern "SQL" {
    fn counted_dispatch_uuid() -> diesel::sql_types::Binary;
}

const TABLES: &str = "
CREATE TABLE inputs (id INTEGER PRIMARY KEY);
CREATE TABLE candidates (id INTEGER PRIMARY KEY);
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("counted_dispatch_uuid")
}

fn connection() -> (Replica, Arc<AtomicUsize>) {
    let mut connection = establish_connection();
    let calls = Arc::new(AtomicUsize::new(0));
    let generator_calls = Arc::clone(&calls);
    counted_dispatch_uuid_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        move || {
            u128::try_from(generator_calls.fetch_add(1, Ordering::Relaxed) + 1)
                .unwrap()
                .to_be_bytes()
                .to_vec()
        },
    )
    .unwrap();
    (connection, calls)
}

fn seed_candidates(connection: &mut Replica) {
    for identifier in 1..=3 {
        diesel::insert_into(candidates::table)
            .values(candidates::id.eq(identifier))
            .execute(connection)
            .unwrap();
    }
}

fn loop_function(outside_loop: &str) -> String {
    format!(
        "CREATE OR REPLACE FUNCTION loop_body() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE candidate_id INTEGER;
        BEGIN
            {outside_loop}
            FOR candidate_id IN SELECT id FROM candidates LOOP
                DECLARE generated_id UUID := uuidv7(); BEGIN
                    IF generated_id IS NULL THEN RAISE EXCEPTION 'missing'; END IF;
                END;
            END LOOP;
            RETURN NEW;
        END $$;"
    )
}

#[test]
fn generated_dispatcher_replacement_applies_across_schema_batches() {
    let first = format!(
        "{TABLES} {}
        CREATE TRIGGER loop_body BEFORE INSERT ON inputs
        FOR EACH ROW EXECUTE FUNCTION loop_body();",
        loop_function("")
    );
    let parsed = Pg2Sqlite::default().sql(&first).unwrap();
    let empty_schema = Pg2Sqlite::default().sql("").unwrap().build_schema().unwrap();
    let translated = parsed.translate_with_schema(&empty_schema, &options()).unwrap();
    let (mut connection, calls) = connection();
    translated.register_managed(&mut connection).unwrap();
    for statement in translated.into_vec() {
        connection.batch_execute(&statement.to_string()).unwrap();
    }
    seed_candidates(&mut connection);
    diesel::insert_into(inputs::table).values(inputs::id.eq(1)).execute(&mut connection).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    let schema = parsed.build_schema().unwrap();
    let replacement = loop_function("IF NEW.id < 0 THEN RAISE EXCEPTION 'negative'; END IF;");
    let translated = Pg2Sqlite::default()
        .sql(&replacement)
        .unwrap()
        .translate_with_schema(&schema, &options())
        .unwrap();
    for statement in translated.into_vec() {
        connection.batch_execute(&statement.to_string()).unwrap();
    }
    diesel::insert_into(inputs::table).values(inputs::id.eq(2)).execute(&mut connection).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 6);
    assert!(
        diesel::insert_into(inputs::table)
            .values(inputs::id.eq(-1))
            .execute(&mut connection)
            .is_err()
    );
    assert_eq!(calls.load(Ordering::Relaxed), 6);
}

#[test]
fn untaken_generated_dispatcher_does_not_evaluate_loop_source() {
    let source = format!(
        "{TABLES}
        CREATE FUNCTION loop_body() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE candidate_id INTEGER; source_id UUID;
        BEGIN
            IF NEW.id < 0 THEN
                FOR candidate_id, source_id IN SELECT id, uuidv7() FROM candidates LOOP
                    DECLARE generated_id UUID := uuidv7(); BEGIN
                        IF generated_id = source_id THEN RAISE EXCEPTION 'equal'; END IF;
                    END;
                END LOOP;
            END IF;
            RETURN NEW;
        END $$;
        CREATE TRIGGER loop_body BEFORE INSERT ON inputs
        FOR EACH ROW EXECUTE FUNCTION loop_body();"
    );
    let translated = Pg2Sqlite::default().sql(&source).unwrap().translate(&options()).unwrap();
    let (mut connection, calls) = connection();
    translated.register_managed(&mut connection).unwrap();
    let mut source_type = None;
    for statement in translated.into_vec() {
        if let sqlparser::ast::Statement::CreateTable(table) = &statement
            && table.name.to_string().starts_with("pg2sqlite_body_input_loop_")
        {
            source_type = table
                .columns
                .iter()
                .find(|column| column.name.value == "pg2sqlite_loop_1")
                .map(|column| column.data_type.to_string());
        }
        connection.batch_execute(&statement.to_string()).unwrap();
    }
    assert_eq!(source_type.as_deref(), Some("BLOB"));
    seed_candidates(&mut connection);
    diesel::insert_into(inputs::table).values(inputs::id.eq(1)).execute(&mut connection).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(inputs::table.count().get_result::<i64>(&mut connection).unwrap(), 1);
}

#[test]
fn generated_dispatcher_preserves_definer_owner_policy_refusal() {
    let options = options()
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("dispatch_write_exempt")
        .with_deployment(deployment(["app"], []));
    for declaration in ["generated_id UUID;", "generated_id UUID := uuidv7();"] {
        let source = format!(
            "CREATE ROLE app;
            CREATE ROLE checker;
            {TABLES}
            CREATE TABLE secrets (id INTEGER PRIMARY KEY);
            ALTER TABLE secrets ENABLE ROW LEVEL SECURITY;
            CREATE POLICY secrets_visible ON secrets TO app USING (true);
            CREATE POLICY secrets_checked ON secrets TO checker USING (id > 0);
            GRANT SELECT, INSERT ON inputs TO app;
            GRANT SELECT ON candidates, secrets TO app, checker;
            CREATE FUNCTION loop_body() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$
            DECLARE candidate_id INTEGER;
            BEGIN
                FOR candidate_id IN SELECT id FROM candidates LOOP
                    DECLARE {declaration} BEGIN
                        IF NOT EXISTS (SELECT 1 FROM secrets WHERE id = candidate_id) THEN
                            RAISE EXCEPTION 'missing %', generated_id;
                        END IF;
                    END;
                END LOOP;
                RETURN NEW;
            END $$;
            ALTER FUNCTION loop_body() OWNER TO checker;
            CREATE TRIGGER loop_body BEFORE INSERT ON inputs
            FOR EACH ROW EXECUTE FUNCTION loop_body();"
        );
        let message = Pg2Sqlite::default()
            .sql(&source)
            .unwrap()
            .translate(&options)
            .expect_err("the owner's policies cannot be applied")
            .to_string();
        assert!(
            message.contains("function 'loop_body' is SECURITY DEFINER")
                && message.contains("table 'secrets' as its owner 'checker'")
                && message.contains("whom the table's row-level security policies bind"),
            "{declaration} {message}"
        );
    }
}

#[test]
fn generated_dispatcher_preserves_unknown_and_composed_query_inputs() {
    for (query, invalid_value, expected_calls) in [
        ("SELECT NULL FROM candidates", "candidate_id IS NOT NULL", 3),
        (
            "SELECT id FROM candidates UNION ALL SELECT id + 3 FROM candidates",
            "candidate_id < 1 OR candidate_id > 6",
            6,
        ),
        ("(SELECT id FROM candidates)", "candidate_id < 1 OR candidate_id > 3", 3),
        ("SELECT id AS selected_id FROM candidates", "candidate_id < 1 OR candidate_id > 3", 3),
    ] {
        let source = format!(
            "{TABLES}
            CREATE FUNCTION loop_body() RETURNS trigger LANGUAGE plpgsql AS $$
            DECLARE candidate_id INTEGER;
            BEGIN
                FOR candidate_id IN {query} LOOP
                    DECLARE generated_id UUID := uuidv7(); BEGIN
                        IF generated_id IS NULL OR ({invalid_value}) THEN
                            RAISE EXCEPTION 'invalid iteration value';
                        END IF;
                    END;
                END LOOP;
                RETURN NEW;
            END $$;
            CREATE TRIGGER loop_body BEFORE INSERT ON inputs
            FOR EACH ROW EXECUTE FUNCTION loop_body();"
        );
        let translated = Pg2Sqlite::default().sql(&source).unwrap().translate(&options()).unwrap();
        let (mut connection, calls) = connection();
        translated.register_managed(&mut connection).unwrap();
        for statement in translated.into_vec() {
            connection.batch_execute(&statement.to_string()).unwrap();
        }
        seed_candidates(&mut connection);
        diesel::insert_into(inputs::table)
            .values(inputs::id.eq(1))
            .execute(&mut connection)
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), expected_calls, "{query}");
        assert_eq!(inputs::table.count().get_result::<i64>(&mut connection).unwrap(), 1, "{query}");
    }
}

#[test]
fn generated_dispatcher_refuses_user_table_writes() {
    let source = format!(
        "{TABLES}
        CREATE FUNCTION loop_body() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE candidate_id INTEGER;
        BEGIN
            FOR candidate_id IN SELECT id FROM candidates LOOP
                DECLARE generated_id UUID := uuidv7(); BEGIN
                    IF generated_id IS NULL THEN RAISE EXCEPTION 'missing'; END IF;
                    INSERT INTO inputs VALUES (candidate_id);
                END;
            END LOOP;
            RETURN NEW;
        END $$;
        CREATE TRIGGER loop_body BEFORE INSERT ON inputs
        FOR EACH ROW EXECUTE FUNCTION loop_body();"
    );
    let message = Pg2Sqlite::default()
        .sql(&source)
        .unwrap()
        .translate(&options())
        .expect_err("iteration-local storage must not permit writes to user tables")
        .to_string();
    assert!(
        message.contains("the work a FOR loop or a PERFORM runs per row writes `inputs`"),
        "{message}"
    );
}

#[test]
fn generated_dispatcher_refuses_unnamed_query_inputs() {
    for (query, refusal) in
        [("SELECT * FROM candidates", "selects a wildcard"), ("VALUES (1), (2)", "is no SELECT")]
    {
        let source = format!(
            "{TABLES}
            CREATE FUNCTION loop_body() RETURNS trigger LANGUAGE plpgsql AS $$
            DECLARE candidate_id INTEGER;
            BEGIN
                FOR candidate_id IN {query} LOOP
                    DECLARE generated_id UUID := uuidv7(); BEGIN
                        IF generated_id IS NULL THEN RAISE EXCEPTION 'missing'; END IF;
                    END;
                END LOOP;
                RETURN NEW;
            END $$;
            CREATE TRIGGER loop_body BEFORE INSERT ON inputs
            FOR EACH ROW EXECUTE FUNCTION loop_body();"
        );
        let message = Pg2Sqlite::default()
            .sql(&source)
            .unwrap()
            .translate(&options())
            .expect_err("a dispatched query needs individually named columns")
            .to_string();
        assert!(message.contains(refusal), "{query} {message}");
    }
}
