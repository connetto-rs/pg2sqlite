//! Replayed-connection scaffolding for the connection replay tests.

use diesel::{connection::SimpleConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, Script};

/// Translates `sql` with the default options, expecting both steps to succeed.
pub fn script(sql: &str) -> Script<String> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

/// Opens the `main.db` file under `files`.
pub fn open(files: &tempfile::TempDir) -> SqliteConnection {
    SqliteConnection::establish(files.path().join("main.db").to_str().expect("path")).expect("open")
}

/// Batch-executes `statements`, panicking with the statement that failed.
pub fn run(
    connection: &mut SqliteConnection,
    statements: impl IntoIterator<Item = impl AsRef<str>>,
) {
    for statement in statements {
        let statement = statement.as_ref();
        connection.batch_execute(statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
}

/// Runs `script` on a fresh store and returns the replayed connection.
pub fn create_and_reopen(files: &tempfile::TempDir, script: &Script<String>) -> SqliteConnection {
    {
        let mut creating = open(files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(files);
    run(&mut reopened, script.connect().expect("connection program"));
    reopened
}
