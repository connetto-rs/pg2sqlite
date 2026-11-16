//! A PL/pgSQL assignment in a trigger body runs where PostgreSQL runs it.
//!
//! A variable is read back by substituting its value at each read, so an
//! assignment is not a statement of its own. A value that raises, such as a
//! helper's check, has to run even when nothing reads the variable, and a call
//! with no SQLite translation has to be refused wherever it is assigned.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};
use rusqlite::{Connection, functions::FunctionFlags};

/// A function the destination registers, failing for a negative argument.
const CHECK: &str = "check_positive";

/// The trigger statements `body` translates to, around a BEFORE INSERT
/// trigger on `t (id, val)`.
fn translated(body: &str, options: &Pg2SqliteOptions) -> Result<Vec<String>, Error> {
    let statements = Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE t (id INT PRIMARY KEY, val INT);
             CREATE FUNCTION require_positive(target INT) RETURNS BOOLEAN LANGUAGE plpgsql AS $$
             BEGIN
                 IF target < 0 THEN RAISE EXCEPTION 'negative'; END IF;
                 RETURN TRUE;
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
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .create_scalar_function(CHECK, 1, FunctionFlags::SQLITE_DETERMINISTIC, |context| {
            let value = context.get::<i64>(0)?;
            if value < 0 {
                return Err(rusqlite::Error::UserFunctionError(
                    format!("{value} is negative").into(),
                ));
            }
            Ok(true)
        })
        .expect("register check_positive");
    for statement in &statements {
        connection.execute_batch(statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

/// Whether inserting `(id, val)` succeeds.
fn inserts(connection: &Connection, id: i64, val: i64) -> bool {
    connection.execute("INSERT INTO t VALUES (?1, ?2)", [id, val]).is_ok()
}

fn stored_ids(connection: &Connection) -> Vec<i64> {
    let mut prepared = connection.prepare("SELECT id FROM t ORDER BY id").expect("prepare");
    prepared
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<Vec<i64>, _>>()
        .expect("rows")
}

#[test]
fn a_call_with_no_translation_assigned_to_a_read_variable_is_refused() {
    let error = translated(
        "DECLARE ok BOOLEAN;
         BEGIN
             ok := require_positive(NEW.val);
             IF ok THEN RETURN NULL; END IF;
             RETURN NEW;
         END",
        &Pg2SqliteOptions::default(),
    )
    .expect_err("require_positive() has no SQLite translation");
    assert!(error.to_string().contains("require_positive()"), "{error}");
}

#[test]
fn a_call_with_no_translation_assigned_to_an_unread_variable_is_refused() {
    let error = translated(
        "DECLARE ok BOOLEAN;
         BEGIN
             ok := require_positive(NEW.val);
             RETURN NEW;
         END",
        &Pg2SqliteOptions::default(),
    )
    .expect_err("require_positive() has no SQLite translation");
    assert!(error.to_string().contains("require_positive()"), "{error}");
}

#[test]
fn an_unread_assignment_runs_its_check() {
    let connection = applied(
        "DECLARE ok BOOLEAN;
         BEGIN
             ok := check_positive(NEW.val);
             RETURN NEW;
         END",
    );
    assert!(inserts(&connection, 1, 5));
    assert!(!inserts(&connection, 2, -1), "the check must reject the negative row");
    assert_eq!(stored_ids(&connection), vec![1]);
}

#[test]
fn an_unread_declaration_default_runs_its_check() {
    let connection = applied(
        "DECLARE ok BOOLEAN := check_positive(NEW.val);
         BEGIN
             RETURN NEW;
         END",
    );
    assert!(inserts(&connection, 1, 5));
    assert!(!inserts(&connection, 2, -1), "the check must reject the negative row");
    assert_eq!(stored_ids(&connection), vec![1]);
}

/// The branch not taken assigns nothing, so its check does not run either.
#[test]
fn an_assignment_inside_a_branch_runs_only_when_the_branch_does() {
    let connection = applied(
        "DECLARE ok BOOLEAN;
         BEGIN
             IF NEW.val > 100 THEN
                 ok := check_positive(NEW.val - 200);
             END IF;
             RETURN NEW;
         END",
    );
    assert!(inserts(&connection, 1, 50), "val 50 takes no branch");
    assert!(!inserts(&connection, 2, 150), "val 150 checks -50");
    assert!(inserts(&connection, 3, 250), "val 250 checks 50");
    assert_eq!(stored_ids(&connection), vec![1, 3]);
}

/// A value read later still runs where it is assigned, so a veto before the
/// read cannot skip the check PostgreSQL ran.
#[test]
fn an_assignment_runs_before_a_later_veto() {
    let connection = applied(
        "DECLARE ok BOOLEAN;
         BEGIN
             ok := check_positive(NEW.val + 10);
             IF NEW.val < 0 THEN RETURN NULL; END IF;
             IF ok THEN RETURN NEW; END IF;
             RETURN NEW;
         END",
    );
    assert!(inserts(&connection, 1, 5));
    assert!(inserts(&connection, 2, -5), "-5 checks 5 and is vetoed without an error");
    assert!(!inserts(&connection, 3, -20), "-20 checks -10, which raises before the veto");
    assert_eq!(stored_ids(&connection), vec![1]);
}
