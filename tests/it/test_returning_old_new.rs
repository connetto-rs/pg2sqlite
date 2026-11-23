//! Row versions in `RETURNING` expressions.

use crate::helpers::{run_translated::run_translated_default, translate::assert_refused};

/// `UPDATE RETURNING` refuses the unavailable pre-update row.
#[test]
fn returning_old_new_update() {
    assert_refused(
        "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
         INSERT INTO base VALUES (2, 'b', 6);
         UPDATE base SET score = 7 WHERE id = 2
         RETURNING old.score || ' ' || new.score || ' ' || old.name;",
        "SQLite's RETURNING sees only the row after the write",
    );
}

/// Plain `INSERT` returns the inserted row.
#[test]
fn returning_old_new_insert_new() {
    let rows = run_translated_default(
        "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
         INSERT INTO base VALUES (2, 'b', 6) RETURNING new.score;",
    );
    assert_eq!(rows, vec![Some("6".to_owned())]);
}

/// `UPDATE` returns the updated row through `new`.
#[test]
fn returning_old_new_update_new() {
    let rows = run_translated_default(
        "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
         INSERT INTO base VALUES (2, 'b', 6);
         UPDATE base SET score = 7 WHERE id = 2 RETURNING new.score;",
    );
    assert_eq!(rows, vec![Some("7".to_owned())]);
}

/// `DELETE` returns the deleted row through `old`.
#[test]
fn returning_old_new_delete_old() {
    let rows = run_translated_default(
        "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
         INSERT INTO base VALUES (2, 'b', 6);
         DELETE FROM base WHERE id = 2 RETURNING old.score;",
    );
    assert_eq!(rows, vec![Some("6".to_owned())]);
}

/// Plain `INSERT` has no old row.
#[test]
fn returning_old_new_insert_old() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
             INSERT INTO base VALUES (2, 'b', 6) RETURNING old.score;"
        ),
        vec![None],
    );
}

/// `DELETE` has no new row.
#[test]
fn returning_old_new_delete_new() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
             INSERT INTO base VALUES (2, 'b', 6);
             DELETE FROM base RETURNING new.score;"
        ),
        vec![None],
    );
}

/// An upsert returns the written row and refuses the pre-conflict row.
#[test]
fn returning_old_new_upsert() {
    let setup = "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
                 INSERT INTO base VALUES (2, 'b', 6);";
    for (identifier, expected) in [(2, "9"), (3, "9")] {
        let statement = format!(
            "{setup} INSERT INTO base VALUES ({identifier}, 'b', 9)
             ON CONFLICT (id) DO UPDATE SET score = excluded.score RETURNING new.score;"
        );
        assert_eq!(run_translated_default(&statement), vec![Some(expected.to_owned())]);
    }
    for returned in ["old.score", "old.*", "old"] {
        assert_refused(
            &format!(
                "{setup} INSERT INTO base VALUES (2, 'b', 9)
                ON CONFLICT (id) DO UPDATE SET score = excluded.score RETURNING {returned};"
            ),
            "SQLite's RETURNING sees only the row after the write",
        );
    }
}

/// Pre-update row references are refused across expression shapes.
#[test]
fn returning_old_new_update_shapes() {
    for returned in
        ["old.*", "old", "(SELECT old.score)", "coalesce(old.score, 0)", "row_to_json(old.*)"]
    {
        assert_refused(
            &format!(
                "CREATE TABLE base (id INTEGER PRIMARY KEY, score INTEGER);
                UPDATE base SET score = 7 RETURNING {returned};"
            ),
            "SQLite's RETURNING sees only the row after the write",
        );
    }
}

/// Row version names remain ordinary declared column names.
#[test]
fn returning_old_new_column_names() {
    for name in ["old", "new"] {
        assert_eq!(
            run_translated_default(&format!(
                "CREATE TABLE base (id INTEGER PRIMARY KEY, old INTEGER, new INTEGER);
                 INSERT INTO base VALUES (2, 70, 80);
                 UPDATE base SET id = 3 RETURNING {name};"
            )),
            vec![Some(if name == "old" { "70" } else { "80" }.to_owned())],
        );
    }
}

/// A nested relation named `old` shadows the old row.
#[test]
fn returning_old_new_nested_binding() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE base (id INTEGER PRIMARY KEY, score INTEGER);
             CREATE TABLE other (id INTEGER PRIMARY KEY, score INTEGER);
             INSERT INTO base VALUES (2, 6);
             INSERT INTO other VALUES (2, 33);
             UPDATE base SET score = 9 RETURNING (SELECT old.score FROM other AS old);"
        ),
        vec![Some("33".to_owned())],
    );
}

/// A correlated query can return the new row's column.
#[test]
fn returning_old_new_correlated() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE base (id INTEGER PRIMARY KEY, score INTEGER);
             INSERT INTO base VALUES (2, 6);
             UPDATE base SET score = 8 RETURNING (SELECT new.score);"
        ),
        vec![Some("8".to_owned())],
    );
}

/// Qualified row stars preserve each version's columns.
#[test]
fn returning_old_new_stars() {
    for (statement, expected) in [
        ("INSERT INTO base VALUES (2, 'b', 6) RETURNING new.*", Some("2")),
        ("INSERT INTO base VALUES (2, 'b', 6) RETURNING old.*", None),
        ("UPDATE base SET score = 8 RETURNING new.*", Some("1")),
        ("DELETE FROM base RETURNING old.*", Some("1")),
        ("DELETE FROM base RETURNING new.*", None),
    ] {
        let rows = run_translated_default(&format!(
            "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
             INSERT INTO base VALUES (1, 'a', 5); {statement};"
        ));
        assert_eq!(rows, vec![expected.map(str::to_owned)]);
    }
}

/// Nested stars cannot expose an unavailable old row.
#[test]
fn returning_old_new_nested_star() {
    assert_refused(
        "CREATE TABLE base (score INTEGER);
         UPDATE base SET score = 7 RETURNING (SELECT old.*);",
        "SQLite's RETURNING sees only the row after the write",
    );
}

/// Correlated row versions must not bind an inner relation with the target's
/// name.
#[test]
fn returning_old_new_shadowed_target() {
    assert_refused(
        "CREATE TABLE base (id INTEGER PRIMARY KEY, score INTEGER);
         INSERT INTO base VALUES (2, 6);
         UPDATE base SET score = 8
         RETURNING (SELECT new.score FROM (VALUES (99)) AS base(score));",
        "shadows the target",
    );
}

/// A nested star expands the available or absent row.
#[test]
fn returning_old_new_nested_available_stars() {
    for (statement, expected) in [
        ("UPDATE base SET score = 8 RETURNING (SELECT new.*)", Some("8")),
        ("DELETE FROM base RETURNING (SELECT new.*)", None),
    ] {
        assert_eq!(
            run_translated_default(&format!(
                "CREATE TABLE base (score INTEGER); INSERT INTO base VALUES (6); {statement};"
            )),
            vec![expected.map(str::to_owned)],
        );
    }
}

/// A row version names the target column beside an auxiliary column of the same
/// name.
#[test]
fn returning_old_new_auxiliary_columns() {
    let setup = "CREATE TABLE base (id INTEGER PRIMARY KEY, score INTEGER);
                 CREATE TABLE other (id INTEGER PRIMARY KEY, score INTEGER);
                 INSERT INTO base VALUES (2, 6); INSERT INTO other VALUES (2, 33);";
    for statement in [
        "UPDATE base AS target SET score = 8 FROM other WHERE target.id = other.id RETURNING new.score",
        "DELETE FROM base AS target USING other WHERE target.id = other.id RETURNING old.score",
    ] {
        let expected = if statement.starts_with("UPDATE") { "8" } else { "6" };
        assert_eq!(
            run_translated_default(&format!("{setup} {statement};")),
            vec![Some(expected.to_owned())],
        );
    }
}

/// A row version star includes only the target beside auxiliary relations.
#[test]
fn returning_old_new_auxiliary_stars() {
    let setup = "CREATE TABLE base (id INTEGER PRIMARY KEY, score INTEGER);
                 CREATE TABLE other (id INTEGER PRIMARY KEY, score INTEGER);
                 INSERT INTO base VALUES (2, 6); INSERT INTO other VALUES (2, 33);";
    for statement in [
        "UPDATE base AS target SET score = 8 FROM other WHERE target.id = other.id RETURNING new.*",
        "DELETE FROM base AS target USING other WHERE target.id = other.id RETURNING old.*",
    ] {
        assert_eq!(
            run_translated_default(&format!("{setup} {statement};")),
            vec![Some("2".to_owned())],
        );
    }
}

/// Nested columns named `old` and `new` retain their local bindings.
#[test]
fn returning_old_new_nested_column_names() {
    for (name, expected) in [("old", "70"), ("new", "80")] {
        assert_eq!(
            run_translated_default(&format!(
                "CREATE TABLE base (score INTEGER);
                 CREATE TABLE other (old INTEGER, new INTEGER);
                 INSERT INTO base VALUES (6); INSERT INTO other VALUES (70, 80);
                 UPDATE base SET score = 7 RETURNING (SELECT {name} FROM other);"
            )),
            vec![Some(expected.to_owned())],
        );
    }
}
