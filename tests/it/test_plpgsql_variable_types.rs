//! A PL/pgSQL variable compares as the type it is declared with.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    /// The table whose insert fires the guarded trigger.
    log (id) {
        /// Primary key.
        id -> Integer,
    }
}

/// A trigger body that raises when `condition` holds, with `declarations`
/// in its `DECLARE` section, `seeded` stored in `t.d` and 1.50 in
/// `t.amount`.
fn guarded_log(declarations: &str, condition: &str, seeded: &str) -> String {
    guarded_script("", declarations, "", condition, seeded)
}

/// [`guarded_log`] with `preamble` defined ahead of the trigger function and
/// `statements` run before the condition is tested. The `sink` table keys on
/// `id` and holds a `NUMERIC(10,3)` `amount`.
fn guarded_script(
    preamble: &str,
    declarations: &str,
    statements: &str,
    condition: &str,
    seeded: &str,
) -> String {
    format!(
        "CREATE TABLE t (id INT PRIMARY KEY, d INTERVAL NOT NULL, amount NUMERIC(10,2) NOT NULL);
CREATE TABLE log (id INT PRIMARY KEY);
CREATE TABLE sink (id INT PRIMARY KEY, amount NUMERIC(10,3));
INSERT INTO t VALUES (1, '{seeded}', 1.50);
{preamble}
CREATE FUNCTION mark() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE {declarations}
BEGIN
  {statements}
  IF {condition} THEN
    RAISE EXCEPTION 'hit';
  END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER mark_log BEFORE INSERT ON log FOR EACH ROW EXECUTE FUNCTION mark();"
    )
}

/// Whether the trigger `pg` translates to raised on a `log` insert.
fn raises(pg: &str) -> bool {
    let script = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec()
        .iter()
        .map(|statement| format!("{statement};"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    connection.batch_execute(&script).expect("apply the translated script");
    match diesel::insert_into(log::table).values(log::id.eq(1)).execute(&mut connection) {
        Ok(_) => false,
        Err(error) => {
            assert!(error.to_string().contains("hit"), "unexpected error: {error}");
            true
        }
    }
}

/// An interval variable read from a column orders as an interval against an
/// untyped literal. `9 days` sorts after `10 days` as text, so a text
/// comparison raises where PostgreSQL does not.
#[test]
fn an_interval_variable_orders_against_a_literal_as_an_interval() {
    let declarations = "v interval := (SELECT d FROM t WHERE id = 1);";
    assert!(!raises(&guarded_log(declarations, "v > '10 days'", "9 days")));
    assert!(raises(&guarded_log(declarations, "v > '10 days'", "11 days")));
}

/// Two interval variables order as intervals, `1 day` equal to `24 hours`
/// and below `25 hours`, however each was written.
#[test]
fn two_interval_variables_order_as_intervals() {
    let declarations = "a interval := (SELECT d FROM t WHERE id = 1); b interval := '24 hours';";
    assert!(raises(&guarded_log(declarations, "a = b", "1 day")));
    assert!(!raises(&guarded_log(declarations, "a < b", "1 day")));
    assert!(raises(&guarded_log(declarations, "a > b", "25 hours")));
}

/// A `CONSTANT` declaration types the variable as the type it names.
#[test]
fn a_constant_interval_orders_as_an_interval() {
    let declarations =
        "v interval := (SELECT d FROM t WHERE id = 1); c CONSTANT interval := '10 days';";
    assert!(!raises(&guarded_log(declarations, "v > c", "9 days")));
    assert!(raises(&guarded_log(declarations, "v > c", "11 days")));
}

/// A `NUMERIC(10,2)` variable holds its value as the minor units a column of
/// that type stores, so it compares against the column's 1.50 by value.
#[test]
fn a_numeric_variable_compares_against_a_column_of_its_type() {
    let amount = "(SELECT amount FROM t WHERE id = 1)";
    let declarations = "v NUMERIC(10,2) := 1.50; w NUMERIC(10,2) := 2;";
    assert!(raises(&guarded_log(declarations, &format!("v = {amount}"), "1 day")));
    assert!(!raises(&guarded_log(declarations, &format!("v < {amount}"), "1 day")));
    assert!(raises(&guarded_log(declarations, &format!("w > {amount}"), "1 day")));
}

/// An interval variable read in the query or the `VALUES` an `INSERT` takes
/// its row from compares as an interval, as it does in a bare `SELECT`.
#[test]
fn an_interval_variable_compares_inside_an_insert_source() {
    let declarations = "v interval := '216 hours';";
    let selected = "INSERT INTO sink (id) SELECT id FROM t WHERE d = v;";
    let condition = "EXISTS (SELECT 1 FROM sink)";
    assert!(raises(&guarded_script("", declarations, selected, condition, "9 days")));
    assert!(!raises(&guarded_script("", declarations, selected, condition, "8 days")));
    let counted = "INSERT INTO sink (id) VALUES ((SELECT count(*) FROM t WHERE d = v));";
    let condition = "EXISTS (SELECT 1 FROM sink WHERE id = 1)";
    assert!(raises(&guarded_script("", declarations, counted, condition, "9 days")));
    assert!(!raises(&guarded_script("", declarations, counted, condition, "8 days")));
}

/// A `NUMERIC(10,2)` variable stored into a `NUMERIC(10,3)` column keeps its
/// value, 1.50 reading back as 1.500.
#[test]
fn a_numeric_variable_keeps_its_value_in_a_wider_column() {
    let declarations = "v NUMERIC(10,2) := 1.50;";
    let statements = "INSERT INTO sink VALUES (1, v);";
    let condition = "(SELECT amount FROM sink WHERE id = 1) = 1.5";
    assert!(raises(&guarded_script("", declarations, statements, condition, "1 day")));
}

/// A `NUMERIC(10,3)` variable assigned to a `NUMERIC(10,2)` column rounds to
/// the column's scale, 2.125 storing 2.13.
#[test]
fn a_numeric_variable_rounds_to_a_narrower_column() {
    let declarations = "v NUMERIC(10,3) := 2.125;";
    let statements = "UPDATE t SET amount = v WHERE id = 1;";
    let condition = "(SELECT amount FROM t WHERE id = 1) = 2.13";
    assert!(raises(&guarded_script("", declarations, statements, condition, "1 day")));
}

/// A `NUMERIC(10,3)` variable that takes a `NUMERIC(10,2)` column's 1.50,
/// by `SELECT INTO` or as its initial value, holds 1.500.
#[test]
fn a_numeric_variable_holds_a_narrower_column_value_at_its_own_scale() {
    let selected = guarded_script(
        "",
        "v NUMERIC(10,3);",
        "SELECT amount INTO v FROM t WHERE id = 1;",
        "v = 1.5",
        "1 day",
    );
    assert!(raises(&selected));
    let declarations = "v NUMERIC(10,3) := (SELECT amount FROM t WHERE id = 1);";
    assert!(raises(&guarded_log(declarations, "v = 1.5", "1 day")));
}

/// A variable declared under a quoted name is read under that name.
#[test]
fn a_quoted_variable_name_is_read() {
    let declarations = "\"V\" int := 9;";
    let statements = "INSERT INTO sink (id) VALUES (\"V\");";
    let condition = "EXISTS (SELECT 1 FROM sink WHERE id = 9)";
    assert!(raises(&guarded_script("", declarations, statements, condition, "1 day")));
}

/// A `%TYPE` declaration types the variable as the column it names. `2 days`
/// sorts after `10 days` as text, and the column on the left of the second
/// comparison is an interval the translator orders by key.
#[test]
fn a_column_type_declaration_orders_as_the_column_type() {
    let declarations = "v t.d%TYPE := '2 days';";
    assert!(!raises(&guarded_log(declarations, "v > '10 days'", "9 days")));
    assert!(raises(&guarded_log(declarations, "v < '10 days'", "9 days")));
    let declarations = "v t.d%TYPE := '10 days';";
    let condition = "(SELECT d FROM t WHERE id = 1) > v";
    assert!(!raises(&guarded_log(declarations, condition, "9 days")));
    assert!(raises(&guarded_log(declarations, condition, "11 days")));
}

/// A `%TYPE` declaration of a `NUMERIC(10,2)` column holds minor units, so
/// 1.50 equals the stored 1.50.
#[test]
fn a_column_type_declaration_holds_the_column_scale() {
    let declarations = "v t.amount%TYPE := 1.50;";
    let condition = "v = (SELECT amount FROM t WHERE id = 1)";
    assert!(raises(&guarded_log(declarations, condition, "1 day")));
}

/// A `%TYPE` declaration naming an earlier variable takes that variable's
/// type.
#[test]
fn a_variable_type_declaration_orders_as_the_variable_type() {
    let declarations = "w interval; v w%TYPE := '2 days';";
    assert!(!raises(&guarded_log(declarations, "v > '10 days'", "9 days")));
    assert!(raises(&guarded_log(declarations, "v < '10 days'", "9 days")));
}
