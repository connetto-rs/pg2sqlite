//! The whole row a trigger reads as `NEW` or `OLD`.
//!
//! `tests/it/gauntlet/parity_whole_row.rs` compares the values with
//! PostgreSQL. These pin what SQLite stores and what is refused.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};

use crate::helpers::{jsonb_functions, run_translated::run_translated_with};

fn options() -> Pg2SqliteOptions {
    jsonb_functions::with_functions(
        Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text),
    )
}

/// A script whose trigger on `r` runs `body` for `event`, ending with `tail`.
fn script(event: &str, condition: &str, body: &str, tail: &str) -> String {
    format!(
        r#"CREATE TABLE r (id UUID PRIMARY KEY, "Label" TEXT, n INTEGER);
           CREATE TABLE audit (body JSONB, json_body JSON);
           CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
           BEGIN
               {body}
               RETURN NULL;
           END $$;
           CREATE TRIGGER t AFTER {event} ON r FOR EACH ROW {condition} EXECUTE FUNCTION f();
           {tail}"#
    )
}

fn refusal(event: &str, condition: &str, body: &str) -> String {
    Pg2Sqlite::default()
        .sql(&script(event, condition, body, ""))
        .expect("parse")
        .translate(&options())
        .expect_err("the whole row has no translation here")
        .to_string()
}

/// The stored text under the stored column names, `jsonb` in the form
/// PostgreSQL prints it and `json` with its keys in column order.
#[test]
fn a_whole_row_is_stored_as_an_object_over_its_columns() {
    let rows = run_translated_with(
        &script(
            "INSERT",
            "",
            "INSERT INTO audit (body, json_body) VALUES (to_jsonb(NEW), row_to_json(NEW));",
            "INSERT INTO r VALUES ('00010203-0405-0607-0809-0a0b0c0d0e0f', 'x', NULL);
             SELECT body::text || ' ' || json_body::text FROM audit;",
        ),
        &options(),
    );
    assert_eq!(
        rows,
        [Some(
            r#"{"n": null, "id": "00010203-0405-0607-0809-0a0b0c0d0e0f", "Label": "x"} {"id":"00010203-0405-0607-0809-0a0b0c0d0e0f","Label":"x","n":null}"#
                .to_owned()
        )]
    );
}

#[test]
fn a_whole_row_compared_with_equals_is_refused() {
    let message = refusal("UPDATE", "WHEN (OLD = NEW)", "INSERT INTO audit (body) VALUES (NULL);");
    assert!(message.contains("`OLD` reads the whole row of `r`"), "{message}");
}

/// PostgreSQL reads `OLD` in an insert trigger as `NULL`, and SQLite refuses
/// to prepare `OLD.id` inside an insert trigger, so the row is never read.
#[test]
fn a_whole_row_the_event_does_not_have_is_null() {
    let rows = run_translated_with(
        &script(
            "INSERT",
            "",
            "INSERT INTO audit (body, json_body) VALUES (to_jsonb(OLD), row_to_json(OLD));",
            "INSERT INTO r VALUES ('00010203-0405-0607-0809-0a0b0c0d0e0f', 'x', NULL);
             SELECT coalesce(body::text, 'none') || coalesce(json_body::text, 'none') FROM audit;",
        ),
        &options(),
    );
    assert_eq!(rows, [Some("nonenone".to_owned())]);
}

/// A record assigned `OLD` in one branch and `NEW` in the other reads each
/// field from the row its branch assigned, with the column's type.
#[test]
fn a_record_reads_the_fields_of_the_row_its_branch_assigned() {
    let rows = run_translated_with(
        &script(
            "INSERT OR DELETE",
            "",
            r#"DECLARE rec RECORD;
               BEGIN
                   IF TG_OP = 'DELETE' THEN rec := OLD; ELSE rec := NEW; END IF;
                   IF rec.id::text LIKE '0001%' THEN
                       INSERT INTO audit (json_body)
                           VALUES (json_build_array(TG_OP, rec.id::text, rec."Label"));
                   END IF;
               END;"#,
            "INSERT INTO r VALUES ('00010203-0405-0607-0809-0a0b0c0d0e0f', 'x', NULL);
             INSERT INTO r VALUES ('ffffffff-ffff-ffff-ffff-ffffffffffff', 'y', NULL);
             DELETE FROM r;
             SELECT json_body FROM audit;",
        ),
        &options(),
    );
    assert_eq!(
        rows,
        [
            Some(r#"["INSERT","00010203-0405-0607-0809-0a0b0c0d0e0f","x"]"#.to_owned()),
            Some(r#"["DELETE","00010203-0405-0607-0809-0a0b0c0d0e0f","x"]"#.to_owned()),
        ]
    );
}

#[test]
fn a_record_assigned_a_row_in_some_branches_only_is_refused() {
    let message = refusal(
        "UPDATE",
        "",
        "DECLARE rec RECORD;
         BEGIN
             IF NEW.n > 0 THEN rec := NEW; END IF;
             INSERT INTO audit (body) VALUES (to_jsonb(rec.n));
         END;",
    );
    assert!(message.contains("`rec` holds a row only after some branches"), "{message}");
}

/// PostgreSQL raises only when the unassigned record is read.
#[test]
fn a_record_assigned_in_some_branches_only_translates_when_unread() {
    Pg2Sqlite::default()
        .sql(&script(
            "UPDATE",
            "",
            "DECLARE rec RECORD;
             BEGIN
                 IF NEW.n > 0 THEN rec := NEW; END IF;
                 INSERT INTO audit (body) VALUES (to_jsonb(NEW.n));
             END;",
            "",
        ))
        .expect("parse")
        .translate(&options())
        .expect("no read of the record is left to refuse");
}

/// PostgreSQL keeps the row the `IF` chose. SQLite would test the condition
/// again at each field read, over the rows written since.
#[test]
fn a_record_whose_if_condition_reads_a_table_written_before_the_read_is_refused() {
    let message = refusal(
        "UPDATE",
        "",
        "DECLARE rec RECORD;
         BEGIN
             IF EXISTS (SELECT 1 FROM audit) THEN rec := NEW; ELSE rec := OLD; END IF;
             INSERT INTO audit (body) VALUES (NULL);
             INSERT INTO audit (body) VALUES (to_jsonb(rec.n));
         END;",
    );
    assert!(message.contains("`rec` reads `audit`, which the body writes"), "{message}");
}

#[test]
fn a_record_whose_nested_if_condition_reads_a_table_written_before_the_read_is_refused() {
    let message = refusal(
        "UPDATE",
        "",
        "DECLARE rec RECORD;
         BEGIN
             IF NEW.n > 0 THEN
                 IF EXISTS (SELECT 1 FROM audit) THEN rec := NEW; ELSE rec := OLD; END IF;
             ELSE
                 rec := OLD;
             END IF;
             INSERT INTO audit (body) VALUES (NULL);
             INSERT INTO audit (body) VALUES (to_jsonb(rec.n));
         END;",
    );
    assert!(message.contains("`rec` reads `audit`, which the body writes"), "{message}");
}

#[test]
fn a_whole_record_whose_if_condition_reads_a_table_written_before_the_read_is_refused() {
    let message = refusal(
        "UPDATE",
        "",
        "DECLARE rec RECORD;
         BEGIN
             IF EXISTS (SELECT 1 FROM audit) THEN rec := NEW; ELSE rec := OLD; END IF;
             INSERT INTO audit (body) VALUES (NULL);
             INSERT INTO audit (body) VALUES (to_jsonb(rec));
         END;",
    );
    assert!(message.contains("`rec` reads `audit`, which the body writes"), "{message}");
}

#[test]
fn a_record_a_nested_if_leaves_unassigned_on_one_path_is_refused() {
    let message = refusal(
        "UPDATE",
        "",
        "DECLARE rec RECORD;
         BEGIN
             IF NEW.n > 0 THEN
                 IF NEW.n > 1 THEN rec := NEW; END IF;
             ELSE
                 rec := OLD;
             END IF;
             INSERT INTO audit (body) VALUES (to_jsonb(rec.n));
         END;",
    );
    assert!(message.contains("`rec` holds a row only after some branches"), "{message}");
}

/// Read before the write, the condition sees the rows PostgreSQL's `IF` saw.
#[test]
fn a_record_whose_if_condition_reads_a_table_translates_when_read_before_the_write() {
    let rows = run_translated_with(
        &script(
            "UPDATE",
            "",
            "DECLARE rec RECORD;
             BEGIN
                 IF EXISTS (SELECT 1 FROM audit) THEN rec := NEW; ELSE rec := OLD; END IF;
                 INSERT INTO audit (body) VALUES (to_jsonb(rec.n));
             END;",
            "INSERT INTO r VALUES ('00010203-0405-0607-0809-0a0b0c0d0e0f', 'x', 1);
             UPDATE r SET n = 2;
             UPDATE r SET n = 3;
             SELECT body FROM audit;",
        ),
        &options(),
    );
    assert_eq!(rows, [Some("1".to_owned()), Some("3".to_owned())]);
}
