//! Parity for a trigger body that runs a PL/pgSQL check once per row of a
//! query, written as `FOR target IN query LOOP` and as `PERFORM f(column)
//! FROM ...`, which the replica runs as the check's statements over the
//! query's rows.
//!
//! Closing a parent checks each of its children. Each scenario closes the
//! same parent on both engines and compares whether the write is accepted,
//! which child the refusal names and what the tables hold after.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::{Outcome, apply, fresh_database};

use crate::helpers::postgres as postgres_harness;

mod schema {
    diesel::table! {
        parents (id) {
            id -> Integer,
            closed -> Bool,
        }
    }
    diesel::table! {
        children (id) {
            id -> Integer,
            parent_id -> Integer,
            weight -> Integer,
        }
    }
}

/// The tables and the per-child check, ahead of the trigger body.
const CHECK: &str = "
CREATE TABLE parents (id INTEGER PRIMARY KEY, closed BOOLEAN NOT NULL DEFAULT FALSE);
CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parents (id), weight INTEGER NOT NULL);
CREATE FUNCTION require_light(checked_child_id INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM children child WHERE child.id = checked_child_id AND child.weight > 10) THEN
        RAISE EXCEPTION 'child % is too heavy', checked_child_id;
    END IF;
END;
$$;
CREATE FUNCTION require_light_children(closing_id INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE
    kid_id INTEGER;
BEGIN
    FOR kid_id IN WITH kids AS (SELECT child.id FROM children child WHERE child.parent_id = closing_id)
        SELECT kid.id FROM kids kid LOOP
        PERFORM require_light(kid_id);
    END LOOP;
END;
$$;
";

/// The trigger running `body` after a parent changes.
fn trigger(body: &str) -> String {
    format!(
        "{CHECK}
CREATE FUNCTION check_parent() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    child_id INTEGER;
BEGIN
    {body}
    RETURN NULL;
END;
$$;
CREATE TRIGGER parents_check AFTER UPDATE ON parents FOR EACH ROW EXECUTE FUNCTION check_parent();
"
    )
}

/// The check run by a loop over the parent's children.
const LOOPED: &str =
    "FOR child_id IN SELECT child.id FROM children child WHERE child.parent_id = NEW.id LOOP
        PERFORM require_light(child_id);
    END LOOP;";

/// The check run by a `PERFORM` over the parent's children.
const PERFORMED: &str =
    "PERFORM require_light(child.id) FROM children child WHERE child.parent_id = NEW.id;";

/// The loop of [`LOOPED`] in a block declaring its variable by a name an
/// earlier sibling block declared as a whole row.
const LOOPED_AFTER_A_ROW_BLOCK: &str = "
    DECLARE kid children%ROWTYPE; BEGIN END;
    DECLARE kid INTEGER; BEGIN
        FOR kid IN SELECT child.id FROM children child WHERE child.parent_id = NEW.id LOOP
            PERFORM require_light(kid);
        END LOOP;
    END;";

/// The loop of [`LOOPED`] reading the parent through a variable.
const LOOPED_OVER_A_VARIABLE: &str = "
    DECLARE closing_id INTEGER := NEW.id; BEGIN
        FOR child_id IN SELECT child.id FROM children child WHERE child.parent_id = closing_id LOOP
            PERFORM require_light(child_id);
        END LOOP;
    END;";

/// The check of [`PERFORMED`] reading the parent through a variable.
const PERFORMED_OVER_A_VARIABLE: &str = "
    DECLARE closing_id INTEGER := NEW.id; BEGIN
        PERFORM require_light(child.id) FROM children child WHERE child.parent_id = closing_id;
    END;";

/// The loop of [`LOOPED_OVER_A_VARIABLE`] reading the variable in a `WITH`
/// query.
const LOOPED_OVER_A_WITH_QUERY: &str = "
    DECLARE closing_id INTEGER := NEW.id; BEGIN
        FOR child_id IN WITH kids AS (SELECT child.id FROM children child WHERE child.parent_id = closing_id)
            SELECT kid.id FROM kids kid LOOP
            PERFORM require_light(child_id);
        END LOOP;
    END;";

/// The loop of [`LOOPED_OVER_A_WITH_QUERY`] in a function passed the parent.
const CALLED_LOOP_OVER_A_WITH_QUERY: &str = "PERFORM require_light_children(NEW.id);";

/// Parent 1 has light children only, parent 2 one heavy child, parent 3 two,
/// and parent 4 none.
const CHILDREN: [(i32, i32, i32); 6] =
    [(11, 1, 1), (12, 1, 2), (21, 2, 20), (22, 2, 3), (31, 3, 30), (32, 3, 40)];

macro_rules! runner {
    ($name:ident, $connection:ty) => {
        struct $name {
            connection: $connection,
        }

        impl $name {
            fn seed(&mut self) {
                use self::schema::{children, parents};
                for parent in 1..=4 {
                    diesel::insert_into(parents::table)
                        .values(parents::id.eq(parent))
                        .execute(&mut self.connection)
                        .expect("seed a parent");
                }
                for (id, parent, weight) in CHILDREN {
                    diesel::insert_into(children::table)
                        .values((
                            children::id.eq(id),
                            children::parent_id.eq(parent),
                            children::weight.eq(weight),
                        ))
                        .execute(&mut self.connection)
                        .expect("seed a child");
                }
            }

            fn close(&mut self, parent: i32) -> QueryResult<usize> {
                use self::schema::parents;
                diesel::update(parents::table.filter(parents::id.eq(parent)))
                    .set(parents::closed.eq(true))
                    .execute(&mut self.connection)
            }

            fn closed(&mut self) -> Vec<i32> {
                use self::schema::parents;
                parents::table
                    .filter(parents::closed)
                    .select(parents::id)
                    .order(parents::id)
                    .load(&mut self.connection)
                    .expect("read the parents")
            }
        }
    };
}

runner!(Postgres, PgConnection);
runner!(Sqlite, SqliteConnection);

fn replica(source: &str) -> SqliteConnection {
    let statements = Pg2Sqlite::default()
        .sql(source)
        .and_then(|parsed| parsed.translate_to_sql(&Pg2SqliteOptions::default()))
        .unwrap_or_else(|error| panic!("the schema should translate: {error}"))
        .into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    for statement in &statements {
        diesel::connection::SimpleConnection::batch_execute(&mut connection, statement)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

/// The children a refusal names as too heavy.
fn named_children(error: &diesel::result::Error) -> Vec<i32> {
    let message = error.to_string();
    CHILDREN
        .iter()
        .map(|(id, _, _)| *id)
        .filter(|id| message.contains(&format!("child {id} is too heavy")))
        .collect()
}

/// Closing each parent runs the check for each of its children on both
/// engines: parents 1 and 4 close, parent 2 is refused naming child 21, and
/// parent 3 naming one of its two heavy children.
fn checks_every_row_as_postgres_does(body: &str) {
    let source = trigger(body);
    let mut postgres = Postgres { connection: fresh_database() };
    apply(&mut postgres.connection, &source).expect("apply the schema on PostgreSQL");
    let mut sqlite = Sqlite { connection: replica(&source) };
    postgres.seed();
    sqlite.seed();

    for (parent, heavy) in [(1, &[][..]), (2, &[21][..]), (3, &[31, 32][..]), (4, &[][..])] {
        let expected = postgres.close(parent);
        let actual = sqlite.close(parent);
        assert_eq!(Outcome::of(&actual), Outcome::of(&expected), "closing parent {parent}");
        for (engine, result) in [("PostgreSQL", &expected), ("SQLite", &actual)] {
            match result {
                Ok(_) => assert!(heavy.is_empty(), "{engine} closed parent {parent}"),
                Err(error) => {
                    let named = named_children(error);
                    assert!(
                        named.len() == 1 && heavy.contains(&named[0]),
                        "{engine} refused parent {parent} with {error}"
                    );
                }
            }
        }
    }
    assert_eq!(sqlite.closed(), postgres.closed());
    assert_eq!(postgres.closed(), vec![1, 4]);
}

#[test]
fn a_loop_checks_every_row_as_postgres_does() {
    checks_every_row_as_postgres_does(LOOPED);
}

#[test]
fn a_perform_over_rows_checks_every_row_as_postgres_does() {
    checks_every_row_as_postgres_does(PERFORMED);
}

#[test]
fn a_loop_variable_named_as_an_ended_blocks_row_checks_every_row_as_postgres_does() {
    checks_every_row_as_postgres_does(LOOPED_AFTER_A_ROW_BLOCK);
}

#[test]
fn a_loop_over_a_with_query_reading_a_variable_checks_every_row_as_postgres_does() {
    checks_every_row_as_postgres_does(LOOPED_OVER_A_WITH_QUERY);
}

#[test]
fn a_loop_reading_a_variable_checks_every_row_as_postgres_does() {
    checks_every_row_as_postgres_does(LOOPED_OVER_A_VARIABLE);
}

#[test]
fn a_called_loop_over_a_with_query_reading_a_parameter_checks_every_row_as_postgres_does() {
    checks_every_row_as_postgres_does(CALLED_LOOP_OVER_A_WITH_QUERY);
}

#[test]
fn a_perform_over_rows_reading_a_variable_checks_every_row_as_postgres_does() {
    checks_every_row_as_postgres_does(PERFORMED_OVER_A_VARIABLE);
}
