//! Fixture application for the SQL files under `tests/fixtures`.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Translates the `sql` fixture with `options`, turns foreign keys on, and
/// applies every emitted statement to a fresh in-memory connection.
pub fn apply_fixture(
    sql: &str,
    options: &Pg2SqliteOptions,
) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default().sql(sql)?.translate(options)?.into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");
    // Vendor pragma the typed DSL cannot express.
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    for statement in &translated {
        // Emitted DDL is not expressible via the typed DSL.
        diesel::sql_query(&statement.to_string()).execute(&mut connection)?;
    }
    Ok(connection)
}
