//! Parity for PL/pgSQL variables a trigger body declares without a default
//! and reads before any statement assigns them, which PostgreSQL reads as
//! `NULL` in whatever statement reads them.
//!
//! Each fixture fires its trigger with the same writes on both engines and
//! compares what it left in storage.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use super::harness::execute_ddl;
use crate::{helpers, helpers::postgres as postgres_harness};

mod schema {
    diesel::table! {
        parents (id) {
            id -> Integer,
            label -> Nullable<Text>,
        }
    }
    diesel::table! {
        children (id) {
            id -> Integer,
            parent -> Integer,
            label -> Nullable<Text>,
        }
    }
    diesel::table! {
        audit (child) {
            child -> Integer,
            outcome -> Text,
        }
    }
}

const TABLES: &str = "
CREATE TABLE parents (id INTEGER PRIMARY KEY, label TEXT);
CREATE TABLE children (id INTEGER PRIMARY KEY, parent INTEGER NOT NULL, label TEXT);
CREATE TABLE audit (child INTEGER PRIMARY KEY, outcome TEXT NOT NULL);
";

/// The trigger function `t` declaring `fallback`, `noted` and `picked`
/// without defaults and running `statements`, fired `timing` every insert
/// into `children`.
fn trigger(timing: &str, statements: &str) -> String {
    trigger_declaring("fallback TEXT; noted TEXT; picked TEXT;", timing, statements)
}

/// The trigger function `t` with `declarations` running `statements`, fired
/// `timing` every insert into `children`.
fn trigger_declaring(declarations: &str, timing: &str, statements: &str) -> String {
    format!(
        "
CREATE FUNCTION t() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    {declarations}
BEGIN
{statements}
END $$;
CREATE TRIGGER children_t {timing} INSERT ON children
    FOR EACH ROW EXECUTE FUNCTION t();
"
    )
}

#[derive(Debug)]
enum Step {
    InsertParent { id: i32, label: &'static str },
    InsertChild { id: i32, parent: i32 },
    ReadAudit,
    ReadParents,
    ReadChildren,
}

macro_rules! runner {
    ($name:ident, $connection:ty) => {
        struct $name {
            connection: $connection,
        }
        impl $name {
            fn step(&mut self, step: &Step) -> Outcome {
                use self::schema::{audit, children, parents};
                match step {
                    Step::InsertParent { id, label } => {
                        Outcome::of(
                            &diesel::insert_into(parents::table)
                                .values((parents::id.eq(*id), parents::label.eq(*label)))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertChild { id, parent } => {
                        Outcome::of(
                            &diesel::insert_into(children::table)
                                .values((children::id.eq(*id), children::parent.eq(*parent)))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::ReadAudit => {
                        let rows: Vec<(i32, String)> = audit::table
                            .select((audit::child, audit::outcome))
                            .order(audit::child.asc())
                            .load(&mut self.connection)
                            .expect("read audit");
                        Outcome::Rows(
                            rows.into_iter()
                                .map(|(child, outcome)| format!("{child}:{outcome}"))
                                .collect(),
                        )
                    }
                    Step::ReadParents => {
                        let rows: Vec<(i32, Option<String>)> = parents::table
                            .select((parents::id, parents::label))
                            .order(parents::id.asc())
                            .load(&mut self.connection)
                            .expect("read parents");
                        Outcome::Rows(
                            rows.into_iter().map(|(id, label)| format!("{id}:{label:?}")).collect(),
                        )
                    }
                    Step::ReadChildren => {
                        let rows: Vec<(i32, Option<String>)> = children::table
                            .select((children::id, children::label))
                            .order(children::id.asc())
                            .load(&mut self.connection)
                            .expect("read children");
                        Outcome::Rows(
                            rows.into_iter().map(|(id, label)| format!("{id}:{label:?}")).collect(),
                        )
                    }
                }
            }
        }
    };
}

runner!(PostgresRunner, PgConnection);
runner!(SqliteRunner, SqliteConnection);

/// Fires `triggers` with a child of an existing parent and one of a missing
/// parent, and asserts both engines accept the same writes and leave the
/// same rows.
fn assert_parity(triggers: &str) {
    let fixture = format!("{TABLES}{triggers}");
    let mut postgres = PostgresRunner {
        connection: {
            let mut connection = postgres_harness::fresh_database();
            postgres_harness::apply(&mut connection, &fixture)
                .expect("apply fixture to PostgreSQL");
            connection
        },
    };
    let translated = Pg2Sqlite::default()
        .sql(&fixture)
        .expect("parse fixture")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate fixture")
        .into_vec();
    let mut sqlite = SqliteRunner {
        connection: {
            let mut connection = establish_connection();
            execute_ddl(&mut connection, &translated);
            connection
        },
    };
    for step in [
        Step::InsertParent { id: 1, label: "p" },
        Step::InsertChild { id: 10, parent: 1 },
        Step::InsertChild { id: 11, parent: 2 },
        Step::ReadAudit,
        Step::ReadParents,
        Step::ReadChildren,
    ] {
        let expected = postgres.step(&step);
        let actual = sqlite.step(&step);
        assert_eq!(actual, expected, "the engines disagree on {step:?}");
    }
}

/// An `UPDATE` reads an unassigned variable in its `SET` and its `WHERE`.
#[test]
fn an_update_reads_an_unassigned_variable_as_null() {
    assert_parity(&trigger(
        "AFTER",
        "    UPDATE parents SET label = coalesce(fallback, 'updated')
        WHERE fallback IS NULL AND id = NEW.parent;
    RETURN NULL;",
    ));
}

/// A `DELETE` reads an unassigned variable in its `WHERE`.
#[test]
fn a_delete_reads_an_unassigned_variable_as_null() {
    assert_parity(&trigger(
        "AFTER",
        "    DELETE FROM parents WHERE id = NEW.parent AND fallback IS NULL;
    RETURN NULL;",
    ));
}

/// An `INSERT ... SELECT` and an `ON CONFLICT ... DO UPDATE` read an
/// unassigned variable.
#[test]
fn an_insert_select_and_its_conflict_update_read_an_unassigned_variable_as_null() {
    assert_parity(&trigger(
        "AFTER",
        "    INSERT INTO audit (child, outcome) SELECT NEW.id, coalesce(fallback, 'selected');
    INSERT INTO audit (child, outcome) VALUES (NEW.id, 'again')
        ON CONFLICT (child) DO UPDATE SET outcome = coalesce(fallback, 'conflicted');
    RETURN NULL;",
    ));
}

/// A `SELECT ... INTO` reads an unassigned variable in its column and its
/// `WHERE`.
#[test]
fn a_select_into_reads_an_unassigned_variable_as_null() {
    assert_parity(&trigger(
        "AFTER",
        "    SELECT coalesce(fallback, label) INTO noted FROM parents
        WHERE id = NEW.parent AND fallback IS NULL;
    INSERT INTO audit (child, outcome) VALUES (NEW.id, coalesce(noted, 'none'));
    RETURN NULL;",
    ));
}

/// A value assigned from an unassigned variable, directly in a `CASE` or a
/// subquery, or through the `IF` that chose it, reads it as `NULL`.
#[test]
fn an_assigned_value_reads_an_unassigned_variable_as_null() {
    assert_parity(&trigger(
        "AFTER",
        "    IF fallback IS NULL THEN
        picked := 'chosen';
    ELSE
        picked := 'other';
    END IF;
    noted := CASE WHEN fallback IS NULL THEN picked ELSE 'set' END;
    noted := noted || (SELECT coalesce(fallback, ':') FROM parents WHERE id = NEW.parent);
    INSERT INTO audit (child, outcome) VALUES (NEW.id, coalesce(noted, 'none'));
    RETURN NULL;",
    ));
}

/// A `RAISE` guarded by an unassigned variable and a value read through a
/// subquery in `VALUES` read it as `NULL`.
#[test]
fn a_raise_and_a_values_subquery_read_an_unassigned_variable_as_null() {
    assert_parity(&trigger(
        "AFTER",
        "    IF fallback IS NOT NULL THEN
        RAISE EXCEPTION 'fallback is set';
    END IF;
    INSERT INTO audit (child, outcome) VALUES (NEW.id, (SELECT coalesce(fallback, 'valued')));
    RETURN NULL;",
    ));
}

/// A `BEFORE` trigger returning `NEW` or `NULL` by an unassigned variable
/// keeps or skips the row as PostgreSQL does.
#[test]
fn a_return_chosen_by_an_unassigned_variable_reads_it_as_null() {
    assert_parity(&trigger(
        "BEFORE",
        "    IF fallback IS NULL THEN
        RETURN NEW;
    END IF;
    RETURN NULL;",
    ));
}

/// A nested block's own unassigned variable reads as `NULL`.
#[test]
fn a_nested_block_reads_its_unassigned_variable_as_null() {
    assert_parity(&trigger(
        "AFTER",
        "    DECLARE
        inner_value TEXT;
    BEGIN
        INSERT INTO audit (child, outcome) VALUES (NEW.id, coalesce(inner_value, 'inner'));
    END;
    RETURN NULL;",
    ));
}

/// A variable of a row type that no statement assigned holds a row of
/// `NULL` fields, whether declared through `%ROWTYPE` or the table's name.
#[test]
fn an_unassigned_row_type_variable_reads_null_fields() {
    assert_parity(&trigger_declaring(
        "typed children%ROWTYPE; named children;",
        "AFTER",
        "    INSERT INTO audit (child, outcome)
        VALUES (NEW.id, coalesce(typed.label, 'typed') || coalesce(named.label, ':named'));
    RETURN NULL;",
    ));
}

/// A row-typed variable an `IF` assigned only in a branch that did not run
/// keeps its row of `NULL` fields.
#[test]
fn a_row_type_variable_assigned_in_one_branch_reads_null_fields_otherwise() {
    assert_parity(&trigger_declaring(
        "chosen children%ROWTYPE;",
        "AFTER",
        "    IF NEW.parent = 1 THEN
        chosen := NEW;
    END IF;
    INSERT INTO audit (child, outcome)
        VALUES (NEW.id, CASE WHEN chosen.id IS NULL THEN 'none' ELSE 'row' END);
    RETURN NULL;",
    ));
}

/// An unassigned row-typed variable is `NULL` as a whole, in a test and as
/// the text of the row.
#[test]
fn an_unassigned_row_type_variable_is_null() {
    assert_parity(&trigger_declaring(
        "typed children%ROWTYPE;",
        "AFTER",
        "    IF typed IS NULL THEN
        INSERT INTO audit (child, outcome)
            VALUES (NEW.id, coalesce(typed::text, 'null'));
    ELSE
        INSERT INTO audit (child, outcome) VALUES (NEW.id, 'set');
    END IF;
    RETURN NULL;",
    ));
}

/// A variable of a type no table names, an enum here, holds a scalar `NULL`
/// until assigned.
#[test]
fn an_unassigned_enum_variable_reads_null() {
    assert_parity(&format!(
        "CREATE TYPE mood AS ENUM ('calm', 'angry');{}",
        trigger_declaring(
            "felt mood;",
            "AFTER",
            "    INSERT INTO audit (child, outcome) VALUES (NEW.id, coalesce(felt::text, 'unset'));
    RETURN NULL;",
        )
    ));
}
