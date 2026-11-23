//! A `CHECK` reading the clock, which SQLite refuses inside a `CHECK`,
//! enforced by triggers that judge each write when it happens.

use core::sync::atomic::{AtomicBool, Ordering};

use diesel::{
    prelude::*,
    result::Error as DieselError,
    sql_types::{Bool, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite::prelude::Pg2SqliteOptions;

use crate::helpers;

diesel::table! {
    stamped (id) {
        id -> Integer,
        started_at -> Nullable<Text>,
        note -> Nullable<Text>,
    }
}

#[declare_sql_function]
extern "SQL" {
    /// SQLite's `strftime`, reading `time` shifted by `modifier`.
    fn strftime(format: Text, time: Text, modifier: Text) -> Text;
    /// The RLS exemption the translated guards consult.
    fn write_exempt() -> Bool;
}

/// The text the replica stores a `timestamptz` as.
const TIMESTAMPTZ: &str = "%Y-%m-%d %H:%M:%f000+00:00";

/// The `timestamptz` text of the instant `shift` away from now.
fn stamp(connection: &mut SqliteConnection, shift: &str) -> String {
    diesel::select(strftime(TIMESTAMPTZ, "now", shift)).get_result(connection).expect("strftime")
}

fn insert(
    connection: &mut SqliteConnection,
    id: i32,
    started_at: Option<&str>,
) -> Result<usize, DieselError> {
    diesel::insert_into(stamped::table)
        .values((stamped::id.eq(id), stamped::started_at.eq(started_at)))
        .execute(connection)
}

/// The message SQLite reports for a refused write.
fn refusal(result: Result<usize, DieselError>) -> String {
    match result {
        Err(DieselError::DatabaseError(_, information)) => information.message().to_string(),
        other => panic!("the write must be refused, got {other:?}"),
    }
}

const TOLERANT: &str = "CREATE TABLE stamped (id INT PRIMARY KEY, started_at TIMESTAMPTZ CHECK \
                        (started_at <= now() + interval '5 minutes'), note TEXT);";

#[test]
fn a_clock_check_admits_a_row_stamped_now_and_null_and_refuses_one_an_hour_ahead() {
    let mut connection = helpers::apply_default(TOLERANT).expect("the translated table applies");
    let now = stamp(&mut connection, "+0 seconds");
    let ahead = stamp(&mut connection, "+1 hour");

    insert(&mut connection, 1, Some(&now)).expect("a row stamped now is admitted");
    insert(&mut connection, 2, None).expect("a NULL stamp passes, as it does in a CHECK");
    let violation = "new row for relation \"stamped\" violates check constraint \
                     \"stamped_started_at_check\"";
    assert_eq!(refusal(insert(&mut connection, 3, Some(&ahead))), violation);
    assert_eq!(
        refusal(
            diesel::update(stamped::table.find(1))
                .set(stamped::started_at.eq(&ahead))
                .execute(&mut connection)
        ),
        violation
    );
}

/// Whether the exemption the translated guards consult holds.
static EXEMPT: AtomicBool = AtomicBool::new(true);

/// PostgreSQL checks the whole row on every update, and a row the server
/// wrote, applied under the RLS exemption, is not judged again.
#[test]
fn an_exempt_write_lands_and_an_update_of_another_column_is_checked() {
    let options = Pg2SqliteOptions::default().with_rls_exemption_function("write_exempt");
    let mut connection =
        helpers::apply_with_options(TOLERANT, &options).expect("the translated table applies");
    write_exempt_utils::register_nondeterministic_impl(&mut connection, || {
        EXEMPT.load(Ordering::Relaxed)
    })
    .expect("register the exemption");
    let ahead = stamp(&mut connection, "+1 hour");

    insert(&mut connection, 1, Some(&ahead)).expect("an exempt write is not judged");
    EXEMPT.store(false, Ordering::Relaxed);
    refusal(insert(&mut connection, 2, Some(&ahead)));
    refusal(
        diesel::update(stamped::table.find(1)).set(stamped::note.eq("x")).execute(&mut connection),
    );
}

#[test]
fn a_table_check_qualifying_its_column_raises_its_declared_name() {
    let mut connection = helpers::apply_default(
        "CREATE TABLE stamped (id INT PRIMARY KEY, started_at TIMESTAMPTZ, note TEXT, \
         CONSTRAINT Bounded CHECK (stamped.started_at <= now()));",
    )
    .expect("the translated table applies");
    let ago = stamp(&mut connection, "-1 minute");
    let ahead = stamp(&mut connection, "+1 hour");

    insert(&mut connection, 1, Some(&ago)).expect("a row stamped in the past is admitted");
    assert_eq!(
        refusal(insert(&mut connection, 2, Some(&ahead))),
        "new row for relation \"stamped\" violates check constraint \"bounded\""
    );
}

fn insert_note(connection: &mut SqliteConnection, id: i32) -> Result<usize, DieselError> {
    diesel::insert_into(stamped::table)
        .values((stamped::id.eq(id), stamped::note.eq("x")))
        .execute(connection)
}

/// PostgreSQL drops a check along with a column it reads.
#[test]
fn dropping_the_column_a_clock_check_reads_drops_the_check() {
    let mut connection =
        helpers::apply_default(&format!("{TOLERANT} ALTER TABLE stamped DROP COLUMN started_at;"))
            .expect("the column drops");
    insert_note(&mut connection, 1).expect("the table takes rows without the column");
}

#[test]
fn dropping_another_column_a_table_clock_check_reads_drops_the_check() {
    let mut connection = helpers::apply_default(
        "CREATE TABLE stamped (id INT PRIMARY KEY, started_at TIMESTAMPTZ, note TEXT, \
         CHECK (started_at <= now() OR note IS NOT NULL)); \
         ALTER TABLE stamped DROP COLUMN note;",
    )
    .expect("the column drops");
    let ahead = stamp(&mut connection, "+1 hour");
    insert(&mut connection, 1, Some(&ahead)).expect("the check left with the column");
}

#[test]
fn dropping_a_column_no_clock_check_reads_keeps_the_checks() {
    let mut connection =
        helpers::apply_default(&format!("{TOLERANT} ALTER TABLE stamped DROP COLUMN note;"))
            .expect("the column drops");
    let ahead = stamp(&mut connection, "+1 hour");
    refusal(insert(&mut connection, 1, Some(&ahead)));
}

/// PostgreSQL checks every row the table holds when a check arrives, which a
/// trigger cannot.
#[test]
fn a_clock_check_added_to_an_existing_table_is_refused() {
    let error = helpers::translate_default_statements(
        "CREATE TABLE stamped (id INT PRIMARY KEY); \
         ALTER TABLE stamped ADD COLUMN started_at TIMESTAMPTZ CHECK (started_at <= now());",
    )
    .expect_err("the added check is refused");
    assert!(error.contains("reads the clock"), "{error}");
}
