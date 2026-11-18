//! `PERFORM` and locking `SELECT ... INTO` in a trigger body, read from the
//! body's statement tree.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    warnings::TranslationWarning,
};

const TABLE: &str = "CREATE TABLE r (id UUID PRIMARY KEY, parent UUID);";

/// The trigger statement the translation emits.
fn trigger(statements: &[String]) -> &str {
    statements
        .iter()
        .find(|statement| statement.starts_with("CREATE TRIGGER"))
        .expect("a trigger is emitted")
}

fn translate_body(
    declare: &str,
    body: &str,
) -> Result<(Vec<String>, Vec<TranslationWarning>), Error> {
    translate_body_over(TABLE, declare, body)
}

/// Translates a trigger on `r` running `body`, with `schema` declaring `r` and
/// whatever else the body reads.
fn translate_body_over(
    schema: &str,
    declare: &str,
    body: &str,
) -> Result<(Vec<String>, Vec<TranslationWarning>), Error> {
    let report = Pg2Sqlite::default()
        .sql(&format!(
            "{schema}
            CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$ {declare} BEGIN {body} RETURN NULL; END $$;
            CREATE TRIGGER t AFTER INSERT ON r FOR EACH ROW EXECUTE FUNCTION f();"
        ))?
        .translate_with_report(
            &Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob),
        )?;
    Ok((report.script.iter().map(ToString::to_string).collect(), report.warnings))
}

#[test]
fn perform_runs_its_query() {
    let (statements, _) =
        translate_body("", "PERFORM 1 FROM r WHERE id = NEW.parent;").expect("PERFORM translates");
    assert!(
        trigger(&statements).contains("SELECT 1 FROM r WHERE id = NEW.parent"),
        "{statements:?}"
    );
}

#[test]
fn perform_of_a_call_runs_the_call() {
    let (statements, _) = translate_body("", "PERFORM lower('A');").expect("PERFORM translates");
    assert!(trigger(&statements).contains("SELECT lower('A')"), "{statements:?}");
}

#[test]
fn a_locking_select_into_reads_without_the_lock_and_says_why() {
    for lock in [
        "FOR UPDATE",
        "FOR NO KEY UPDATE",
        "FOR SHARE",
        "FOR KEY SHARE",
        "FOR UPDATE NOWAIT",
        "FOR SHARE SKIP LOCKED",
        "FOR NO KEY UPDATE OF r",
    ] {
        let (statements, warnings) = translate_body(
            "DECLARE x UUID;",
            &format!(
                "SELECT id INTO x FROM r WHERE id = NEW.parent {lock}; \
                 UPDATE r SET parent = x WHERE id = NEW.id;"
            ),
        )
        .unwrap_or_else(|error| panic!("{lock}: {error}"));
        let trigger = trigger(&statements);
        let (_, body) = trigger.split_once(" BEGIN ").expect("the trigger has a body");
        assert!(!body.contains(" FOR "), "{lock}: {trigger}");
        assert!(
            warnings.iter().any(|warning| matches!(
                warning,
                TranslationWarning::LossyDrop { construct, .. } if construct == "FOR UPDATE / FOR SHARE"
            )),
            "{lock}: {warnings:?}"
        );
    }
}

/// `FOUND` after a write reports whether it touched a row, which the emitted
/// trigger does not track.
#[test]
fn found_after_a_write_is_refused() {
    let error = translate_body(
        "",
        "PERFORM 1 FROM r WHERE id = NEW.parent; \
         UPDATE r SET parent = NULL WHERE id = NEW.id; \
         IF NOT FOUND THEN RAISE EXCEPTION 'gone'; END IF;",
    )
    .expect_err("FOUND after UPDATE is refused");
    assert!(error.to_string().contains("FOUND after INSERT, UPDATE or DELETE"), "{error}");
}

/// The emitted trigger tests an `IF` condition again on each statement it
/// guards, so the upsert idiom would see its own `INSERT` and run the `ELSE`
/// branch too.
#[test]
fn a_condition_written_under_its_own_if_is_refused() {
    let error = translate_body(
        "",
        "PERFORM 1 FROM r WHERE id = NEW.parent; \
         IF NOT FOUND THEN INSERT INTO r (id) VALUES (NEW.parent); \
         ELSE UPDATE r SET parent = NULL WHERE id = NEW.parent; END IF;",
    )
    .expect_err("the ELSE guard reads what the THEN branch wrote");
    assert!(error.to_string().contains("an IF condition reads `r`"), "{error}");

    translate_body(
        "",
        "PERFORM 1 FROM r WHERE id = NEW.parent; \
         IF NOT FOUND THEN INSERT INTO r (id) VALUES (NEW.parent); END IF;",
    )
    .expect("a write that is the last statement its guard covers translates");
}

/// A binding that reads one a write made unreadable fails only where it is
/// read.
#[test]
fn a_binding_over_an_unreadable_one_fails_only_its_readers() {
    translate_body(
        "DECLARE b UUID; nb UUID;",
        "SELECT parent INTO b FROM r WHERE id = NEW.id; nb := b; \
         UPDATE r SET parent = nb WHERE id = NEW.id; \
         UPDATE r SET parent = NULL WHERE id = NEW.parent;",
    )
    .expect("the last UPDATE reads neither b nor nb");

    let error = translate_body(
        "DECLARE b UUID; nb UUID;",
        "SELECT parent INTO b FROM r WHERE id = NEW.id; nb := b; \
         UPDATE r SET parent = nb WHERE id = NEW.id; \
         UPDATE r SET parent = nb WHERE id = NEW.parent;",
    )
    .expect_err("nb reads b, which reads the updated table");
    assert!(error.to_string().contains("`b` reads `r`"), "{error}");
}

/// A nested block's `found` shadows PostgreSQL's only inside that block.
#[test]
fn found_declared_in_a_nested_block_is_refused() {
    let error = translate_body(
        "",
        "PERFORM 1 FROM r WHERE id = NEW.parent; \
         DECLARE found int; BEGIN found := 1; END; \
         IF FOUND THEN UPDATE r SET parent = NULL WHERE id = NEW.id; END IF;",
    )
    .expect_err("which found the IF reads is not tracked");
    assert!(error.to_string().contains("a nested block declares `found`"), "{error}");
}

/// A write to a table invalidates a variable read through a view over it.
#[test]
fn a_variable_read_through_a_view_is_stale_after_a_write_to_its_table() {
    let error = translate_body_over(
        &format!("{TABLE} CREATE VIEW rv AS SELECT id, parent FROM r;"),
        "DECLARE x UUID;",
        "SELECT parent INTO x FROM rv WHERE id = NEW.id; \
         UPDATE r SET parent = NULL WHERE id = NEW.id; \
         UPDATE r SET parent = x WHERE id = NEW.parent;",
    )
    .expect_err("x reads r through rv");
    assert!(error.to_string().contains("`x` reads `r`"), "{error}");
}

/// PostgreSQL rejects a bare `SELECT` and the set-returning `RETURN` forms in
/// a trigger function, so translating them as `PERFORM` would accept a body
/// that never runs.
#[test]
fn forms_postgres_rejects_in_a_trigger_body_are_refused() {
    for (body, named) in [
        ("SELECT 1 FROM r WHERE id = NEW.parent;", "without INTO"),
        ("RETURN NEXT NEW;", "RETURN NEXT"),
        ("RETURN NEXT;", "RETURN NEXT"),
        ("RETURN QUERY SELECT * FROM r;", "RETURN QUERY"),
        ("RETURN QUERY EXECUTE 'SELECT * FROM r';", "RETURN QUERY"),
    ] {
        let error = translate_body("", body).expect_err(body);
        assert!(error.to_string().contains(named), "{body}: {error}");
    }
}
