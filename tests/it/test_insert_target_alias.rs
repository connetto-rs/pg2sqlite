//! An `INSERT` target alias names the stored row and hides the table name.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::run_translated::run_translated_default;

#[test]
fn an_aliased_upsert_reads_the_target_row_through_its_alias() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
             INSERT INTO base VALUES (1, 'a', 5);
             INSERT INTO base AS x (id, name, score) VALUES (1, 'b', 6)
                 ON CONFLICT (id) DO UPDATE SET name = x.name || excluded.name
                 RETURNING x.id || ' ' || x.name;"
        ),
        [Some("1 ab".to_owned())]
    );
}

#[test]
fn an_aliased_upsert_filters_the_target_row() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
             INSERT INTO base VALUES (1, 'a', 5);
             INSERT INTO base AS x VALUES (1, 'c', 8)
                 ON CONFLICT (id) DO UPDATE SET score = excluded.score
                 WHERE x.score < excluded.score RETURNING x.score;"
        ),
        [Some("8".to_owned())]
    );
}

#[test]
fn an_aliased_upsert_preserves_a_filtered_target_row() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
             INSERT INTO base VALUES (1, 'a', 5);
             INSERT INTO base AS x VALUES (1, 'c', 4)
                 ON CONFLICT (id) DO UPDATE SET score = excluded.score
                 WHERE x.score < excluded.score;
             SELECT score FROM base;"
        ),
        [Some("5".to_owned())]
    );
}

#[test]
fn an_aliased_insert_returns_the_target_row() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
             INSERT INTO base AS x VALUES (2, 'd', 9) RETURNING x.id || ' ' || x.name;"
        ),
        [Some("2 d".to_owned())]
    );
}

#[test]
fn an_insert_alias_hides_the_table_name() {
    for suffix in [
        "ON CONFLICT (id) DO UPDATE SET name = base.name",
        "ON CONFLICT (id) DO UPDATE SET score = 10 WHERE base.score < 10",
        "RETURNING base.name",
    ] {
        let error = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
                 INSERT INTO base AS x VALUES (1, 'e', 10) {suffix};"
            ))
            .expect("parse")
            .translate_to_sql(&Pg2SqliteOptions::default())
            .expect_err("the alias hides the table name");
        assert!(error.to_string().contains("base"), "{error}");
    }
}
