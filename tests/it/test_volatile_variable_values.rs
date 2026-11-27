use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use diesel::{connection::SimpleConnection, prelude::*, sql_types::BigInt};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! { inputs (id) { id -> Integer, first_value -> Nullable<Text>, second_value -> Nullable<Text>, } }
diesel::table! { observed (id) { id -> Integer, first_value -> Nullable<Text>, second_value -> Nullable<Text>, } }

#[diesel::declare_sql_function]
extern "SQL" {
    fn counted() -> BigInt;
}

fn replica(
    declarations: &str,
    body: &str,
    helpers: &str,
    maintenance: bool,
) -> (crate::helpers::Replica, Arc<AtomicI64>, String) {
    let timing = if maintenance { "BEFORE" } else { "AFTER" };
    let source = format!(
        "CREATE TABLE inputs (id INTEGER PRIMARY KEY, first_value TEXT, second_value TEXT); CREATE TABLE observed (id INTEGER PRIMARY KEY, first_value TEXT, second_value TEXT); CREATE TABLE source_rows (id INTEGER PRIMARY KEY); {helpers} CREATE FUNCTION run_body() RETURNS trigger LANGUAGE plpgsql AS $$ {declarations} BEGIN {body} RETURN NEW; END $$; CREATE TRIGGER run_body {timing} INSERT ON inputs FOR EACH ROW EXECUTE FUNCTION run_body();"
    );
    let options = if body.contains("clock_timestamp") {
        crate::helpers::deployment::utc_request()
    } else {
        Pg2SqliteOptions::default()
    }
    .with_user_defined_functions(["counted"]);
    let source = if body.contains("clock_timestamp") {
        format!("CREATE ROLE app; {source} GRANT ALL ON inputs, observed, source_rows TO app;")
    } else {
        source
    };
    let translated = Pg2Sqlite::default().sql(&source).unwrap().translate_to_sql(&options).unwrap();
    let emitted = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join(";\n");
    let mut connection = crate::helpers::Replica::establish(":memory:").unwrap();
    translated.register_managed(&mut connection).unwrap();
    let calls = Arc::new(AtomicI64::new(0));
    let counter = Arc::clone(&calls);
    counted_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        move || counter.fetch_add(1, Ordering::Relaxed) + 1,
    )
    .unwrap();
    connection.batch_execute(&emitted).unwrap();
    connection.batch_execute("INSERT INTO source_rows VALUES (10), (20)").unwrap();
    diesel::insert_into(inputs::table).values(inputs::id.eq(1)).execute(&mut connection).unwrap();
    (connection, calls, emitted)
}

fn assert_counted(body: &str, helpers: &str, expected: Option<&str>, count: i64) {
    let (mut connection, calls, _) = replica("DECLARE value BIGINT;", body, helpers, false);
    let rows = observed::table
        .select((observed::first_value, observed::second_value))
        .load::<(Option<String>, Option<String>)>(&mut connection)
        .unwrap();
    assert_eq!(rows, vec![(expected.map(str::to_owned), expected.map(str::to_owned))]);
    assert_eq!(calls.load(Ordering::Relaxed), count);
}

#[test]
fn volatile_value_single_statement() {
    assert_counted(
        "value := counted(); INSERT INTO observed VALUES (1, value::text, value::text);",
        "",
        Some("1"),
        1,
    );
}

#[test]
fn volatile_value_insert_then_update() {
    assert_counted(
        "value := counted(); INSERT INTO observed VALUES (1, value::text, NULL); UPDATE observed SET second_value = value::text WHERE id = 1;",
        "",
        Some("1"),
        1,
    );
}

#[test]
fn volatile_value_helper_return() {
    assert_counted(
        "value := helper(); INSERT INTO observed VALUES (1, value::text, value::text);",
        "CREATE FUNCTION helper() RETURNS BIGINT LANGUAGE plpgsql VOLATILE AS $$ BEGIN RETURN counted(); END $$;",
        Some("1"),
        1,
    );
}

#[test]
fn volatile_value_helper_local() {
    assert_counted(
        "value := helper(); INSERT INTO observed VALUES (1, value::text, value::text);",
        "CREATE FUNCTION helper() RETURNS BIGINT LANGUAGE plpgsql VOLATILE AS $$ DECLARE local_value BIGINT; BEGIN local_value := counted(); RETURN local_value + local_value; END $$;",
        Some("2"),
        1,
    );
}

#[test]
fn volatile_value_untaken_assignment() {
    assert_counted(
        "IF NEW.id < 0 THEN value := counted(); END IF; INSERT INTO observed VALUES (1, value::text, value::text);",
        "",
        None,
        0,
    );
}

#[test]
fn volatile_value_unread_assignment() {
    assert_counted(
        "value := counted(); INSERT INTO observed VALUES (1, '0', '0');",
        "",
        Some("0"),
        1,
    );
}

#[test]
fn volatile_value_select_counted_into() {
    assert_counted(
        "SELECT counted() INTO value; INSERT INTO observed VALUES (1, value::text, NULL); UPDATE observed SET second_value = value::text WHERE id = 1;",
        "",
        Some("1"),
        1,
    );
}

#[test]
fn volatile_value_maintenance_copy() {
    let (mut connection, calls, _) = replica(
        "",
        "NEW.first_value := counted()::bigint::text; NEW.second_value := NEW.first_value;",
        "",
        true,
    );
    let values = inputs::table
        .select((inputs::first_value, inputs::second_value))
        .first::<(Option<String>, Option<String>)>(&mut connection)
        .unwrap();
    assert_eq!(values, (Some("1".into()), Some("1".into())));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

fn assert_repeated_draw(assignment: &str, data_type: &str) {
    let (mut connection, _, emitted) = replica(
        &format!("DECLARE draw {data_type};"),
        &format!(
            "{assignment} INSERT INTO observed VALUES (1, draw::text, draw::text); INSERT INTO observed VALUES (2, draw::text, draw::text);"
        ),
        "",
        false,
    );
    let rows = observed::table
        .order(observed::id)
        .select((observed::first_value, observed::second_value))
        .load::<(Option<String>, Option<String>)>(&mut connection)
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].0.is_some());
    assert_eq!(rows[0].0, rows[0].1);
    assert_eq!(rows[0], rows[1]);
    assert!(emitted.contains("pg2sqlite_body_values"));
}

#[test]
fn volatile_value_random() {
    assert_repeated_draw("draw := random();", "double precision");
}
#[test]
fn volatile_value_cast() {
    assert_repeated_draw("draw := CAST(random() AS TEXT);", "text");
}
#[test]
fn volatile_value_multiply() {
    assert_repeated_draw("draw := random() * 10;", "double precision");
}
#[test]
fn volatile_value_concat() {
    assert_repeated_draw("draw := 'draw=' || CAST(random() AS TEXT);", "text");
}
#[test]
fn volatile_value_scalar_aggregate() {
    assert_repeated_draw(
        "draw := (SELECT MAX(id) + random() FROM source_rows);",
        "double precision",
    );
}
#[test]
fn volatile_value_select_random_into() {
    assert_repeated_draw("SELECT random() INTO draw;", "double precision");
}
#[test]
fn volatile_value_clock_timestamp() {
    assert_repeated_draw("draw := clock_timestamp();", "TIMESTAMPTZ");
}

#[test]
fn volatile_value_stable_helper_remains_inline() {
    let (mut connection, calls, emitted) = replica(
        "DECLARE value BIGINT;",
        "value := helper(); INSERT INTO observed VALUES (1, value::text, value::text);",
        "CREATE FUNCTION helper() RETURNS BIGINT LANGUAGE SQL STABLE AS $$ SELECT 7::bigint $$;",
        false,
    );
    assert!(!emitted.contains("pg2sqlite_body_values"));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        observed::table
            .select(observed::first_value)
            .first::<Option<String>>(&mut connection)
            .unwrap(),
        Some("7".into())
    );
}

#[test]
fn volatile_value_stable_helper_with_counted_is_captured() {
    assert_counted(
        "value := helper(); INSERT INTO observed VALUES (1, value::text, NULL); UPDATE observed SET second_value = value::text WHERE id = 1;",
        "CREATE FUNCTION helper() RETURNS BIGINT LANGUAGE SQL STABLE AS $$ SELECT counted() $$;",
        Some("1"),
        1,
    );
}

#[test]
fn volatile_value_declared_helper_is_captured() {
    let (_, _, emitted) = replica(
        "DECLARE value BIGINT;",
        "value := helper(); INSERT INTO observed VALUES (1, value::text, value::text);",
        "CREATE FUNCTION helper() RETURNS BIGINT LANGUAGE plpgsql VOLATILE AS $$ BEGIN RETURN 7; END $$;",
        false,
    );
    assert!(emitted.contains("pg2sqlite_body_values"));
}

#[test]
fn volatile_value_preserves_refusals() {
    for (helpers, body) in [
        (
            "",
            "value := (SELECT count(*) FROM source_rows); INSERT INTO source_rows VALUES (1); INSERT INTO observed VALUES (value);",
        ),
        ("", "value := nextval('counter'); INSERT INTO observed VALUES (value);"),
        (
            "CREATE FUNCTION helper() RETURNS BIGINT LANGUAGE SQL VOLATILE AS $$ SELECT counted() $$;",
            "value := helper(); INSERT INTO observed VALUES (value);",
        ),
    ] {
        let source = format!(
            "CREATE TABLE inputs (id INTEGER PRIMARY KEY); CREATE TABLE source_rows (id INTEGER PRIMARY KEY); CREATE TABLE observed (value BIGINT); {helpers} CREATE FUNCTION run_body() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE value BIGINT; BEGIN {body} RETURN NEW; END $$; CREATE TRIGGER run_body AFTER INSERT ON inputs FOR EACH ROW EXECUTE FUNCTION run_body();"
        );
        assert!(
            Pg2Sqlite::default()
                .sql(&source)
                .unwrap()
                .translate_to_sql(
                    &Pg2SqliteOptions::default().with_user_defined_functions(["counted"])
                )
                .is_err()
        );
    }
}

#[test]
fn volatile_value_declaration_default() {
    let (mut connection, calls, _) = replica(
        "DECLARE value BIGINT := counted();",
        "INSERT INTO observed VALUES (1, value::text, value::text);",
        "",
        false,
    );
    assert_eq!(
        observed::table
            .select(observed::first_value)
            .first::<Option<String>>(&mut connection)
            .unwrap(),
        Some("1".into())
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}
