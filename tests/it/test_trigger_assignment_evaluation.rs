//! A PL/pgSQL assignment in a trigger body runs where PostgreSQL runs it.
//!
//! A variable is read back by substituting its value at each read, so an
//! assignment is not a statement of its own. A value that raises, such as a
//! helper's check, has to run even when nothing reads the variable. A
//! PL/pgSQL helper assigned to a variable runs its body where the assignment
//! stands, and a call with no SQLite translation is refused wherever it is
//! assigned.

use diesel::{Connection as _, connection::SimpleConnection};
use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};

use crate::helpers::{Replica as Connection, SqliteAnyValue, query_values};

#[derive(Debug)]
struct CheckedPositive(i64);

impl diesel::serialize::ToSql<diesel::sql_types::Bool, diesel::sqlite::Sqlite> for CheckedPositive {
    fn to_sql<'value>(
        &'value self,
        output: &mut diesel::serialize::Output<'value, '_, diesel::sqlite::Sqlite>,
    ) -> diesel::serialize::Result {
        if self.0 < 0 {
            return Err(std::io::Error::other(format!("{} is negative", self.0)).into());
        }
        output.set_value(1);
        Ok(diesel::serialize::IsNull::No)
    }
}

/// A function the destination registers, failing for a negative argument.
const CHECK: &str = "check_positive";

/// The trigger statements `body` translates to, around a BEFORE INSERT
/// trigger on `t (id, val)`, beside three PL/pgSQL helpers.
/// `require_positive` raises for a negative argument and answers whether it
/// exceeds ten, `wrapped` returns what `require_positive` answers, and
/// `swallow` catches errors, which no SQLite trigger can.
fn translated(body: &str, options: &Pg2SqliteOptions) -> Result<Vec<String>, Error> {
    let statements = Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE t (id INT PRIMARY KEY, val INT);
             CREATE FUNCTION require_positive(target INT) RETURNS BOOLEAN LANGUAGE plpgsql AS $$
             BEGIN
                 IF target < 0 THEN RAISE EXCEPTION 'negative'; END IF;
                 RETURN target > 10;
             END $$;
             CREATE FUNCTION wrapped(target INT) RETURNS BOOLEAN LANGUAGE plpgsql AS $$
             BEGIN
                 RETURN require_positive(target);
             END $$;
             CREATE FUNCTION swallow(target INT) RETURNS BOOLEAN LANGUAGE plpgsql AS $$
             BEGIN
                 RETURN target > 0;
             EXCEPTION WHEN OTHERS THEN RETURN FALSE;
             END $$;
             CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$ {body} $$;
             CREATE TRIGGER go BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();"
        ))?
        .translate_to_sql(options)?;
    Ok(statements.into_vec().iter().map(ToString::to_string).collect())
}

/// A connection holding the translation of `body`, with `check_positive`
/// registered.
fn applied(body: &str) -> Connection {
    let options = Pg2SqliteOptions::default().with_user_defined_functions([CHECK]);
    let statements = translated(body, &options)
        .unwrap_or_else(|error| panic!("the trigger should translate: {error}"));
    let mut connection = Connection::establish(":memory:").expect("in-memory SQLite");
    connection
        .registration()
        .expect("registration")
        .sqlite()
        .register_sql_function::<(diesel::sql_types::BigInt,), diesel::sql_types::Bool, _, _, _>(
            CHECK,
            diesel::sqlite::SqliteFunctionBehavior::DETERMINISTIC,
            |(value,): (i64,)| CheckedPositive(value),
        )
        .expect("register check_positive");
    for statement in &statements {
        connection.batch_execute(statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

/// Whether inserting `(id, val)` succeeds.
fn inserts(connection: &mut Connection, id: i64, val: i64) -> bool {
    connection
        .execute(
            "INSERT INTO t VALUES (?1, ?2)",
            &[SqliteAnyValue::Integer(id), SqliteAnyValue::Integer(val)],
        )
        .is_ok()
}

/// The error inserting `(id, val)` fails with.
fn insert_error(connection: &mut Connection, id: i64, val: i64) -> String {
    connection
        .execute(
            "INSERT INTO t VALUES (?1, ?2)",
            &[SqliteAnyValue::Integer(id), SqliteAnyValue::Integer(val)],
        )
        .expect_err("the insert should fail")
        .to_string()
}

fn stored_ids(connection: &mut Connection) -> Vec<i64> {
    query_values(connection, "SELECT id FROM t ORDER BY id", &[])
        .expect("query")
        .into_iter()
        .map(|row| {
            match row[0] {
                SqliteAnyValue::Integer(value) => value,
                ref value => panic!("expected integer, got {value:?}"),
            }
        })
        .collect()
}

/// The helper's body runs where the assignment stands, and the variable reads
/// what it returned.
#[test]
fn a_helper_assigned_to_a_read_variable_runs_its_body_and_returns_its_value() {
    let mut connection = applied(
        "DECLARE large BOOLEAN;
         BEGIN
             large := require_positive(NEW.val);
             IF large THEN RETURN NULL; END IF;
             RETURN NEW;
         END",
    );
    assert!(inserts(&mut connection, 1, 5));
    assert!(inserts(&mut connection, 2, 50), "50 is large and vetoed without an error");
    assert!(insert_error(&mut connection, 3, -1).contains("negative"));
    assert_eq!(stored_ids(&mut connection), vec![1]);
}

#[test]
fn a_helper_assigned_to_an_unread_variable_runs_its_check() {
    let mut connection = applied(
        "DECLARE large BOOLEAN;
         BEGIN
             large := require_positive(NEW.val);
             RETURN NEW;
         END",
    );
    assert!(inserts(&mut connection, 1, 5));
    assert!(insert_error(&mut connection, 2, -1).contains("negative"));
    assert_eq!(stored_ids(&mut connection), vec![1]);
}

#[test]
fn a_helper_in_a_declaration_default_runs_its_body_and_returns_its_value() {
    let mut connection = applied(
        "DECLARE large BOOLEAN := require_positive(NEW.val);
         BEGIN
             IF large THEN RETURN NULL; END IF;
             RETURN NEW;
         END",
    );
    assert!(inserts(&mut connection, 1, 5));
    assert!(inserts(&mut connection, 2, 50), "50 is large and vetoed without an error");
    assert!(insert_error(&mut connection, 3, -1).contains("negative"));
    assert_eq!(stored_ids(&mut connection), vec![1]);
}

#[test]
fn a_helper_selected_into_a_variable_runs_its_body_and_returns_its_value() {
    let mut connection = applied(
        "DECLARE large BOOLEAN;
         BEGIN
             SELECT require_positive(NEW.val) INTO large;
             IF large THEN RETURN NULL; END IF;
             RETURN NEW;
         END",
    );
    assert!(inserts(&mut connection, 1, 5));
    assert!(inserts(&mut connection, 2, 50), "50 is large and vetoed without an error");
    assert!(insert_error(&mut connection, 3, -1).contains("negative"));
    assert_eq!(stored_ids(&mut connection), vec![1]);
}

/// The assignment in the branch not taken runs no helper body.
#[test]
fn a_helper_assigned_inside_a_branch_runs_only_when_the_branch_does() {
    let mut connection = applied(
        "DECLARE large BOOLEAN;
         BEGIN
             IF NEW.val > 100 THEN
                 large := require_positive(NEW.val - 200);
             END IF;
             RETURN NEW;
         END",
    );
    assert!(inserts(&mut connection, 1, 50), "val 50 takes no branch");
    assert!(insert_error(&mut connection, 2, 150).contains("negative"), "val 150 checks -50");
    assert!(inserts(&mut connection, 3, 250), "val 250 checks 50");
    assert_eq!(stored_ids(&mut connection), vec![1, 3]);
}

/// A helper returning another helper's answer runs that helper's body too.
#[test]
fn a_helper_returning_a_helper_call_runs_both_bodies() {
    let mut connection = applied(
        "DECLARE large BOOLEAN;
         BEGIN
             large := wrapped(NEW.val);
             IF large THEN RETURN NULL; END IF;
             RETURN NEW;
         END",
    );
    assert!(inserts(&mut connection, 1, 5));
    assert!(inserts(&mut connection, 2, 50), "50 is large and vetoed without an error");
    assert!(insert_error(&mut connection, 3, -1).contains("negative"));
    assert_eq!(stored_ids(&mut connection), vec![1]);
}

/// PostgreSQL skips the right operand of `AND` once the left one is false, so
/// a body that would run on every test has no equivalent.
#[test]
fn a_helper_assigned_where_postgres_may_skip_it_is_refused() {
    let error = translated(
        "DECLARE large BOOLEAN;
         BEGIN
             large := NEW.val > 0 AND require_positive(NEW.val);
             RETURN NEW;
         END",
        &Pg2SqliteOptions::default(),
    )
    .expect_err("the helper may be skipped");
    let message = error.to_string();
    assert!(message.contains("require_positive()") && message.contains("skip"), "{message}");
}

#[test]
fn a_helper_whose_body_does_not_translate_is_refused_where_assigned() {
    for body in [
        "DECLARE ok BOOLEAN; BEGIN ok := swallow(NEW.val); RETURN NEW; END",
        "DECLARE ok BOOLEAN := swallow(NEW.val); BEGIN RETURN NEW; END",
        "DECLARE ok BOOLEAN; BEGIN SELECT swallow(NEW.val) INTO ok; RETURN NEW; END",
    ] {
        let error = translated(body, &Pg2SqliteOptions::default())
            .expect_err("swallow() catches errors, which a SQLite trigger cannot");
        let message = error.to_string();
        assert!(message.contains("swallow") && message.contains("exception"), "{body}: {message}");
    }
}

#[test]
fn an_unread_assignment_runs_its_check() {
    let mut connection = applied(
        "DECLARE ok BOOLEAN;
         BEGIN
             ok := check_positive(NEW.val);
             RETURN NEW;
         END",
    );
    assert!(inserts(&mut connection, 1, 5));
    assert!(!inserts(&mut connection, 2, -1), "the check must reject the negative row");
    assert_eq!(stored_ids(&mut connection), vec![1]);
}

#[test]
fn an_unread_declaration_default_runs_its_check() {
    let mut connection = applied(
        "DECLARE ok BOOLEAN := check_positive(NEW.val);
         BEGIN
             RETURN NEW;
         END",
    );
    assert!(inserts(&mut connection, 1, 5));
    assert!(!inserts(&mut connection, 2, -1), "the check must reject the negative row");
    assert_eq!(stored_ids(&mut connection), vec![1]);
}

/// The branch not taken assigns nothing, so its check does not run either.
#[test]
fn an_assignment_inside_a_branch_runs_only_when_the_branch_does() {
    let mut connection = applied(
        "DECLARE ok BOOLEAN;
         BEGIN
             IF NEW.val > 100 THEN
                 ok := check_positive(NEW.val - 200);
             END IF;
             RETURN NEW;
         END",
    );
    assert!(inserts(&mut connection, 1, 50), "val 50 takes no branch");
    assert!(!inserts(&mut connection, 2, 150), "val 150 checks -50");
    assert!(inserts(&mut connection, 3, 250), "val 250 checks 50");
    assert_eq!(stored_ids(&mut connection), vec![1, 3]);
}

/// A value read later still runs where it is assigned, so a veto before the
/// read cannot skip the check PostgreSQL ran.
#[test]
fn an_assignment_runs_before_a_later_veto() {
    let mut connection = applied(
        "DECLARE ok BOOLEAN;
         BEGIN
             ok := check_positive(NEW.val + 10);
             IF NEW.val < 0 THEN RETURN NULL; END IF;
             IF ok THEN RETURN NEW; END IF;
             RETURN NEW;
         END",
    );
    assert!(inserts(&mut connection, 1, 5));
    assert!(inserts(&mut connection, 2, -5), "-5 checks 5 and is vetoed without an error");
    assert!(!inserts(&mut connection, 3, -20), "-20 checks -10, which raises before the veto");
    assert_eq!(stored_ids(&mut connection), vec![1]);
}
