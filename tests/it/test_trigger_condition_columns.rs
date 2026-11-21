//! A trigger's `WHEN` condition reads the row through `NEW` and `OLD` alone.
//!
//! PostgreSQL refuses a condition naming a column the table does not declare,
//! a bare column, or any other relation, when the trigger is created. SQLite
//! creates the trigger and fails every write that fires it, so each condition
//! here is refused at translation with the SQLSTATE and message PostgreSQL 18
//! answers.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlstate::{SqlState, pg};

const SCHEMA: &str = "
CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER);
CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
";

/// The trigger on `t` firing `f` before each updated row where `condition`
/// holds, or the error translating it answers.
fn translate(condition: &str) -> Result<Vec<String>, pg2sqlite::prelude::Error> {
    let source = format!(
        "{SCHEMA} CREATE TRIGGER tr BEFORE UPDATE ON t FOR EACH ROW WHEN ({condition}) \
         EXECUTE FUNCTION f();"
    );
    Pg2Sqlite::default()
        .sql(&source)?
        .translate_to_sql(&Pg2SqliteOptions::default())
        .map(|script| script.into_vec().iter().map(ToString::to_string).collect())
}

fn assert_refused(condition: &str, sqlstate: SqlState, message: &str) {
    let error = translate(condition)
        .map(|script| panic!("`{condition}` translated where PostgreSQL refuses it: {script:?}"))
        .unwrap_err();
    let refused = error.postgres_error().unwrap_or_else(|| panic!("`{condition}`: {error}"));
    assert_eq!((refused.sqlstate(), refused.message()), (sqlstate, message), "`{condition}`");
}

#[test]
fn a_column_the_table_does_not_declare_does_not_exist() {
    let undefined = |condition, message| assert_refused(condition, pg::UNDEFINED_COLUMN, message);
    undefined(r#"NEW."nosuch" IS NULL"#, "column new.nosuch does not exist");
    undefined("New.NoSuch IS NULL", "column new.nosuch does not exist");
    undefined(r#"NEW."NoSuch" IS NULL"#, "column new.NoSuch does not exist");
    undefined("OLD.nosuch IS NULL", "column old.nosuch does not exist");
    undefined(r#"NEW."A" IS NULL"#, "column new.A does not exist");
    undefined("nosuch IS NULL", r#"column "nosuch" does not exist"#);
}

/// `NEW` and `OLD` both carry every column of the table, so a bare column
/// reaches two rows.
#[test]
fn a_bare_column_is_ambiguous() {
    assert_refused("a IS NULL", pg::AMBIGUOUS_COLUMN, r#"column reference "a" is ambiguous"#);
    assert_refused("Id IS NULL", pg::AMBIGUOUS_COLUMN, r#"column reference "id" is ambiguous"#);
}

/// The table itself is in the condition's range only as `NEW` and `OLD`.
#[test]
fn a_relation_other_than_new_and_old_is_out_of_reach() {
    let invalid = |condition, table: &str| {
        assert_refused(
            condition,
            pg::UNDEFINED_TABLE,
            &format!(r#"invalid reference to FROM-clause entry for table "{table}""#),
        );
    };
    invalid("t.a IS NULL", "t");
    invalid("T.a IS NULL", "t");
    invalid("public.t.a IS NULL", "t");
    invalid("public.new.a IS NULL", "new");
    let missing = |condition, table: &str| {
        assert_refused(
            condition,
            pg::UNDEFINED_TABLE,
            &format!(r#"missing FROM-clause entry for table "{table}""#),
        );
    };
    missing("x.a IS NULL", "x");
    missing("NEW.a.b IS NULL", "a");
    missing(r#""NEW".a IS NULL"#, "NEW");
}

#[test]
fn columns_of_new_and_old_and_whole_rows_resolve() {
    for condition in [
        "NEW.a IS NULL",
        "New.A IS NULL",
        "OLD.id = 1",
        "NEW IS DISTINCT FROM OLD",
        r#"NEW."a" > 0"#,
    ] {
        translate(condition).unwrap_or_else(|error| panic!("`{condition}`: {error}"));
    }
}
