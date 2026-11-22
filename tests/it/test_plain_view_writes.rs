//! An `INSERT`, `UPDATE` or `DELETE` naming an automatically updatable view
//! writes the table the view reads, each view column standing for what it
//! selects and the view's condition narrowing the rows an update or delete
//! reaches, as PostgreSQL rewrites it. The expected rows are PostgreSQL 18's.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{run_translated::run_translated_default, translate::assert_refused};

/// A table and a view renaming, computing and filtering its columns.
const SCHEMA: &str = "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT DEFAULT 'anon', score INTEGER);
    CREATE VIEW v AS SELECT id AS key, name, score * 2 AS doubled, score FROM base WHERE score > 0;";

fn run(statements: &str) -> Vec<Option<String>> {
    run_translated_default(&format!("{SCHEMA}\n{statements}"))
}

#[test]
fn writes_through_a_view_land_on_its_table() {
    assert_eq!(
        run("INSERT INTO v (key, score) VALUES (1, 5);
             INSERT INTO v (key, score) VALUES (3, -1);
             INSERT INTO v VALUES (2, 'b');
             UPDATE v SET score = score + 1 WHERE doubled > 10;
             UPDATE v SET name = 'z' WHERE key = 3;
             DELETE FROM v WHERE name = 'anon' AND key = 1;
             SELECT id || ' ' || name || ' ' || coalesce(score::text, 'null') FROM base ORDER BY id;"),
        [Some("2 b null".to_owned()), Some("3 anon -1".to_owned())]
    );
}

#[test]
fn returning_reads_the_view_columns() {
    assert_eq!(
        run("INSERT INTO base VALUES (1, 'a', 5), (2, 'b', 6);
             UPDATE v AS x SET name = x.name || '!' WHERE x.doubled > 10
                 RETURNING key || ' ' || name || ' ' || doubled;"),
        [Some("2 b! 12".to_owned())]
    );
    assert_eq!(
        run(
            "INSERT INTO v (key, score) VALUES (8, 4) RETURNING key || ' ' || name || ' ' || doubled;"
        ),
        [Some("8 anon 8".to_owned())]
    );
}

/// `key` names the view's column inside the subquery, which declares none of
/// that name, and `name` the subquery's own.
#[test]
fn a_subquery_reaches_the_view_columns_it_does_not_shadow() {
    assert_eq!(
        run("CREATE TABLE gone (k INTEGER, name TEXT);
             INSERT INTO base VALUES (1, 'a', 5), (2, 'b', 6), (3, 'c', -2);
             INSERT INTO gone VALUES (2, 'other'), (3, 'x');
             DELETE FROM v WHERE EXISTS (SELECT 1 FROM gone WHERE gone.k = key AND name <> 'b')
                 RETURNING 'gone ' || key;"),
        [Some("gone 2".to_owned())]
    );
}

#[test]
fn an_upsert_through_a_view_reads_its_columns_on_both_rows() {
    assert_eq!(
        run("INSERT INTO base VALUES (1, 'a', 5);
             INSERT INTO v (key, name, score) VALUES (1, 'n', 9)
                 ON CONFLICT (key) DO UPDATE SET name = excluded.name, score = v.score + excluded.doubled
                 RETURNING key || ' ' || name || ' ' || score;"),
        [Some("1 n 23".to_owned())]
    );
}

#[test]
fn a_view_over_a_view_writes_the_innermost_table() {
    assert_eq!(
        run("CREATE VIEW vv AS SELECT key AS k, score AS s FROM v WHERE key < 10;
             INSERT INTO vv VALUES (1, 2), (11, 3);
             UPDATE vv SET s = s * 10 WHERE k > 0;
             DELETE FROM vv WHERE s = 30;
             SELECT id || ' ' || score FROM base ORDER BY id;"),
        [Some("1 20".to_owned()), Some("11 3".to_owned())]
    );
}

#[test]
fn a_trigger_writing_a_view_writes_its_table() {
    assert_eq!(
        run("CREATE TABLE src (x INTEGER);
             CREATE FUNCTION g() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 INSERT INTO v (key, score) VALUES (NEW.x, 3);
                 RETURN NEW;
             END $$;
             CREATE TRIGGER t AFTER INSERT ON src FOR EACH ROW EXECUTE FUNCTION g();
             INSERT INTO src VALUES (4);
             SELECT id || ' ' || name || ' ' || score FROM base;"),
        [Some("4 anon 3".to_owned())]
    );
}

/// PostgreSQL refuses every row written into a view column that computes its
/// value.
#[test]
fn a_write_into_a_computed_view_column_is_refused() {
    assert_refused(
        &format!("{SCHEMA} INSERT INTO v (key, doubled) VALUES (9, 1);"),
        "cannot insert into column doubled of view v",
    );
    assert_refused(
        &format!("{SCHEMA} UPDATE v SET doubled = 1;"),
        "cannot update column doubled of view v",
    );
}

/// An `INSTEAD OF` trigger takes the write in place of the rewrite.
#[test]
fn an_instead_of_trigger_keeps_the_write_on_the_view() {
    assert_eq!(
        run("CREATE FUNCTION redirect() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 INSERT INTO base (id, name, score) VALUES (100, 'trigger', 7);
                 RETURN NEW;
             END $$;
             CREATE TRIGGER redirect INSTEAD OF INSERT ON v
                 FOR EACH ROW EXECUTE FUNCTION redirect();
             INSERT INTO v (key, score) VALUES (1, 5);
             SELECT id || ' ' || name || ' ' || score FROM base;"),
        [Some("100 trigger 7".to_owned())]
    );
}

/// An `INSTEAD OF` trigger of the schema a later script is translated against
/// takes that script's write too.
#[test]
fn an_instead_of_trigger_of_the_prior_schema_keeps_the_write_on_the_view() {
    let schema = Pg2Sqlite::default()
        .sql(&format!(
            "{SCHEMA}
             CREATE FUNCTION redirect() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 INSERT INTO base (id, name, score) VALUES (100, 'trigger', 7);
                 RETURN NEW;
             END $$;
             CREATE TRIGGER redirect INSTEAD OF INSERT ON v
                 FOR EACH ROW EXECUTE FUNCTION redirect();"
        ))
        .expect("parse")
        .build_schema()
        .expect("schema");
    let emitted = Pg2Sqlite::default()
        .sql("INSERT INTO v (key, score) VALUES (1, 5);")
        .expect("parse")
        .translate_to_sql_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    assert!(
        emitted.iter().any(|statement| statement.starts_with("INSERT INTO v ")),
        "the write should stay on the view for its trigger: {emitted:?}"
    );
}

/// A view over a temporary table writes that table, which PostgreSQL stores
/// in `pg_temp`.
#[test]
fn a_view_over_a_temporary_table_writes_the_temporary_table() {
    assert_eq!(
        run_translated_default(
            "CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
             CREATE TEMP VIEW chosen AS SELECT id AS key, tag FROM scratch WHERE id > 0;
             INSERT INTO chosen (key, tag) VALUES (4, 't');
             UPDATE chosen SET tag = 'u' WHERE key = 4;
             SELECT id || ' ' || tag FROM scratch;"
        ),
        [Some("4 u".to_owned())]
    );
}
