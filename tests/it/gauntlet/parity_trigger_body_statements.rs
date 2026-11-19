//! Parity for `PERFORM` and locking `SELECT ... INTO` in trigger bodies.
//!
//! Each fixture runs the same writes on both engines and compares what the
//! trigger left in storage, which is where `FOUND` and the locked read show.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use super::harness::execute_ddl;
use crate::{helpers, helpers::postgres as postgres_harness};

/// Translates `fixture` and applies it to a fresh SQLite connection.
fn apply_to_sqlite(fixture: &str) -> SqliteConnection {
    let translated = Pg2Sqlite::default()
        .sql(fixture)
        .expect("parse fixture")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate fixture")
        .into_vec();
    let mut connection = establish_connection();
    execute_ddl(&mut connection, &translated);
    connection
}

/// Applies `fixture` to a fresh PostgreSQL database.
fn apply_to_postgres(fixture: &str) -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, fixture).expect("apply fixture to PostgreSQL");
    connection
}

mod found_schema {
    diesel::table! {
        parents (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        children (id) {
            id -> Integer,
            parent -> Integer,
        }
    }
    diesel::table! {
        audit (child) {
            child -> Integer,
            outcome -> Text,
        }
    }
}

const FOUND_TABLES: &str = "
CREATE TABLE parents (id INTEGER PRIMARY KEY);
CREATE TABLE children (id INTEGER PRIMARY KEY, parent INTEGER NOT NULL);
CREATE TABLE audit (child INTEGER PRIMARY KEY, outcome TEXT NOT NULL);
";

/// `IF FOUND` after a `PERFORM` that reads a row, and after one that reads
/// none.
const FOUND_BRANCHES: &str = "
CREATE FUNCTION note_parent() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM 1 FROM parents WHERE id = NEW.parent;
    IF FOUND THEN
        INSERT INTO audit (child, outcome) VALUES (NEW.id, 'found');
    ELSE
        INSERT INTO audit (child, outcome) VALUES (NEW.id, 'missing');
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER children_note AFTER INSERT ON children
    FOR EACH ROW EXECUTE FUNCTION note_parent();
";

/// `IF NOT FOUND THEN RAISE`, the shape an authorization check takes.
const FOUND_GUARD: &str = "
CREATE FUNCTION require_parent() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM 1;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'a query without FROM always answers a row';
    END IF;
    PERFORM 1 FROM parents WHERE id = NEW.parent;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'missing parent';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER children_require BEFORE INSERT ON children
    FOR EACH ROW EXECUTE FUNCTION require_parent();
";

#[derive(Debug)]
enum FoundStep {
    InsertParent(i32),
    InsertChild { id: i32, parent: i32 },
    ReadAudit,
    ReadChildren,
}

macro_rules! found_runner {
    ($name:ident, $connection:ty) => {
        struct $name {
            connection: $connection,
        }
        impl $name {
            fn step(&mut self, step: &FoundStep) -> Outcome {
                use self::found_schema::{audit, children, parents};
                match step {
                    FoundStep::InsertParent(id) => {
                        Outcome::of(
                            &diesel::insert_into(parents::table)
                                .values(parents::id.eq(*id))
                                .execute(&mut self.connection),
                        )
                    }
                    FoundStep::InsertChild { id, parent } => {
                        Outcome::of(
                            &diesel::insert_into(children::table)
                                .values((children::id.eq(*id), children::parent.eq(*parent)))
                                .execute(&mut self.connection),
                        )
                    }
                    FoundStep::ReadAudit => {
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
                    FoundStep::ReadChildren => {
                        let rows: Vec<i32> = children::table
                            .select(children::id)
                            .order(children::id.asc())
                            .load(&mut self.connection)
                            .expect("read children");
                        Outcome::Rows(rows.into_iter().map(|id| id.to_string()).collect())
                    }
                }
            }
        }
    };
}

found_runner!(FoundPostgresRunner, PgConnection);
found_runner!(FoundSqliteRunner, SqliteConnection);

fn found_scenario() -> Vec<FoundStep> {
    vec![
        FoundStep::InsertParent(1),
        FoundStep::InsertChild { id: 10, parent: 1 },
        FoundStep::InsertChild { id: 11, parent: 2 },
        FoundStep::ReadAudit,
        FoundStep::ReadChildren,
    ]
}

fn run_found(triggers: &str) {
    let fixture = format!("{FOUND_TABLES}{triggers}");
    let mut postgres = FoundPostgresRunner { connection: apply_to_postgres(&fixture) };
    let mut sqlite = FoundSqliteRunner { connection: apply_to_sqlite(&fixture) };
    for step in found_scenario() {
        let expected = postgres.step(&step);
        let actual = sqlite.step(&step);
        assert_eq!(actual, expected, "the engines disagree on {step:?}");
    }
}

/// `FOUND` after a `PERFORM` that reads a row is true, and false after one
/// that reads none, so each child takes the branch PostgreSQL takes.
#[test]
fn found_after_perform_picks_the_same_branch() {
    run_found(FOUND_BRANCHES);
}

/// A `PERFORM` guard refuses the child whose parent is missing on both
/// engines, and a `PERFORM` without `FROM` always finds its row.
#[test]
fn found_after_perform_guards_the_same_writes() {
    run_found(FOUND_GUARD);
}

/// A value the body checks where it is assigned reads a declared variable no
/// statement assigned yet as `NULL`.
const UNASSIGNED_READ: &str = "
CREATE FUNCTION note_fallback() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    fallback TEXT;
    noted TEXT;
BEGIN
    noted := coalesce(fallback, 'unset');
    INSERT INTO audit (child, outcome) VALUES (NEW.id, noted);
    RETURN NULL;
END $$;
CREATE TRIGGER children_fallback AFTER INSERT ON children
    FOR EACH ROW EXECUTE FUNCTION note_fallback();
";

/// An assignment computing from a variable not yet assigned reads `NULL`,
/// so each child is noted as PostgreSQL notes it.
#[test]
fn an_assignment_reads_an_unassigned_variable_as_null() {
    run_found(UNASSIGNED_READ);
}

/// An `IF` condition and a `SELECT ... INTO` reading a declared variable no
/// statement assigned, each read again by the `INSERT` after them, which an
/// `IF` reading the same variable guards.
const UNASSIGNED_CONDITION_AND_SELECT_INTO: &str = "
CREATE FUNCTION note_unassigned_reads() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    fallback INTEGER;
    noted TEXT;
    found_parent INTEGER;
BEGIN
    IF fallback IS NULL THEN
        noted := 'unset';
    ELSE
        noted := 'set';
    END IF;
    SELECT id INTO found_parent FROM parents WHERE id = coalesce(fallback, NEW.parent);
    IF fallback IS NULL THEN
        INSERT INTO audit (child, outcome)
            VALUES (NEW.id, noted || ':' || coalesce(found_parent::text, 'none'));
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER children_unassigned AFTER INSERT ON children
    FOR EACH ROW EXECUTE FUNCTION note_unassigned_reads();
";

/// The values bound under the `IF` and by the `SELECT ... INTO`, and the
/// guard of the `INSERT`, read the unassigned variable as `NULL`.
#[test]
fn a_condition_and_a_select_into_read_an_unassigned_variable_as_null() {
    run_found(UNASSIGNED_CONDITION_AND_SELECT_INTO);
}

mod lock_schema {
    diesel::table! {
        organizations (id) {
            id -> Integer,
            name -> Text,
        }
    }
    diesel::table! {
        members (id) {
            id -> Integer,
            organization -> Integer,
            organization_name -> Nullable<Text>,
        }
    }
}

/// A `PERFORM` and a `SELECT ... INTO` taking `LOCK` on the organization row,
/// as an authorization check locks the row it reads.
fn lock_fixture(lock: &str) -> String {
    format!(
        "
CREATE TABLE organizations (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE members (id INTEGER PRIMARY KEY, organization INTEGER NOT NULL, organization_name TEXT);
CREATE FUNCTION stamp_member() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    found_name TEXT;
BEGIN
    PERFORM 1 FROM organizations WHERE id = NEW.organization {lock};
    IF NOT FOUND THEN
        RAISE EXCEPTION 'no organization';
    END IF;
    SELECT name INTO found_name FROM organizations WHERE id = NEW.organization {lock};
    UPDATE members SET organization_name = found_name WHERE id = NEW.id;
    RETURN NULL;
END $$;
CREATE TRIGGER members_stamp AFTER INSERT ON members
    FOR EACH ROW EXECUTE FUNCTION stamp_member();
"
    )
}

#[derive(Debug)]
enum LockStep {
    InsertOrganization { id: i32, name: &'static str },
    InsertMember { id: i32, organization: i32 },
    ReadMembers,
}

macro_rules! lock_runner {
    ($name:ident, $connection:ty) => {
        struct $name {
            connection: $connection,
        }
        impl $name {
            fn step(&mut self, step: &LockStep) -> Outcome {
                use self::lock_schema::{members, organizations};
                match step {
                    LockStep::InsertOrganization { id, name } => {
                        Outcome::of(
                            &diesel::insert_into(organizations::table)
                                .values((organizations::id.eq(*id), organizations::name.eq(*name)))
                                .execute(&mut self.connection),
                        )
                    }
                    LockStep::InsertMember { id, organization } => {
                        Outcome::of(
                            &diesel::insert_into(members::table)
                                .values((
                                    members::id.eq(*id),
                                    members::organization.eq(*organization),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    LockStep::ReadMembers => {
                        let rows: Vec<(i32, Option<String>)> = members::table
                            .select((members::id, members::organization_name))
                            .order(members::id.asc())
                            .load(&mut self.connection)
                            .expect("read members");
                        Outcome::Rows(
                            rows.into_iter()
                                .map(|(id, name)| format!("{id}:{}", name.unwrap_or_default()))
                                .collect(),
                        )
                    }
                }
            }
        }
    };
}

lock_runner!(LockPostgresRunner, PgConnection);
lock_runner!(LockSqliteRunner, SqliteConnection);

/// Every row-locking clause reads the same rows on both engines, `SKIP
/// LOCKED` included, and a missing row is refused on both.
#[test]
fn locking_reads_answer_the_same_rows() {
    for lock in [
        "FOR UPDATE",
        "FOR NO KEY UPDATE",
        "FOR SHARE",
        "FOR KEY SHARE",
        "FOR UPDATE NOWAIT",
        "FOR UPDATE SKIP LOCKED",
        "FOR NO KEY UPDATE OF organizations",
    ] {
        let fixture = lock_fixture(lock);
        let mut postgres = LockPostgresRunner { connection: apply_to_postgres(&fixture) };
        let mut sqlite = LockSqliteRunner { connection: apply_to_sqlite(&fixture) };
        for step in [
            LockStep::InsertOrganization { id: 1, name: "acme" },
            LockStep::InsertMember { id: 10, organization: 1 },
            LockStep::InsertMember { id: 11, organization: 2 },
            LockStep::ReadMembers,
        ] {
            let expected = postgres.step(&step);
            let actual = sqlite.step(&step);
            assert_eq!(actual, expected, "the engines disagree on {step:?} under {lock}");
        }
    }
}
