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

/// An `INSTEAD OF` trigger created after a trigger writing the view takes
/// that trigger's write too.
#[test]
fn a_later_instead_of_trigger_takes_an_earlier_triggers_write() {
    assert_eq!(
        run("CREATE TABLE src (x INTEGER);
             CREATE FUNCTION g() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 INSERT INTO v (key, score) VALUES (NEW.x, 3);
                 RETURN NEW;
             END $$;
             CREATE TRIGGER t AFTER INSERT ON src FOR EACH ROW EXECUTE FUNCTION g();
             CREATE FUNCTION redirect() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 INSERT INTO base (id, name, score) VALUES (100, 'trigger', 7);
                 RETURN NEW;
             END $$;
             CREATE TRIGGER redirect INSTEAD OF INSERT ON v
                 FOR EACH ROW EXECUTE FUNCTION redirect();
             INSERT INTO src VALUES (4);
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

/// An aliased upsert through the view writes the table's row.
#[test]
fn an_aliased_upsert_through_the_view_writes_the_table() {
    assert_eq!(
        run("INSERT INTO base VALUES (1, 'a', 5);
             INSERT INTO v AS x (key, name, score) VALUES (1, 'b', 6), (2, 'c', 7)
                 ON CONFLICT (key) DO UPDATE SET name = excluded.name;
             SELECT id || ' ' || name || ' ' || score FROM base ORDER BY id;"),
        [Some("1 b 5".to_owned()), Some("2 c 7".to_owned())]
    );
}

/// A row assignment through a schema-qualified view over a schema-qualified
/// table writes each column it names.
#[test]
fn a_row_assignment_through_qualified_names_writes_the_table() {
    assert_eq!(
        run("CREATE VIEW pv AS SELECT id AS key, name, score FROM public.base WHERE score > 0;
             INSERT INTO base VALUES (1, 'a', 5);
             UPDATE public.pv SET (name, score) = ('t', 3) WHERE key = 1;
             SELECT id || ' ' || name || ' ' || score FROM base;"),
        [Some("1 t 3".to_owned())]
    );
}

/// A view selecting every column of a quoted table writes that table.
#[test]
fn a_wildcard_view_over_a_quoted_table_writes_it() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE \"Base\" (id INTEGER PRIMARY KEY, tag TEXT);
             CREATE VIEW sv AS SELECT * FROM \"Base\";
             INSERT INTO sv VALUES (1, 'x'), (2, 'y');
             DELETE FROM sv WHERE id = 2;
             SELECT id || tag FROM \"Base\";"
        ),
        [Some("1x".to_owned())]
    );
}

/// A view column computed by a subquery keeps the subquery's own columns.
#[test]
fn a_subquery_column_of_the_view_reads_its_own_relation() {
    assert_eq!(
        run("CREATE TABLE lim (m INTEGER);
             CREATE VIEW cv AS SELECT id, score, (SELECT max(m) FROM lim) AS cap FROM base;
             INSERT INTO lim VALUES (9);
             INSERT INTO cv (id, score) VALUES (1, 4) RETURNING cap;"),
        [Some("9".to_owned())]
    );
}

/// PostgreSQL refuses a write to a column the view does not have.
#[test]
fn a_write_into_a_missing_view_column_is_refused() {
    assert_refused(&format!("{SCHEMA} UPDATE v SET nope = 1;"), "nope");
}

#[test]
fn a_parenthesized_view_definition_writes_its_table() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE coverage_parenthesized_base (id INTEGER PRIMARY KEY, score INTEGER);
             CREATE VIEW coverage_parenthesized_view AS (SELECT id, score FROM coverage_parenthesized_base);
             INSERT INTO coverage_parenthesized_view VALUES (1, 2);
             UPDATE coverage_parenthesized_view SET score = 3 WHERE id = 1;
             DELETE FROM coverage_parenthesized_view WHERE id = 2;
             SELECT id || ' ' || score FROM coverage_parenthesized_base;"
        ),
        [Some("1 3".to_owned())]
    );
}

#[test]
fn an_ambiguous_reference_in_a_view_write_is_refused() {
    assert_refused(
        &format!(
            "{SCHEMA} UPDATE v SET score = (SELECT id FROM base first_base, base second_base);"
        ),
        "ambiguous",
    );
}

#[test]
fn a_wildcard_view_writes_a_table_in_another_schema() {
    assert_eq!(
        run_translated_default(
            "CREATE SCHEMA coverage_schema;
             CREATE TABLE coverage_schema.base (id INTEGER PRIMARY KEY, score INTEGER);
             CREATE VIEW v AS SELECT * FROM coverage_schema.base;
             INSERT INTO v VALUES (1, 2);
             UPDATE v SET score = 4;
             SELECT id || ' ' || score FROM coverage_schema.base;"
        ),
        [Some("1 4".to_owned())]
    );
}

#[test]
fn conflicts_through_a_view_can_skip_the_write() {
    assert_eq!(
        run("INSERT INTO base VALUES (1, 'a', 5);
             INSERT INTO v (key, score) VALUES (1, 9) ON CONFLICT DO NOTHING;
             INSERT INTO v (key, score) VALUES (1, 9) ON CONFLICT (key) DO NOTHING;
             SELECT id || ' ' || score FROM base;"),
        [Some("1 5".to_owned())]
    );
}

#[test]
fn conflict_skipping_and_a_quoted_view_alias_write_the_table() {
    assert_eq!(
        run("INSERT INTO base VALUES (1, 'a', 5);
             INSERT INTO v (key, score) VALUES (1, 9) ON CONFLICT DO NOTHING;
             INSERT INTO v (key, score) VALUES (1, 9) ON CONFLICT (key) DO NOTHING;
             INSERT INTO v AS \"Chosen\" (key, score) VALUES (1, 9)
                 ON CONFLICT (key) DO UPDATE SET score = \"Chosen\".score + excluded.score;
             SELECT id || ' ' || score FROM base;"),
        [Some("1 14".to_owned())]
    );
}

#[test]
fn returning_new_reads_the_view_column() {
    assert_eq!(
        run("INSERT INTO v (key, score) VALUES (2, 7) RETURNING new.key;"),
        [Some("2".to_owned())]
    );
}

#[test]
fn an_unquoted_view_alias_reads_the_conflicting_row() {
    assert_eq!(
        run("INSERT INTO base VALUES (1, 'a', 5);
             INSERT INTO v AS chosen (key, score) VALUES (1, 9)
                 ON CONFLICT (key) DO UPDATE SET score = chosen.score + excluded.score;
             SELECT id || ' ' || score FROM base;"),
        [Some("1 14".to_owned())]
    );
}

#[test]
fn returning_an_absent_view_row_version_is_null() {
    assert_eq!(run("INSERT INTO v (key, score) VALUES (2, 7) RETURNING old.key;"), [None]);
    assert_eq!(
        run("INSERT INTO base VALUES (1, 'a', 5);
             DELETE FROM v WHERE key = 1 RETURNING new.key;"),
        [None]
    );
}

#[test]
fn returning_current_view_versions_reads_computed_columns() {
    assert_eq!(
        run("INSERT INTO base VALUES (2, 'a', 7);
             UPDATE v SET score = 8 WHERE key = 2 RETURNING new.key || ' ' || new.doubled;"),
        [Some("2 16".to_owned())]
    );
    assert_eq!(
        run("INSERT INTO base VALUES (2, 'a', 8);
             DELETE FROM v WHERE key = 2 RETURNING old.key || ' ' || old.doubled;"),
        [Some("2 16".to_owned())]
    );
}
