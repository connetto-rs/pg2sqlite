//! An `INSERT` without a column list fills the table's first columns, as many
//! as its rows hold, and PostgreSQL gives every column after them its default.
//! The expected rows are PostgreSQL 18's.

use crate::helpers::run_translated::run_translated_default;

/// A table whose first column is quoted and whose second has a default.
const TABLE: &str = r#"CREATE TABLE a ("X" INTEGER, y TEXT DEFAULT 'd', z INTEGER);"#;

/// What `a` holds, one row per line in the order of `"X"` and `y`.
const ROWS: &str = r#"SELECT "X" || ' ' || coalesce(y, 'null') || ' ' || coalesce(z::text, 'null')
    FROM a ORDER BY "X", y;"#;

fn rows(inserts: &str) -> Vec<Option<String>> {
    run_translated_default(&format!("{TABLE}\n{inserts}\n{ROWS}"))
}

#[test]
fn a_short_row_fills_the_first_columns_and_defaults_the_rest() {
    assert_eq!(
        rows(
            "INSERT INTO a VALUES (1);
             INSERT INTO a VALUES (3, DEFAULT);
             INSERT INTO a VALUES (4, 'v'), (5, 'w');"
        ),
        [
            Some("1 d null".to_owned()),
            Some("3 d null".to_owned()),
            Some("4 v null".to_owned()),
            Some("5 w null".to_owned())
        ]
    );
}

#[test]
fn a_short_query_fills_the_first_columns_and_defaults_the_rest() {
    assert_eq!(
        rows(
            "INSERT INTO a SELECT 2;
             INSERT INTO a SELECT 6 UNION ALL SELECT 7;"
        ),
        [Some("2 d null".to_owned()), Some("6 d null".to_owned()), Some("7 d null".to_owned())]
    );
}

/// A wildcard counts the columns of the relations it reads, a `USING` column
/// once.
#[test]
fn a_short_wildcard_fills_the_columns_its_relations_hold() {
    assert_eq!(
        rows(
            "CREATE TABLE s (k INTEGER);
             CREATE TABLE s2 (k INTEGER, v TEXT);
             INSERT INTO s VALUES (11);
             INSERT INTO s2 VALUES (11, 'j');
             INSERT INTO a SELECT * FROM s;
             INSERT INTO a SELECT t.*, 'q' FROM (SELECT k + 2 AS m FROM s) AS t;
             WITH c AS (SELECT 14 AS k) INSERT INTO a SELECT * FROM c;
             INSERT INTO a SELECT * FROM s JOIN s2 USING (k);"
        ),
        [
            Some("11 d null".to_owned()),
            Some("11 j null".to_owned()),
            Some("13 q null".to_owned()),
            Some("14 d null".to_owned())
        ]
    );
}

/// A wildcard over a relation of unknown columns leaves the insert as written,
/// which SQLite runs when the widths agree.
#[test]
fn a_wildcard_of_unknown_width_is_inserted_as_written() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE s (k INTEGER);
             CREATE TABLE one (n INTEGER);
             INSERT INTO s VALUES (1);
             INSERT INTO one SELECT * FROM (SELECT k + 2 FROM s) AS t;
             SELECT n FROM one;"
        ),
        [Some("3".to_owned())]
    );
}

#[test]
fn a_short_row_returns_the_defaults_it_was_given() {
    assert_eq!(
        run_translated_default(&format!("{TABLE}\nINSERT INTO a VALUES (8) RETURNING y;")),
        [Some("d".to_owned())]
    );
}

#[test]
fn a_short_row_meeting_a_conflict_updates_the_existing_row() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE b (id INTEGER PRIMARY KEY, n INTEGER DEFAULT 0);
             INSERT INTO b VALUES (1) ON CONFLICT (id) DO UPDATE SET n = b.n + 1;
             INSERT INTO b VALUES (1) ON CONFLICT (id) DO UPDATE SET n = b.n + 1;
             SELECT id || ' ' || n FROM b;"
        ),
        [Some("1 1".to_owned())]
    );
}

#[test]
fn a_short_row_a_trigger_writes_defaults_the_rest() {
    assert_eq!(
        rows(
            "CREATE TABLE w (k INTEGER);
             CREATE FUNCTION wf() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN INSERT INTO a VALUES (NEW.k * 10); RETURN NULL; END $$;
             CREATE TRIGGER wt AFTER INSERT ON w FOR EACH ROW EXECUTE FUNCTION wf();
             INSERT INTO w VALUES (9);"
        ),
        [Some("90 d null".to_owned())]
    );
}
