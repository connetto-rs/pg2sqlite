//! A set-returning SQL helper running as its caller, called in `FROM` with a
//! column of an enclosing query, translates as the rows of its body, the
//! `parity_set_returning_helpers` gauntlet checking them against PostgreSQL.
//! A body or a call whose translation would read the arguments elsewhere
//! than PostgreSQL does is refused.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::Deployment;

use crate::helpers;

const RUNS: &str = "
CREATE ROLE postgres;
CREATE ROLE app;
CREATE TABLE runs (id INTEGER PRIMARY KEY, note TEXT, measured INTEGER NOT NULL, target INTEGER NOT NULL);
CREATE TABLE links (id INTEGER PRIMARY KEY, run_id INTEGER);
GRANT SELECT, INSERT ON runs, links TO app;
";

/// A view reading the deviations of each run.
const DEVIATING: &str = "
CREATE VIEW deviating AS SELECT run.id FROM runs run
  WHERE EXISTS (SELECT 1 FROM run_deviations(run.id));
";

/// The body of `run_deviations`, which names its own `run` as the caller's
/// argument does.
const BODY: &str = "SELECT 'measured'::text FROM runs run WHERE run.id = checked_run_id AND run.measured <> run.target";

fn options() -> Pg2SqliteOptions {
    let deployment = Deployment::new("postgres", ["app"], []).expect("a valid deployment");
    Pg2SqliteOptions::default().with_deployment(deployment).with_acting_role("app")
}

/// `run_deviations` declared with `attributes` over `body`.
fn deviations(attributes: &str, body: &str) -> String {
    format!(
        "CREATE FUNCTION run_deviations(checked_run_id INTEGER) RETURNS SETOF TEXT \
         LANGUAGE sql {attributes} AS $$ {body} $$;"
    )
}

/// The refusal translating `helper` and then `caller` over [`RUNS`].
fn refusal(helper: &str, caller: &str) -> String {
    match helpers::translate_pg(&format!("{RUNS}{helper}{caller}"), &options()) {
        Ok(script) => panic!("translated where a refusal was due:\n{}", script.join("\n")),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_volatile_helper_is_refused() {
    let error = refusal(&deviations("VOLATILE", BODY), DEVIATING);
    assert!(error.contains("run_deviations()") && error.contains("is VOLATILE"), "{error}");
}

#[test]
fn a_body_of_two_statements_is_refused() {
    let error = refusal(&deviations("STABLE", "SELECT 'a'::text; SELECT 'b'::text"), DEVIATING);
    assert!(error.contains("has a body of other than one query"), "{error}");
}

/// SQLite reads the arguments only from a select's FROM list, so a body
/// reading a parameter in a LIMIT, a VALUES list or inside one of its FROM
/// items has nowhere to read it from.
#[test]
fn a_parameter_the_selects_of_the_body_cannot_reach_is_refused() {
    let limited = refusal(
        &deviations("STABLE", "SELECT 'measured'::text FROM runs LIMIT checked_run_id"),
        DEVIATING,
    );
    assert!(limited.contains("outside the SELECT lists"), "{limited}");
    let values = refusal(
        &deviations(
            "STABLE",
            "SELECT 'measured'::text FROM runs run WHERE run.id = checked_run_id \
             UNION VALUES ((SELECT run.note FROM runs run WHERE run.id = checked_run_id))",
        ),
        DEVIATING,
    );
    assert!(values.contains("outside the SELECT lists"), "{values}");
    let derived = refusal(
        &deviations(
            "STABLE",
            "SELECT kind FROM (SELECT 'measured'::text AS kind FROM runs run \
             WHERE run.id = checked_run_id) deviation",
        ),
        DEVIATING,
    );
    assert!(derived.contains("in an item of a FROM list"), "{derived}");
    let right = refusal(
        &deviations(
            "STABLE",
            "SELECT 'linked'::text FROM runs run RIGHT JOIN links link ON link.run_id = run.id \
             WHERE link.run_id = checked_run_id",
        ),
        DEVIATING,
    );
    assert!(right.contains("joining RIGHT or FULL"), "{right}");
}

/// PostgreSQL reads a call beside another FROM item `LATERAL`, written so or
/// not, so `run.id` or `id` names the `run` beside it, which a derived table
/// in SQLite cannot reach.
#[test]
fn a_call_passed_a_column_beside_another_from_item_is_refused() {
    for caller in [
        "CREATE VIEW deviations AS SELECT run.id, deviation.kind
           FROM runs run, run_deviations(run.id) AS deviation (kind);",
        "CREATE VIEW deviations AS SELECT run.id, deviation.kind
           FROM runs run, LATERAL run_deviations(run.id) AS deviation (kind);",
        "CREATE VIEW deviations AS SELECT runs.id, deviation.kind
           FROM runs, run_deviations(id) AS deviation (kind);",
    ] {
        let error = refusal(&deviations("STABLE", BODY), caller);
        assert!(
            error.contains("run_deviations()") && error.contains("LATERAL"),
            "{caller}\n{error}"
        );
    }
}

/// A `*` over a `USING` or `NATURAL` join reads one of each pair of joined
/// columns, and a `*` over a FROM item without a name has no name to read
/// it by, so neither keeps its columns once the arguments join the select.
#[test]
fn a_star_the_joined_arguments_would_change_is_refused() {
    for body in [
        "SELECT * FROM runs JOIN links USING (id) WHERE runs.id = checked_run_id",
        "SELECT * FROM runs NATURAL JOIN links WHERE runs.id = checked_run_id",
        "SELECT * FROM runs CROSS JOIN (links JOIN links other USING (run_id)) \
         WHERE runs.id = checked_run_id",
        "SELECT * FROM (SELECT run.note FROM runs run) WHERE note = checked_run_id::text",
    ] {
        let error = refusal(&deviations("STABLE", body), DEVIATING);
        assert!(error.contains("USING or NATURAL join"), "{body}\n{error}");
    }
}

/// Rows of a table's type or of a pseudo-type take the columns of a table
/// or of the caller's column list, which the inliner does not read.
#[test]
fn rows_whose_columns_the_declaration_does_not_name_are_refused() {
    for returned in ["SETOF runs", "SETOF record"] {
        let helper = format!(
            "CREATE FUNCTION run_rows(wanted INTEGER) RETURNS {returned} LANGUAGE sql STABLE \
             AS $$ SELECT * FROM runs run WHERE run.id = wanted $$;"
        );
        let error = refusal(
            &helper,
            "CREATE VIEW linked AS SELECT link.id FROM links link
               WHERE EXISTS (SELECT 1 FROM run_rows(link.run_id));",
        );
        assert!(
            error.contains("run_rows()") && error.contains("does not name"),
            "{returned}\n{error}"
        );
    }
}

/// PostgreSQL reads the FROM list of an `UPDATE` and the `USING` list of a
/// `DELETE` `LATERAL` too.
#[test]
fn a_call_passed_a_column_beside_another_item_of_a_write_is_refused() {
    for write in [
        "UPDATE links SET run_id = 0 FROM runs run, run_deviations(run.id) AS deviation (kind)
           WHERE links.run_id = run.id;",
        "DELETE FROM links USING runs run, run_deviations(run.id) AS deviation (kind)
           WHERE links.run_id = run.id;",
    ] {
        let error = refusal(&deviations("STABLE", BODY), write);
        assert!(
            error.contains("run_deviations()") && error.contains("LATERAL"),
            "{write}\n{error}"
        );
    }
}

/// A trigger's `NEW` is no item of the FROM list, so a call passed one of its
/// fields beside another item reads the row the trigger fires for.
#[test]
fn a_call_passed_a_trigger_field_beside_another_from_item_reads_the_row() {
    let source = format!(
        "{RUNS}{}
        CREATE FUNCTION check_link() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN
            IF EXISTS (SELECT 1 FROM runs run, run_deviations(NEW.run_id) AS deviation (kind)
                WHERE run.id = NEW.run_id AND run.note IS NULL) THEN
                RAISE EXCEPTION 'run % needs a note', NEW.run_id USING ERRCODE = 'check_violation';
            END IF;
            RETURN NULL;
        END;
        $$;
        CREATE TRIGGER links_check AFTER INSERT ON links FOR EACH ROW EXECUTE FUNCTION check_link();",
        deviations("STABLE", BODY)
    );
    let script = Pg2Sqlite::default()
        .sql(&source)
        .expect("parse")
        .translate_to_sql(&options())
        .unwrap_or_else(|error| panic!("the schema should translate: {error}"));
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    for statement in script.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
        .batch_execute("INSERT INTO runs VALUES (1, NULL, 5, 5), (2, NULL, 5, 6), (3, 'ok', 5, 6);")
        .expect("seed the replica");
    connection.batch_execute("INSERT INTO links VALUES (10, 1)").expect("a run without deviations");
    connection.batch_execute("INSERT INTO links VALUES (11, 3)").expect("a run with a note");
    let error = connection
        .batch_execute("INSERT INTO links VALUES (12, 2)")
        .expect_err("a deviating run without a note");
    assert!(error.to_string().contains("needs a note"), "{error}");
}

diesel::table! {
    linked_runs (id) {
        id -> Integer,
        notes -> Nullable<Text>,
        strict_count -> BigInt,
    }
}

/// A `*` over the body keeps the body's columns once the arguments join it,
/// and a `STRICT` helper passed NULL returns no row without running its body,
/// which would read every run.
#[test]
fn a_star_body_keeps_its_columns_and_a_strict_helper_skips_null() {
    let source = format!(
        "{RUNS}
        CREATE FUNCTION run_rows(wanted INTEGER)
          RETURNS TABLE (id INTEGER, note TEXT, measured INTEGER, target INTEGER)
          LANGUAGE sql STABLE AS $$ SELECT * FROM runs run WHERE run.id = wanted OR wanted IS NULL $$;
        CREATE FUNCTION strict_run_rows(wanted INTEGER)
          RETURNS TABLE (id INTEGER, note TEXT, measured INTEGER, target INTEGER)
          LANGUAGE sql STABLE STRICT AS $$ SELECT * FROM runs run WHERE run.id = wanted OR wanted IS NULL $$;
        CREATE VIEW linked_runs AS SELECT link.id,
          (SELECT string_agg(found.note, ',' ORDER BY found.id) FROM run_rows(link.run_id) AS found) AS notes,
          (SELECT count(*) FROM strict_run_rows(link.run_id)) AS strict_count
          FROM links link;
        GRANT SELECT ON linked_runs TO app;"
    );
    let script = Pg2Sqlite::default()
        .sql(&source)
        .expect("parse")
        .translate_to_sql(&options())
        .unwrap_or_else(|error| panic!("the schema should translate: {error}"));
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    for statement in script.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
        .batch_execute(
            "INSERT INTO runs VALUES (1, 'cold', 5, 5), (2, 'warm', 5, 6);
             INSERT INTO links VALUES (10, 2), (11, NULL);",
        )
        .expect("seed the replica");
    let rows: Vec<(i32, Option<String>, i64)> =
        linked_runs::table.order(linked_runs::id).load(&mut connection).expect("read linked_runs");
    assert_eq!(rows, [(10, Some("warm".to_owned()), 1), (11, Some("cold,warm".to_owned()), 0)]);
}
