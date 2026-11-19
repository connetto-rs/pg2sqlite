//! A trigger body running work once per row of a query, through `FOR target
//! IN query LOOP` or `PERFORM f(...) FROM ...`, refused where the work depends
//! on the order the rows come in.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// The check and the write a loop body calls.
const HELPERS: &str = "
CREATE FUNCTION check_val(n INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
    IF n < 0 THEN RAISE EXCEPTION 'negative %', n; END IF;
END $$;
CREATE FUNCTION log_val(n INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO t_log (val) VALUES (n);
END $$;
";

/// The refusal translating a trigger on `t` that runs `body` after
/// declaring `declarations` gives.
fn refusal(declarations: &str, body: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER);
             CREATE TABLE t_log (val INTEGER);
             {HELPERS}
             CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
             DECLARE {declarations}
             BEGIN {body} RETURN NULL; END $$;
             CREATE TRIGGER go AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();"
        ))
        .and_then(|parsed| parsed.translate_to_sql(&Pg2SqliteOptions::default()))
        .expect_err("the per-row work cannot be translated")
        .to_string()
}

/// A write per row reads what the rows before it wrote.
#[test]
fn a_write_per_row_is_refused() {
    for body in [
        "FOR v IN SELECT val FROM t LOOP PERFORM log_val(v); END LOOP;",
        "PERFORM log_val(val) FROM t;",
    ] {
        let message = refusal("v INTEGER;", body);
        assert!(message.contains("writes `t_log`") && message.contains("per row"), "{message}");
    }
}

/// A variable the loop assigns and declares outside it keeps what the last
/// row left.
#[test]
fn an_outer_variable_assigned_per_row_is_refused() {
    let message = refusal(
        "v INTEGER; total INTEGER := 0;",
        "FOR v IN SELECT val FROM t LOOP total := total + v; END LOOP;",
    );
    assert!(message.contains("`total`"), "{message}");
}

/// `EXIT`, `CONTINUE` and `RETURN` act at whichever row the loop reached.
#[test]
fn a_jump_out_of_the_loop_is_refused() {
    for (jump, keyword) in [
        ("EXIT WHEN v > 3;", "EXIT"),
        ("CONTINUE WHEN v > 3;", "CONTINUE"),
        ("IF v > 3 THEN RETURN NULL; END IF;", "RETURN"),
    ] {
        let message = refusal(
            "v INTEGER;",
            &format!("FOR v IN SELECT val FROM t LOOP {jump} PERFORM check_val(v); END LOOP;"),
        );
        assert!(message.contains(keyword) && message.contains("loop"), "{message}");
    }
}

/// After the loop, its variable holds the last row and `FOUND` whether any
/// row came.
#[test]
fn a_read_of_what_the_loop_left_is_refused() {
    for (read, name) in [
        ("IF v > 3 THEN RAISE EXCEPTION 'big'; END IF;", "`v`"),
        ("IF NOT FOUND THEN RAISE EXCEPTION 'none'; END IF;", "FOUND"),
    ] {
        let message = refusal(
            "v INTEGER;",
            &format!("FOR v IN SELECT val FROM t LOOP PERFORM check_val(v); END LOOP; {read}"),
        );
        assert!(message.contains(name), "{message}");
    }
}

/// A record target holds a whole row, read field by field.
#[test]
fn a_record_target_is_refused() {
    let message =
        refusal("r RECORD;", "FOR r IN SELECT val FROM t LOOP PERFORM check_val(r.val); END LOOP;");
    assert!(message.contains("`r`") && message.contains("row"), "{message}");
}

/// A loop inside a loop's body reads the outer loop's row from its own query.
#[test]
fn a_nested_loop_is_refused() {
    let message = refusal(
        "v INTEGER; w INTEGER;",
        "FOR v IN SELECT val FROM t LOOP
             FOR w IN SELECT val FROM t WHERE val > v LOOP PERFORM check_val(w); END LOOP;
         END LOOP;",
    );
    assert!(message.contains("inside"), "{message}");
}
