//! A fresh main file initializes the schemas, types and domains of a source,
//! which belong to no file and keep their final form.

use diesel::prelude::*;
use pg2sqlite::prelude::FreshFiles;

use crate::helpers::replay::{open, run, script};

diesel::table! {
    tickets (id) { id -> Integer, state -> Text, }
}

#[test]
fn a_fresh_main_file_runs_the_full_initialization_of_schemas_and_types() {
    let script = script(
        "CREATE SCHEMA billing;
         CREATE TYPE billing.state AS ENUM ('open', 'closed');
         CREATE TABLE tickets (id INT PRIMARY KEY, state billing.state NOT NULL);
         ALTER TYPE billing.state ADD VALUE 'archived';",
    );
    let initialization: Vec<&String> =
        script.initialize(FreshFiles::Main).expect("main-file initialization").collect();
    assert_eq!(initialization, script.iter().collect::<Vec<_>>());
    let directory = tempfile::TempDir::new().expect("files");
    let mut connection = open(&directory);
    run(&mut connection, initialization);
    diesel::insert_into(tickets::table)
        .values((tickets::id.eq(1), tickets::state.eq("archived")))
        .execute(&mut connection)
        .expect("the added label is accepted");
}

#[test]
fn a_fresh_main_file_initializes_renamed_and_dropped_schemas_and_types() {
    for source in [
        "CREATE SCHEMA a; ALTER SCHEMA a RENAME TO b;",
        "CREATE SCHEMA a; DROP SCHEMA a;",
        "CREATE TYPE k AS ENUM ('x'); CREATE TABLE t (v k); ALTER TYPE k RENAME TO k2;",
        "CREATE TYPE k AS ENUM ('x'); DROP TYPE k;",
        "CREATE DOMAIN d AS INT CHECK (VALUE > 0);",
        "CREATE DOMAIN d AS INT; DROP DOMAIN d;",
    ] {
        let script = script(source);
        let initialization: Vec<&String> = script
            .initialize(FreshFiles::Main)
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .collect();
        assert_eq!(initialization, script.iter().collect::<Vec<_>>(), "{source}");
    }
}
