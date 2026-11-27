//! Apply helpers for the sqlite_vec test binary, where the emitted SQL runs
//! against the native vector extension.

use diesel::{Connection, connection::SimpleConnection};

use crate::helpers::Replica;

/// Opens an in-memory managed replica with the native vector extension.
pub fn vec_connection() -> Replica {
    crate::helpers::register_sqlite_vec_once();
    Replica::establish(":memory:").expect("in-memory SQLite")
}

/// Opens a connection with the sqlite-vec extension and executes every
/// statement of `stmts` on it.
///
/// # Panics
///
/// Panics on the first statement SQLite rejects.
pub fn apply_vec_statements(stmts: &[String]) -> Replica {
    let mut connection = vec_connection();
    for statement in stmts {
        connection
            .batch_execute(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted statement failed: {error}\n{statement}"));
    }
    connection
}

/// Executes every statement of `stmts` except the `vec0` DDL on a plain
/// in-memory connection, which does not carry the sqlite-vec extension the
/// `vec0` statements need.
///
/// # Panics
///
/// Panics on the first statement SQLite rejects.
pub fn apply_statements_without_vec0(stmts: &[String]) -> Replica {
    let mut connection = Replica::establish(":memory:").expect("in-memory SQLite");
    for statement in stmts.iter().filter(|s| !s.to_ascii_uppercase().contains("VEC0")) {
        connection
            .batch_execute(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted statement failed: {error}\n{statement}"));
    }
    connection
}
