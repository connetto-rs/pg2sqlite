//! A view declared `WITH (security_barrier)` evaluates its own conditions on
//! every row before any condition its caller writes, so a caller function that
//! errors on a value never sees a row the view hides. Row 2 holds the value on
//! which `abs` overflows, and only `shown` decides it stays hidden. The
//! expected rows are PostgreSQL 18's.

use crate::helpers::run_translated::run_translated_default;

const SCHEMA: &str = "CREATE TABLE items (id INT PRIMARY KEY, secret BIGINT NOT NULL);
    CREATE TABLE shown (item INT PRIMARY KEY);
    INSERT INTO items VALUES (1, 0), (2, -9223372036854775808);
    INSERT INTO shown VALUES (1);";

fn run(statements: &str) -> Vec<Option<String>> {
    run_translated_default(&format!("{SCHEMA}\n{statements}"))
}

#[test]
fn a_caller_condition_never_reads_a_row_the_view_condition_hides() {
    for option in [
        "security_barrier",
        "security_barrier = true",
        "check_option = cascaded, security_barrier = on",
    ] {
        assert_eq!(
            run(&format!(
                "CREATE VIEW fenced WITH ({option}) AS
                     SELECT id, secret FROM items WHERE EXISTS (SELECT 1 FROM shown WHERE item = id);
                 SELECT id FROM fenced WHERE abs(secret) >= 0;"
            )),
            [Some("1".to_owned())],
            "{option}"
        );
    }
}

#[test]
fn a_caller_condition_never_reads_a_row_the_view_join_hides() {
    assert_eq!(
        run("CREATE VIEW joined WITH (security_barrier = true) AS
                 SELECT i.id, i.secret FROM items i JOIN shown s ON s.item = i.id;
             SELECT id FROM joined WHERE abs(secret) >= 0;"),
        [Some("1".to_owned())]
    );
}

#[test]
fn a_write_tests_the_view_condition_before_the_caller_condition() {
    const VIEW: &str = "CREATE VIEW fenced WITH (security_barrier) AS
        SELECT id, secret FROM items WHERE EXISTS (SELECT 1 FROM shown WHERE item = id);";
    assert_eq!(
        run(&format!(
            "{VIEW}
             UPDATE fenced SET secret = 5 WHERE abs(secret) >= 0;
             SELECT id || ' ' || secret FROM items ORDER BY id;"
        )),
        [Some("1 5".to_owned()), Some("2 -9223372036854775808".to_owned())]
    );
    assert_eq!(
        run(&format!(
            "{VIEW}
             DELETE FROM fenced WHERE abs(secret) >= 0;
             SELECT id FROM items ORDER BY id;"
        )),
        [Some("2".to_owned())]
    );
}

#[test]
fn a_checked_write_tests_the_view_condition_before_the_caller_condition() {
    assert_eq!(
        run("CREATE VIEW moves WITH (check_option = cascaded, security_barrier = true) AS
                 SELECT id, secret FROM items WHERE EXISTS (SELECT 1 FROM shown WHERE item = id);
             UPDATE moves SET secret = 5 WHERE abs(secret) >= 0;
             SELECT id || ' ' || secret FROM items ORDER BY id;"),
        [Some("1 5".to_owned()), Some("2 -9223372036854775808".to_owned())]
    );
}

#[test]
fn a_view_with_the_barrier_off_reads_as_a_plain_view() {
    assert_eq!(
        run("CREATE VIEW open WITH (security_barrier = false) AS
                 SELECT id, secret FROM items WHERE EXISTS (SELECT 1 FROM shown WHERE item = id);
             SELECT id FROM open WHERE id > 0;"),
        [Some("1".to_owned())]
    );
}
