//! References a PL/pgSQL label qualifies.
//!
//! PL/pgSQL labels the function's outermost level with the function's name,
//! so `keep_val.old.val` reads the trigger's `OLD`, and labels each block
//! with the label written before it. A qualified reference reaches what the
//! bare name reaches unless an inner declaration hides it. Each case was
//! measured on PostgreSQL 18.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers;

mod schema {
    diesel::table! {
        t (id) {
            id -> Integer,
            val -> Nullable<Integer>,
            op -> Nullable<Text>,
        }
    }
}

use schema::t;

/// `t` and the row trigger `event` runs `function`, whose body is `body`.
fn trigger_on_t(event: &str, function: &str, body: &str) -> String {
    format!(
        "CREATE TABLE t (id INT PRIMARY KEY, val INT, op TEXT);
         CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$
         {body}
         $$;
         CREATE TRIGGER guard BEFORE {event} ON t FOR EACH ROW EXECUTE FUNCTION {function}();"
    )
}

fn rows(conn: &mut crate::helpers::Replica) -> Vec<(i32, Option<i32>, Option<String>)> {
    t::table.order(t::id).load(conn).expect("rows load")
}

#[test]
fn the_stored_row_read_through_the_function_label_guards_an_update() {
    let mut conn = helpers::apply_default(&trigger_on_t(
        "UPDATE",
        "keep_val",
        "BEGIN
           IF keep_val.old.val IS NOT NULL
              AND keep_val.new.val IS DISTINCT FROM keep_val.old.val THEN
             RAISE EXCEPTION 'val is already set';
           END IF;
           RETURN keep_val.new;
         END",
    ))
    .expect("the trigger translates and applies");
    diesel::insert_into(t::table)
        .values(&vec![(t::id.eq(1), t::val.eq(None::<i32>)), (t::id.eq(2), t::val.eq(Some(5)))])
        .execute(&mut conn)
        .expect("insert");

    diesel::update(t::table.find(1)).set(t::val.eq(7)).execute(&mut conn).expect("a first value");
    diesel::update(t::table.find(2))
        .set(t::val.eq(8))
        .execute(&mut conn)
        .expect_err("a set value is kept");
    diesel::update(t::table.find(2)).set(t::val.eq(5)).execute(&mut conn).expect("the same value");

    assert_eq!(rows(&mut conn), vec![(1, Some(7), None), (2, Some(5), None)]);
}

#[test]
fn a_field_assigned_through_the_function_label_is_written() {
    let mut conn = helpers::apply_default(&trigger_on_t(
        "INSERT",
        "stamp",
        "BEGIN
           stamp.new.val := stamp.new.id * 42;
           NEW.op := stamp.tg_op;
           RETURN NEW;
         END",
    ))
    .expect("the trigger translates and applies");

    diesel::insert_into(t::table).values(t::id.eq(1)).execute(&mut conn).expect("insert");

    assert_eq!(rows(&mut conn), vec![(1, Some(42), Some("INSERT".to_owned()))]);
}

#[test]
fn a_variable_read_through_its_block_label_is_read() {
    let mut conn = helpers::apply_default(&trigger_on_t(
        "INSERT",
        "capped",
        "<<limits>>
         DECLARE ceiling INT := 100;
         BEGIN
           IF NEW.val > limits.ceiling THEN
             RAISE EXCEPTION 'val is too large';
           END IF;
           RETURN NEW;
         END",
    ))
    .expect("the trigger translates and applies");

    diesel::insert_into(t::table)
        .values((t::id.eq(1), t::val.eq(21)))
        .execute(&mut conn)
        .expect("a value under the ceiling");
    diesel::insert_into(t::table)
        .values((t::id.eq(2), t::val.eq(200)))
        .execute(&mut conn)
        .expect_err("a value over the ceiling");

    assert_eq!(rows(&mut conn), vec![(1, Some(21), None)]);
}

/// The label reaches the trigger's `OLD` past an inner `old`, which the bare
/// name would read instead, so no unlabelled body reads the same.
#[test]
fn a_record_an_inner_declaration_hides_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(&trigger_on_t(
            "UPDATE",
            "hid",
            "BEGIN
               DECLARE old INT := 0;
               BEGIN
                 IF hid.old.val IS NOT NULL THEN
                   RAISE EXCEPTION 'val is already set';
                 END IF;
               END;
               RETURN NEW;
             END",
        ))
        .expect("the schema parses")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("no unlabelled body reads the trigger's OLD there");

    assert!(error.to_string().contains("hid.old"), "the refusal names the reference: {error}");
}

/// PostgreSQL rejects the function, since the alias names no variable.
#[test]
fn a_labelled_body_aliasing_nothing_is_refused() {
    Pg2Sqlite::default()
        .sql(&trigger_on_t(
            "INSERT",
            "aliased",
            "DECLARE stored ALIAS FOR nothing;
             BEGIN
               RETURN aliased.new;
             END",
        ))
        .expect("the schema parses")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("PostgreSQL does not compile the body");
}
