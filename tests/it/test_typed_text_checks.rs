//! The `CHECK` a column stored as text carries for its PostgreSQL type.
//!
//! A `uuid` under the text representation, a date or time, and a JSON
//! document are all `TEXT` in the replica. A value written past the
//! translator, a bound parameter or a row applied directly, reaches the column
//! as it is, so the column itself admits only the text the translation
//! writes for the type.

use diesel::{prelude::*, result::DatabaseErrorKind, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2SqliteOptions, UuidRepresentation};

use crate::helpers::{apply_translated_ddl, jsonb_functions};

diesel::table! {
    typed (id) {
        id -> Integer,
        v -> Nullable<Text>,
    }
}

fn text_uuid_options() -> Pg2SqliteOptions {
    jsonb_functions::with_functions(Pg2SqliteOptions::default())
        .with_uuid_representation(UuidRepresentation::Text)
}

/// A replica holding `typed (id, v <data_type>)`.
fn replica(data_type: &str) -> SqliteConnection {
    replica_with(data_type, &text_uuid_options())
}

/// A replica holding `typed (id, v <data_type>)`, translated with `options`.
fn replica_with(data_type: &str, options: &Pg2SqliteOptions) -> SqliteConnection {
    apply_translated_ddl(
        &format!("CREATE TABLE typed (id INT PRIMARY KEY, v {data_type});"),
        options,
    )
}

fn write(
    connection: &mut SqliteConnection,
    id: i32,
    value: Option<&str>,
) -> diesel::QueryResult<usize> {
    diesel::insert_into(typed::table)
        .values((typed::id.eq(id), typed::v.eq(value)))
        .execute(connection)
}

/// Asserts the replica of `data_type` takes every value of `admitted` and
/// NULL, and that its `CHECK` refuses every value of `refused`.
fn assert_admits(data_type: &str, admitted: &[&str], refused: &[&str]) {
    let mut connection = replica(data_type);
    let mut id = 0;
    for value in admitted.iter().copied().map(Some).chain([None]) {
        id += 1;
        write(&mut connection, id, value)
            .unwrap_or_else(|error| panic!("{data_type} must admit {value:?}: {error}"));
    }
    for value in refused {
        id += 1;
        assert!(
            matches!(
                write(&mut connection, id, Some(value)),
                Err(diesel::result::Error::DatabaseError(DatabaseErrorKind::CheckViolation, _))
            ),
            "{data_type} must refuse {value:?} through its CHECK"
        );
    }
}

#[test]
fn a_text_uuid_holds_the_canonical_lowercase_form() {
    assert_admits(
        "uuid",
        &["00000000-0000-0000-0000-000000000001", "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"],
        &[
            "not a uuid",
            "",
            "A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11",
            "a0eebc999c0b4ef8bb6d6bb9bd380a11",
            "{a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11}",
            "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a1g",
            "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11 ",
            "a0eebc9-99c0b-4ef8-bb6d-6bb9bd380a11",
        ],
    );
}

#[test]
fn a_timestamptz_holds_utc_text_with_microseconds() {
    assert_admits(
        "timestamptz",
        &[
            "2024-01-01 10:00:00.000000+00:00",
            "2024-02-29 23:59:59.999999+00:00",
            "0001-01-01 00:00:00.000000+00:00",
            "9999-12-31 23:59:59.999999+00:00",
            "infinity",
            "-infinity",
        ],
        &[
            "yesterday-ish",
            "2024-01-01T10:00:00Z",
            "2024-01-01 10:00:00+00:00",
            "2024-01-01 10:00:00.123+00:00",
            "2024-01-01 10:00:00.000000+01:00",
            "2024-01-01 10:00:00.000000 UTC",
            "2023-02-29 00:00:00.000000+00:00",
            "2024-01-01 24:00:00.000000+00:00",
            "2024-01-01 10:60:00.000000+00:00",
            "0000-01-01 00:00:00.000000+00:00",
            "Infinity",
        ],
    );
}

#[test]
fn a_timestamp_holds_its_wall_clock_text() {
    assert_admits(
        "timestamp",
        &[
            "2024-03-05 14:07:09",
            "2024-03-05 14:07:09.5",
            "2024-03-05 14:07:09.123",
            "2024-03-05 14:07:09.123456",
            "infinity",
            "-infinity",
        ],
        &[
            "2024-03-05",
            "2024-03-05T14:07:09",
            "2024-03-05 14:07:09.",
            "2024-03-05 14:07:09.1234567",
            "2024-03-05 14:07:09.12a",
            "2024-03-05 14:07:09+00:00",
            "2024-3-5 14:07:09",
            "2024-02-30 00:00:00",
            "2024-03-05 25:00:00",
        ],
    );
}

#[test]
fn a_date_holds_its_calendar_text() {
    assert_admits(
        "date",
        &["2024-03-05", "2024-02-29", "infinity", "-infinity"],
        &["2024-3-5", "2023-02-29", "2024-03-05 00:00:00", "0000-01-01", "tomorrow"],
    );
}

#[test]
fn a_time_holds_its_clock_text() {
    assert_admits(
        "time",
        &["14:07:09", "14:07:09.25", "00:00:00", "23:59:59.999999", "24:00:00"],
        &["14:07", "14:07:09+00:00", "24:00:00.5", "24:00:01", "14:60:00", "infinity"],
    );
}

#[test]
fn a_time_with_time_zone_holds_its_clock_and_offset_text() {
    assert_admits(
        "timetz",
        &["14:07:09+00:00", "14:07:09.25-05:30", "24:00:00+01:00"],
        &["14:07:09", "14:07:09Z", "14:07:09+0000", "14:07:09+00", "25:00:00+00:00"],
    );
}

#[test]
fn a_json_document_holds_valid_json() {
    for data_type in ["json", "jsonb"] {
        assert_admits(
            data_type,
            &[r#"{"a": 1}"#, "[1, 2]", "1", r#""text""#, "null"],
            &["{not json", "", "[1,", "{'a': 1}"],
        );
    }
}

/// The text the translation writes for each type: literals, the clock, and
/// arithmetic over both.
#[test]
fn every_value_the_translation_writes_passes_its_check() {
    crate::helpers::execute_all(
        &crate::helpers::deployment::utc_script(
            "CREATE TABLE written (
                 id INT PRIMARY KEY,
                 u uuid,
                 at timestamptz,
                 at3 timestamptz(3),
                 ts timestamp,
                 ts0 timestamp(0),
                 d date,
                 t time,
                 tz timetz,
                 j json,
                 jb jsonb
             );
             INSERT INTO written VALUES (1, 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11',
                 '2024-01-01 10:00:00.5+02', '2024-01-01 10:00:00.1234+02',
                 '2024-01-01 10:00:00.5', '2024-01-01 10:00:00.5', '2024-01-01', '24:00:00',
                 '10:00:00.5+02', '{\"a\": 1}', '{\"b\": 1.50, \"a\": 2}');
             INSERT INTO written (id, at, at3, ts, ts0, d, t, tz) VALUES (2, now(), now(),
                 LOCALTIMESTAMP, LOCALTIMESTAMP, CURRENT_DATE, LOCALTIME, CURRENT_TIME);
             INSERT INTO written (id, at, ts, d) VALUES (3, now() + interval '1 day 2 hours',
                 LOCALTIMESTAMP - interval '90 minutes', CURRENT_DATE + 1);
             INSERT INTO written (id, at, ts, d) VALUES (4, 'infinity', '-infinity', 'infinity');
             UPDATE written SET at = at + interval '1 month', ts = ts + interval '1 second',
                 d = d - 1 WHERE id = 3;",
        ),
        &crate::helpers::deployment::utc_request()
            .with_uuid_representation(UuidRepresentation::Text),
    );
}

/// With the stored-text checks left out, every column that would carry one
/// takes text PostgreSQL refuses for its type.
#[test]
fn leaving_out_the_stored_text_checks_admits_any_text() {
    let options = text_uuid_options().with_stored_text_checks_omitted();
    for (data_type, value) in [
        ("uuid", "not a uuid"),
        ("timestamptz", "yesterday-ish"),
        ("timestamp", "2024-02-30 10:00:00"),
        ("date", "tomorrow"),
        ("time", "25:00:00"),
        ("timetz", "10:00:00+00"),
        ("json", "{not json"),
        ("jsonb", "{not json"),
    ] {
        let mut connection = replica_with(data_type, &options);
        write(&mut connection, 1, Some(value))
            .unwrap_or_else(|error| panic!("{data_type} must admit {value:?}: {error}"));
    }
}

/// Leaving out the stored-text checks keeps every other declared bound.
#[test]
fn leaving_out_the_stored_text_checks_keeps_the_other_bounds() {
    let options = text_uuid_options().with_stored_text_checks_omitted();
    let mut connection = replica_with("varchar(3)", &options);
    assert!(matches!(
        write(&mut connection, 1, Some("abcd")),
        Err(diesel::result::Error::DatabaseError(DatabaseErrorKind::CheckViolation, _))
    ));
    let blob_options = jsonb_functions::with_functions(Pg2SqliteOptions::default())
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_stored_text_checks_omitted();
    let mut connection =
        apply_translated_ddl("CREATE TABLE typed (id INT PRIMARY KEY, v uuid);", &blob_options);
    assert!(matches!(
        diesel::sql_query("INSERT INTO typed (id, v) VALUES (1, x'00')").execute(&mut connection),
        Err(diesel::result::Error::DatabaseError(DatabaseErrorKind::CheckViolation, _))
    ));
}
