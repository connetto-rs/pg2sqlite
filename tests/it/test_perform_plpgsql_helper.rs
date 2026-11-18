//! `PERFORM` of a PL/pgSQL helper in a trigger body, refused where the
//! helper's body cannot stand in for the call.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::deployment::{UTC_REQUEST_ROLE, deployment};

/// The refusal translating a trigger on `t` that runs `body` gives, with
/// `helpers` declared beside it.
fn refusal(helpers: &str, body: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER);
             {helpers}
             CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN {body} RETURN NULL; END $$;
             CREATE TRIGGER go AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();"
        ))
        .and_then(|parsed| parsed.translate_to_sql(&Pg2SqliteOptions::default()))
        .expect_err("the call cannot be inlined")
        .to_string()
}

/// A helper reaching itself would be inlined without end.
#[test]
fn a_recursive_helper_is_refused() {
    let message = refusal(
        "CREATE FUNCTION down(n INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
         BEGIN
             IF n > 0 THEN PERFORM down(n - 1); END IF;
         END $$;",
        "PERFORM down(NEW.val);",
    );
    assert!(message.contains("down()") && message.contains("calls itself"), "{message}");
}

/// `PERFORM f(x) FROM t` calls the helper once per row, which one inlined
/// body does not.
#[test]
fn a_helper_performed_per_row_is_refused() {
    let message = refusal(
        "CREATE FUNCTION check_val(n INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
         BEGIN
             IF n < 0 THEN RAISE EXCEPTION 'negative'; END IF;
         END $$;",
        "PERFORM check_val(val) FROM t;",
    );
    assert!(message.contains("check_val()") && message.contains("once per row"), "{message}");
}

/// A definer trigger calls the helper as the trigger's owner, and an owner
/// without `USAGE` on the helper's schema makes PostgreSQL raise at the call.
#[test]
fn a_helper_the_trigger_owner_may_not_reach_is_refused() {
    let source = format!(
        "{UTC_REQUEST_ROLE}
         CREATE ROLE trigger_owner NOLOGIN;
         CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER);
         CREATE SCHEMA checks;
         CREATE FUNCTION checks.check_val(n INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
         BEGIN
             IF n < 0 THEN RAISE EXCEPTION 'negative'; END IF;
         END $$;
         GRANT USAGE ON SCHEMA checks TO app;
         GRANT SELECT, INSERT ON t TO app;
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN PERFORM checks.check_val(NEW.val); RETURN NULL; END $$;
         ALTER FUNCTION f() OWNER TO trigger_owner;
         CREATE TRIGGER go AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();"
    );
    let message = Pg2Sqlite::default()
        .sql(&source)
        .and_then(|parsed| {
            parsed.translate_to_sql(
                &Pg2SqliteOptions::default()
                    .with_deployment(deployment([], []))
                    .with_acting_role("app"),
            )
        })
        .expect_err("trigger_owner cannot reach checks")
        .to_string();
    assert!(
        message.contains("checks.check_val()")
            && message.contains("trigger_owner")
            && message.contains("USAGE"),
        "{message}"
    );
}
