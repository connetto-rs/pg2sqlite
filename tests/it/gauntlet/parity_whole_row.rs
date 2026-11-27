//! Parity for the whole row a trigger reads as `NEW` or `OLD`.
//!
//! `to_jsonb(NEW)`, `to_json(NEW)` and `row_to_json(NEW)` make a JSON object of
//! every column of the firing table, each value converted as its type converts
//! to JSON. `OLD IS DISTINCT FROM NEW` compares the two rows column by column,
//! in a trigger's `WHEN` and in its body. The same writes run on both engines
//! and the audit rows the triggers leave are compared.
//!
//! JSON is compared through PostgreSQL's `jsonb`, since the engines lay out
//! whitespace differently. The `json` column holds the column-ordered object
//! `row_to_json` and `to_json` make, so its keys are also compared in order.

use diesel::{
    pg::PgConnection,
    prelude::*,
    sql_types::{Nullable, Text},
};
use pg2sqlite::prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};

use crate::helpers::{establish_connection, postgres as postgres_harness};

/// Three tables with the same columns, one for each place a whole-row value
/// sits, and the table their triggers write. The view reads the JSON as text,
/// which both engines then hand over as the same Rust type.
const SCHEMA: &str = "
CREATE TYPE mood AS ENUM ('sad', 'happy');
CREATE TABLE audited (
    id     UUID PRIMARY KEY,
    label  TEXT,
    n      INTEGER,
    flag   BOOLEAN,
    amount NUMERIC(10,2),
    at     TIMESTAMPTZ,
    naive  TIMESTAMP,
    day    DATE,
    doc    JSONB,
    tags   INTEGER[],
    raw    BYTEA,
    mood   mood
);
CREATE TABLE watched (
    id     UUID PRIMARY KEY,
    label  TEXT,
    n      INTEGER,
    flag   BOOLEAN,
    amount NUMERIC(10,2),
    at     TIMESTAMPTZ,
    naive  TIMESTAMP,
    day    DATE,
    doc    JSONB,
    tags   INTEGER[],
    raw    BYTEA,
    mood   mood
);
CREATE TABLE audit (
    seq      SERIAL PRIMARY KEY,
    source   TEXT NOT NULL,
    op       TEXT NOT NULL,
    old_row  JSONB,
    new_row  JSONB,
    new_json JSON
);
CREATE VIEW audit_text AS
    SELECT seq, source, op, old_row::text AS old_row, new_row::text AS new_row,
           new_json::text AS new_json
    FROM audit;

CREATE FUNCTION audited_inserted() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO audit (source, op, new_row, new_json)
        VALUES ('audited', 'INSERT', to_jsonb(NEW), row_to_json(NEW));
    RETURN NULL;
END $$;
CREATE TRIGGER audited_inserted AFTER INSERT ON audited
    FOR EACH ROW EXECUTE FUNCTION audited_inserted();

CREATE FUNCTION audited_updated() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO audit (source, op, old_row, new_row, new_json)
        VALUES ('audited', 'UPDATE', to_jsonb(OLD), to_jsonb(NEW), to_json(NEW));
    RETURN NULL;
END $$;
CREATE TRIGGER audited_updated AFTER UPDATE ON audited
    FOR EACH ROW WHEN (OLD IS DISTINCT FROM NEW) EXECUTE FUNCTION audited_updated();

CREATE FUNCTION audited_deleted() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO audit (source, op, old_row) VALUES ('audited', 'DELETE', to_jsonb(OLD));
    RETURN NULL;
END $$;
CREATE TRIGGER audited_deleted AFTER DELETE ON audited
    FOR EACH ROW EXECUTE FUNCTION audited_deleted();

CREATE FUNCTION watched_updated() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD IS NOT DISTINCT FROM NEW THEN
        INSERT INTO audit (source, op) VALUES ('watched', 'same');
    END IF;
    IF OLD IS DISTINCT FROM NEW THEN
        INSERT INTO audit (source, op) VALUES ('watched', 'changed');
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER watched_updated AFTER UPDATE ON watched
    FOR EACH ROW EXECUTE FUNCTION watched_updated();

CREATE TABLE logged (
    id     UUID PRIMARY KEY,
    label  TEXT,
    n      INTEGER,
    flag   BOOLEAN,
    amount NUMERIC(10,2),
    at     TIMESTAMPTZ,
    naive  TIMESTAMP,
    day    DATE,
    doc    JSONB,
    tags   INTEGER[],
    raw    BYTEA,
    mood   mood
);
CREATE FUNCTION logged_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    rec RECORD;
BEGIN
    INSERT INTO audit (source, op, old_row, new_row)
    VALUES (
        'logged',
        TG_OP || CASE WHEN OLD IS DISTINCT FROM NEW THEN ' distinct' ELSE ' same' END,
        CASE WHEN TG_OP = 'INSERT' THEN NULL ELSE to_jsonb(OLD) END,
        CASE WHEN TG_OP = 'DELETE' THEN NULL ELSE to_jsonb(NEW) END
    );
    IF TG_OP = 'DELETE' THEN
        rec := OLD;
    ELSE
        rec := NEW;
    END IF;
    INSERT INTO audit (source, op, new_row)
    VALUES (
        'logged record',
        TG_OP,
        jsonb_build_array(
            rec.id, rec.id::text, rec.label, rec.n, rec.day::text, rec.flag, rec.mood,
            rec.mood::text
        )
    );
    RETURN NULL;
END $$;
CREATE TRIGGER logged_changed AFTER INSERT OR DELETE ON logged
    FOR EACH ROW EXECUTE FUNCTION logged_changed();
";

/// A row with every column set and a row with every column but its key unset.
const SEEDS: &str = r#"
INSERT INTO audited VALUES ('00010203-0405-0607-0809-0a0b0c0d0e0f', 'x"y', 7, true, 19.90, '2024-01-02 03:04:05.5+00', '2024-01-02 03:04:05', '2024-12-31', '{"b":[1,2],"a":{"z":1}}', ARRAY[1,2], '\x0102', 'happy');
INSERT INTO audited (id) VALUES ('ffffffff-ffff-ffff-ffff-ffffffffffff');
INSERT INTO watched SELECT * FROM audited;
INSERT INTO logged SELECT * FROM audited;
"#;

/// An update that changes nothing, one that changes one column, a NULL set to a
/// value, a value set to NULL, a jsonb column given another value, and the
/// deletes. Each runs on every table.
const WRITES: [&str; 6] = [
    "UPDATE {t} SET label = label, doc = doc, at = at",
    "UPDATE {t} SET label = 'changed' WHERE id = '00010203-0405-0607-0809-0a0b0c0d0e0f'",
    "UPDATE {t} SET n = 5 WHERE id = 'ffffffff-ffff-ffff-ffff-ffffffffffff'",
    "UPDATE {t} SET amount = NULL WHERE id = '00010203-0405-0607-0809-0a0b0c0d0e0f'",
    r#"UPDATE {t} SET doc = '{"b":[1,3]}' WHERE id = '00010203-0405-0607-0809-0a0b0c0d0e0f'"#,
    "DELETE FROM {t} WHERE id = 'ffffffff-ffff-ffff-ffff-ffffffffffff'",
];

mod schema {
    diesel::table! {
        audit_text (seq) {
            seq -> Integer,
            source -> Text,
            op -> Text,
            old_row -> Nullable<Text>,
            new_row -> Nullable<Text>,
            new_json -> Nullable<Text>,
        }
    }
}

/// One audit row as text.
type AuditRow = (i32, String, String, Option<String>, Option<String>, Option<String>);

fn options() -> Pg2SqliteOptions {
    crate::helpers::jsonb_functions::with_functions(
        crate::helpers::function_options()
            .with_uuid_representation(UuidRepresentation::Blob)
            .with_array_representation(ArrayRepresentation::Json),
    )
}

/// Every write, on every table, as one script.
fn writes() -> String {
    ["audited", "watched", "logged"]
        .iter()
        .flat_map(|table| WRITES.iter().map(move |write| write.replace("{t}", table) + ";\n"))
        .collect()
}

/// One audit row with its JSON read through `jsonb`, and the keys of the
/// column-ordered object in the order they were stored.
type Compared = (i32, String, String, Option<String>, Option<String>, Option<String>, Vec<String>);

fn postgres_rows() -> Vec<AuditRow> {
    use schema::audit_text;
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(
        &mut connection,
        &format!("SET TimeZone = 'UTC';{SCHEMA}{SEEDS}{}", writes()),
    )
    .expect("apply the script to PostgreSQL");
    audit_text::table
        .order(audit_text::seq.asc())
        .load(&mut connection)
        .expect("read PostgreSQL's audit rows")
}

// The script is translator output, a runtime string with no typed form.
fn sqlite_rows() -> Vec<AuditRow> {
    use schema::audit_text;
    let statements = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}{SEEDS}{}", writes()))
        .expect("parse the script")
        .translate_to_sql(&options())
        .expect("translate the script")
        .into_vec();
    let mut connection: crate::helpers::Replica = establish_connection();
    for statement in &statements {
        diesel::sql_query(statement.as_str())
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("SQLite refused {statement}: {error}"));
    }
    audit_text::table
        .order(audit_text::seq.asc())
        .load(&mut connection)
        .expect("read SQLite's audit rows")
}

#[derive(QueryableByName)]
struct Val {
    #[diesel(sql_type = Nullable<Text>)]
    val: Option<String>,
}

/// `text` read back through PostgreSQL's `jsonb`.
fn as_jsonb(pg: &mut PgConnection, text: Option<&str>) -> Option<String> {
    // A cast is not something the DSL spells over a bound value.
    diesel::sql_query("SELECT CAST($1 AS jsonb)::text AS val")
        .bind::<Nullable<Text>, _>(text)
        .get_result::<Val>(pg)
        .unwrap_or_else(|error| panic!("{text:?} is not JSON: {error}"))
        .val
}

#[derive(QueryableByName)]
struct Key {
    #[diesel(sql_type = Text)]
    key: String,
}

/// The top-level keys of the JSON object `text`, in the order stored.
fn keys(pg: &mut PgConnection, text: Option<&str>) -> Vec<String> {
    // A set-returning call over a bound value has no DSL form.
    diesel::sql_query("SELECT json_object_keys(CAST($1 AS json)) AS key")
        .bind::<Nullable<Text>, _>(text)
        .load::<Key>(pg)
        .unwrap_or_else(|error| panic!("{text:?} is not a JSON object: {error}"))
        .into_iter()
        .map(|key| key.key)
        .collect()
}

fn compared(pg: &mut PgConnection, rows: Vec<AuditRow>) -> Vec<Compared> {
    rows.into_iter()
        .map(|(seq, source, op, old_row, new_row, new_json)| {
            (
                seq,
                source,
                op,
                as_jsonb(pg, old_row.as_deref()),
                as_jsonb(pg, new_row.as_deref()),
                as_jsonb(pg, new_json.as_deref()),
                keys(pg, new_json.as_deref()),
            )
        })
        .collect()
}

#[test]
fn whole_rows_convert_and_compare_as_postgresql_does() {
    let mut pg = postgres_harness::fresh_database();
    let expected = compared(&mut pg, postgres_rows());
    let ops: Vec<&str> = expected.iter().map(|row| row.2.as_str()).collect();
    assert_eq!(
        ops,
        [
            "INSERT",
            "INSERT",
            "INSERT distinct",
            "INSERT",
            "INSERT distinct",
            "INSERT",
            "UPDATE",
            "UPDATE",
            "UPDATE",
            "UPDATE",
            "DELETE",
            "same",
            "same",
            "changed",
            "changed",
            "changed",
            "changed",
            "DELETE distinct",
            "DELETE",
        ],
        "the writes fire what the comparisons say, the update changing nothing included",
    );
    let actual = compared(&mut pg, sqlite_rows());
    assert_eq!(actual, expected);
}
