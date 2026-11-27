//! Transaction boundaries required by the managed clock owner.

use diesel::{
    Connection, RunQueryDsl,
    connection::{AnsiTransactionManager, SimpleConnection, TransactionManager},
    prelude::*,
    sqlite::SqliteConnection,
};

diesel::table! {
    /// Rows satisfying the deferred reference.
    clock_parents (identifier) {
        /// The referenced key.
        identifier -> Integer,
    }
}

diesel::table! {
    /// Rows checked at commit.
    clock_children (identifier) {
        /// The deferred parent key.
        identifier -> Integer,
    }
}

/// A failed commit rolls back its writes and closes the transaction.
#[test]
fn failed_commit_rolls_back_pending_writes() {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    connection
        .batch_execute(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE clock_parents (identifier INTEGER PRIMARY KEY);
             CREATE TABLE clock_children (
                 identifier INTEGER PRIMARY KEY REFERENCES clock_parents(identifier)
                 DEFERRABLE INITIALLY DEFERRED
             );",
        )
        .expect("create deferred constraint");
    AnsiTransactionManager::begin_transaction(&mut connection).expect("begin transaction");
    diesel::insert_into(clock_children::table)
        .values(clock_children::identifier.eq(1))
        .execute(&mut connection)
        .expect("queue missing parent");
    AnsiTransactionManager::commit_transaction(&mut connection)
        .expect_err("missing parent prevents commit");
    let pending: i64 =
        clock_children::table.count().get_result(&mut connection).expect("read pending child");
    assert_eq!(pending, 0, "failed commit rolls back pending writes");
    assert_eq!(
        AnsiTransactionManager::transaction_manager_status_mut(&mut connection)
            .transaction_depth()
            .expect("read transaction depth"),
        None,
    );
    AnsiTransactionManager::begin_transaction_sql(&mut connection, "BEGIN")
        .expect("start a fresh transaction");
    AnsiTransactionManager::rollback_transaction(&mut connection).expect("rollback transaction");
}

/// A completed transaction permits another transaction through the same owner.
#[test]
fn explicit_commit_keeps_diesel_transaction_state_synchronized() {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    AnsiTransactionManager::begin_transaction(&mut connection).expect("begin transaction");
    AnsiTransactionManager::commit_transaction(&mut connection).expect("managed commit");
    AnsiTransactionManager::begin_transaction_sql(&mut connection, "BEGIN")
        .expect("start transaction after successful commit");
    AnsiTransactionManager::rollback_transaction(&mut connection).expect("rollback transaction");
}

/// SQLite's conflict rollback leaves Diesel refusing every statement until
/// the caller rolls back, which then unwinds without issuing SQL.
#[test]
fn implicit_rollback_holds_diesel_until_rolled_back() {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    connection
        .batch_execute(
            "CREATE TABLE clock_children (identifier INTEGER PRIMARY KEY ON CONFLICT ROLLBACK)",
        )
        .expect("create rollback constraint");
    AnsiTransactionManager::begin_transaction(&mut connection).expect("begin transaction");
    diesel::insert_into(clock_children::table)
        .values(clock_children::identifier.eq(1))
        .execute(&mut connection)
        .expect("insert first row");
    diesel::insert_into(clock_children::table)
        .values(clock_children::identifier.eq(1))
        .execute(&mut connection)
        .expect_err("duplicate rolls back transaction");
    assert!(matches!(
        clock_children::table.count().get_result::<i64>(&mut connection),
        Err(diesel::result::Error::TransactionRolledBackByDatabase)
    ));
    AnsiTransactionManager::rollback_transaction(&mut connection)
        .expect("unwind the rolled back transaction");
    assert_eq!(
        AnsiTransactionManager::transaction_manager_status_mut(&mut connection)
            .transaction_depth()
            .expect("read transaction depth"),
        None,
    );
    let pending: i64 =
        clock_children::table.count().get_result(&mut connection).expect("read rolled back rows");
    assert_eq!(pending, 0);
    AnsiTransactionManager::begin_transaction_sql(&mut connection, "BEGIN")
        .expect("begin after implicit rollback");
    AnsiTransactionManager::rollback_transaction(&mut connection).expect("rollback transaction");
}
