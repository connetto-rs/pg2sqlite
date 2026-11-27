//! Replayed-connection scaffolding for the connection replay tests.

use diesel::{Connection, connection::SimpleConnection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, Script},
    script::FreshFiles,
};

use super::Replica;

/// Translates `sql` with the default options, expecting both steps to succeed.
pub fn script(sql: &str) -> Script<String> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

/// Opens the `main.db` file under `files`.
pub fn open(files: &tempfile::TempDir) -> Replica {
    Replica::establish(files.path().join("main.db").to_str().expect("path")).expect("open")
}

/// Batch-executes `statements`, panicking with the statement that failed.
pub fn run(connection: &mut Replica, statements: impl IntoIterator<Item = impl AsRef<str>>) {
    for statement in statements {
        let statement = statement.as_ref();
        connection.batch_execute(statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
}

/// Initializes a fresh store with `script` and returns a reopened
/// connection that replayed the script's connection part.
pub fn create_and_reopen(files: &tempfile::TempDir, script: &Script<String>) -> Replica {
    script.initialize_managed(&mut open(files), FreshFiles::Both).expect("initialize the store");
    let mut reopened = open(files);
    script.connect_managed(&mut reopened).expect("replay the connection program");
    reopened
}
