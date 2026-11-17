//! Parity for the control flow of `IF` in trigger bodies: which branch a
//! `NULL` condition takes, and what runs after a `RETURN` inside a branch.
//!
//! Each fixture fires its triggers with the same writes on both engines and
//! compares what they left in `items` and `log`.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use super::harness::execute_ddl;
use crate::{helpers, helpers::postgres as postgres_harness};

mod schema {
    diesel::table! {
        items (id) {
            id -> Integer,
            val -> Nullable<Integer>,
        }
    }
    diesel::table! {
        log (seq) {
            seq -> Integer,
            id -> Integer,
            branch -> Text,
        }
    }
}

const TABLES: &str = "
CREATE TABLE items (id INT PRIMARY KEY, val INT);
CREATE TABLE log (seq SERIAL PRIMARY KEY, id INT NOT NULL, branch TEXT NOT NULL);
";

/// `ELSE` and `ELSIF` after a condition that is `NULL`.
const NULL_CONDITIONS: &str = "
CREATE FUNCTION pick() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.val > 0 THEN
        INSERT INTO log (id, branch) VALUES (NEW.id, 'then');
    ELSE
        INSERT INTO log (id, branch) VALUES (NEW.id, 'else');
    END IF;
    IF NEW.val > 0 THEN
        INSERT INTO log (id, branch) VALUES (NEW.id, 'positive');
    ELSIF NEW.val < 0 THEN
        INSERT INTO log (id, branch) VALUES (NEW.id, 'negative');
    ELSIF NEW.val IS NULL THEN
        INSERT INTO log (id, branch) VALUES (NEW.id, 'null');
    ELSE
        INSERT INTO log (id, branch) VALUES (NEW.id, 'zero');
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER items_pick AFTER INSERT OR UPDATE ON items
    FOR EACH ROW EXECUTE FUNCTION pick();
";

/// `RETURN NEW` and `RETURN NULL` under an `IF` in BEFORE triggers.
const BEFORE_RETURNS: &str = "
CREATE FUNCTION skip_positive() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.val > 0 THEN
        RETURN NEW;
    END IF;
    INSERT INTO log (id, branch) VALUES (NEW.id, 'not positive');
    RETURN NEW;
END $$;
CREATE TRIGGER items_skip BEFORE INSERT ON items
    FOR EACH ROW EXECUTE FUNCTION skip_positive();
CREATE FUNCTION veto_negative() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.val < 0 THEN
        RETURN NULL;
    ELSIF NEW.val IS NULL THEN
        INSERT INTO log (id, branch) VALUES (NEW.id, 'cleared');
    END IF;
    INSERT INTO log (id, branch) VALUES (NEW.id, 'updated');
    RETURN NEW;
END $$;
CREATE TRIGGER items_veto BEFORE UPDATE ON items
    FOR EACH ROW EXECUTE FUNCTION veto_negative();
";

/// `RETURN NULL` and `RETURN OLD` under an `IF` in AFTER triggers, with a
/// second trigger sharing the INSERT event, which PostgreSQL fires whatever
/// the first returned.
const AFTER_RETURNS: &str = "
CREATE FUNCTION log_unless_positive() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.val > 0 THEN
        RETURN NULL;
    END IF;
    INSERT INTO log (id, branch) VALUES (NEW.id, 'not positive');
    RETURN NULL;
END $$;
CREATE TRIGGER items_a AFTER INSERT ON items
    FOR EACH ROW EXECUTE FUNCTION log_unless_positive();
CREATE FUNCTION log_every() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO log (id, branch) VALUES (NEW.id, 'every');
    RETURN NULL;
END $$;
CREATE TRIGGER items_b AFTER INSERT ON items
    FOR EACH ROW EXECUTE FUNCTION log_every();
CREATE FUNCTION log_delete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.val IS NULL THEN
        RETURN OLD;
    END IF;
    INSERT INTO log (id, branch) VALUES (OLD.id, 'deleted');
    RETURN OLD;
END $$;
CREATE TRIGGER items_delete AFTER DELETE ON items
    FOR EACH ROW EXECUTE FUNCTION log_delete();
";

/// An early return two `IF` levels deep, and a helper check skipped by an
/// early return, the shape of a check that holds for some rows alone.
const NESTED_RETURNS: &str = "
CREATE FUNCTION require_small(v INT) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    IF v > 10 THEN
        RAISE EXCEPTION 'too large';
    END IF;
END $$;
CREATE FUNCTION nested() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.val IS NOT NULL THEN
        IF NEW.val > 100 THEN
            RETURN NEW;
        END IF;
        INSERT INTO log (id, branch) VALUES (NEW.id, 'checked');
    END IF;
    INSERT INTO log (id, branch) VALUES (NEW.id, 'tail');
    RETURN NEW;
END $$;
CREATE TRIGGER items_nested BEFORE INSERT ON items
    FOR EACH ROW EXECUTE FUNCTION nested();
CREATE FUNCTION guard_delete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.id <> 1 THEN
        RETURN OLD;
    END IF;
    PERFORM require_small(OLD.val);
    RETURN OLD;
END $$;
CREATE TRIGGER items_guard BEFORE DELETE ON items
    FOR EACH ROW EXECUTE FUNCTION guard_delete();
";

/// Nested blocks after an early return, so they run on two paths, one of them
/// assigning an outer variable under a condition reading its own, and two
/// sibling blocks declaring the same name.
const BLOCK_SCOPES: &str = "
CREATE FUNCTION scoped() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    tag TEXT;
BEGIN
    IF NEW.val > 100 THEN
        RETURN NEW;
    ELSIF NEW.val < 0 THEN
        INSERT INTO log (id, branch) VALUES (NEW.id, 'negative');
    END IF;
    DECLARE
        twice INT := NEW.val * 2;
    BEGIN
        IF twice > 10 THEN
            tag := 'large';
        END IF;
        INSERT INTO log (id, branch) VALUES (
            NEW.id,
            CASE WHEN twice > 0 THEN 'twice positive' WHEN twice < 0 THEN 'twice negative'
                ELSE 'twice null' END
        );
    END;
    IF NEW.val IS NULL THEN
        DECLARE
            kind TEXT := 'missing';
        BEGIN
            INSERT INTO log (id, branch) VALUES (NEW.id, kind);
        END;
    ELSE
        DECLARE
            kind TEXT := 'present';
        BEGIN
            INSERT INTO log (id, branch) VALUES (NEW.id, kind);
        END;
    END IF;
    INSERT INTO log (id, branch) VALUES (NEW.id, coalesce(tag, 'small'));
    RETURN NEW;
END $$;
CREATE TRIGGER items_scoped BEFORE INSERT ON items
    FOR EACH ROW EXECUTE FUNCTION scoped();
";

/// A record variable assigned `NEW` or `OLD` under a condition reading a
/// variable of a nested block, read after that block ends.
const ROW_CHOSEN_IN_BLOCK: &str = "
CREATE FUNCTION keep_larger() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    rec RECORD;
BEGIN
    DECLARE
        grew BOOLEAN := NEW.val > OLD.val;
    BEGIN
        IF grew THEN
            rec := NEW;
        ELSE
            rec := OLD;
        END IF;
    END;
    INSERT INTO log (id, branch) VALUES (coalesce(rec.val, -1), 'kept');
    RETURN NEW;
END $$;
CREATE TRIGGER items_keep AFTER UPDATE ON items
    FOR EACH ROW EXECUTE FUNCTION keep_larger();
";

/// A `RETURN` inside a nested block, and variables of a nested block read
/// after it ends, one never assigned and two assigned from each other.
const BLOCK_ENDINGS: &str = "
CREATE FUNCTION block_return() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    BEGIN
        IF NEW.val > 100 THEN
            INSERT INTO log (id, branch) VALUES (NEW.id, 'large');
            RETURN NEW;
        END IF;
        INSERT INTO log (id, branch) VALUES (NEW.id, 'inside');
    END;
    INSERT INTO log (id, branch) VALUES (NEW.id, 'after');
    RETURN NEW;
END $$;
CREATE TRIGGER items_block_return BEFORE INSERT ON items
    FOR EACH ROW EXECUTE FUNCTION block_return();
CREATE FUNCTION block_defaults() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    tag TEXT;
BEGIN
    DECLARE
        unset INT;
        a INT;
        b INT;
    BEGIN
        IF NEW.val > 0 THEN
            a := b;
        END IF;
        IF NEW.val > 0 THEN
            b := a;
        END IF;
        IF unset IS NULL AND a IS NULL THEN
            tag := 'defaults';
        END IF;
    END;
    INSERT INTO log (id, branch) VALUES (NEW.id, coalesce(tag, 'none'));
    RETURN NEW;
END $$;
CREATE TRIGGER items_block_defaults AFTER INSERT ON items
    FOR EACH ROW EXECUTE FUNCTION block_defaults();
";

#[derive(Debug)]
enum Step {
    Insert(&'static [(i32, Option<i32>)]),
    Update { id: i32, val: Option<i32> },
    Delete(i32),
    ReadItems,
    ReadLog,
}

macro_rules! runner {
    ($name:ident, $connection:ty) => {
        struct $name {
            connection: $connection,
        }
        impl $name {
            fn step(&mut self, step: &Step) -> Outcome {
                use self::schema::{items, log};
                match step {
                    Step::Insert(rows) => {
                        let rows: Vec<_> = rows
                            .iter()
                            .map(|(id, val)| (items::id.eq(*id), items::val.eq(*val)))
                            .collect();
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values(rows)
                                .execute(&mut self.connection),
                        )
                    }
                    Step::Update { id, val } => {
                        Outcome::of(
                            &diesel::update(items::table.find(*id))
                                .set(items::val.eq(*val))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::Delete(id) => {
                        Outcome::of(
                            &diesel::delete(items::table.find(*id)).execute(&mut self.connection),
                        )
                    }
                    Step::ReadItems => {
                        let rows: Vec<(i32, Option<i32>)> = items::table
                            .select((items::id, items::val))
                            .order(items::id.asc())
                            .load(&mut self.connection)
                            .expect("read items");
                        Outcome::Rows(
                            rows.into_iter().map(|(id, val)| format!("{id}:{val:?}")).collect(),
                        )
                    }
                    Step::ReadLog => {
                        let rows: Vec<(i32, String)> = log::table
                            .select((log::id, log::branch))
                            .order(log::seq.asc())
                            .load(&mut self.connection)
                            .expect("read log");
                        Outcome::Rows(
                            rows.into_iter().map(|(id, branch)| format!("{id}:{branch}")).collect(),
                        )
                    }
                }
            }
        }
    };
}

runner!(PgRunner, PgConnection);
runner!(SqliteRunner, SqliteConnection);

/// Runs `steps` against `triggers` on both engines, comparing every step.
fn assert_engines_agree(triggers: &str, steps: &[Step]) {
    let fixture = format!("{TABLES}{triggers}");
    let mut postgres = postgres_harness::fresh_database();
    postgres_harness::apply(&mut postgres, &fixture).expect("apply fixture to PostgreSQL");
    let translated = Pg2Sqlite::default()
        .sql(&fixture)
        .expect("parse fixture")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate fixture")
        .into_vec();
    let mut sqlite = establish_connection();
    execute_ddl(&mut sqlite, &translated);

    let mut postgres = PgRunner { connection: postgres };
    let mut sqlite = SqliteRunner { connection: sqlite };
    for step in steps {
        let expected = postgres.step(step);
        let actual = sqlite.step(step);
        assert_eq!(actual, expected, "the engines disagree on {step:?}");
    }
}

#[test]
fn a_null_condition_takes_else_and_reaches_a_later_elsif() {
    assert_engines_agree(
        NULL_CONDITIONS,
        &[
            Step::Insert(&[(1, None)]),
            Step::ReadLog,
            Step::Insert(&[(2, Some(5)), (3, Some(-5)), (4, Some(0))]),
            Step::ReadLog,
            Step::Update { id: 2, val: None },
            Step::ReadLog,
        ],
    );
}

#[test]
fn a_return_under_if_in_a_before_trigger_ends_the_body() {
    assert_engines_agree(
        BEFORE_RETURNS,
        &[
            Step::Insert(&[(1, Some(5)), (2, Some(-1)), (3, None)]),
            Step::ReadLog,
            Step::Update { id: 1, val: Some(-7) },
            Step::Update { id: 2, val: None },
            Step::Update { id: 3, val: Some(9) },
            Step::ReadItems,
            Step::ReadLog,
        ],
    );
}

#[test]
fn a_return_under_if_in_an_after_trigger_ends_its_body_alone() {
    assert_engines_agree(
        AFTER_RETURNS,
        &[
            Step::Insert(&[(1, Some(5)), (2, None), (3, Some(-1))]),
            Step::ReadItems,
            Step::ReadLog,
            Step::Delete(2),
            Step::Delete(1),
            Step::ReadItems,
            Step::ReadLog,
        ],
    );
}

#[test]
fn a_return_two_levels_deep_skips_what_follows_both() {
    assert_engines_agree(
        NESTED_RETURNS,
        &[
            Step::Insert(&[(1, Some(500)), (2, Some(50)), (3, None)]),
            Step::ReadItems,
            Step::ReadLog,
            Step::Delete(2),
            Step::Delete(1),
            Step::ReadItems,
        ],
    );
}

#[test]
fn a_block_after_an_early_return_keeps_its_variables_to_itself() {
    assert_engines_agree(
        BLOCK_SCOPES,
        &[
            Step::Insert(&[(1, Some(500)), (2, Some(-3)), (3, Some(7)), (4, None)]),
            Step::ReadItems,
            Step::ReadLog,
        ],
    );
}

#[test]
fn a_row_chosen_inside_a_block_reads_its_condition_after_the_block() {
    assert_engines_agree(
        ROW_CHOSEN_IN_BLOCK,
        &[
            Step::Insert(&[(1, Some(5)), (2, Some(5)), (3, None)]),
            Step::Update { id: 1, val: Some(9) },
            Step::Update { id: 2, val: Some(1) },
            Step::Update { id: 3, val: Some(4) },
            Step::ReadLog,
        ],
    );
}

#[test]
fn a_nested_block_returns_early_and_its_variables_end_with_it() {
    assert_engines_agree(
        BLOCK_ENDINGS,
        &[
            Step::Insert(&[(1, Some(500))]),
            Step::Insert(&[(2, Some(5))]),
            Step::ReadItems,
            Step::ReadLog,
        ],
    );
}
