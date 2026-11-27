//! A write naming a view PostgreSQL cannot rewrite onto the relation it reads,
//! which no `INSTEAD OF` trigger takes, fails on the replica when it runs with
//! PostgreSQL 18's `55000` and message.

use diesel::connection::SimpleConnection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, postgres_error};
use sqlstate::pg;

const SCHEMA: &str = "CREATE TABLE base (id INTEGER PRIMARY KEY, score INTEGER NOT NULL);
    CREATE VIEW v AS SELECT DISTINCT id, score FROM base;
    INSERT INTO base VALUES (1, 5);";

/// A replica holding `schema`, translated and applied.
fn replica(schema: &str) -> crate::helpers::Replica {
    let mut connection = crate::helpers::establish_connection();
    let script = Pg2Sqlite::default()
        .sql(schema)
        .expect("the schema parses")
        .translate(&Pg2SqliteOptions::default())
        .expect("the schema translates");
    script.register_managed(&mut connection).expect("the helpers register");
    for statement in script.iter() {
        connection.batch_execute(&format!("{statement};")).expect("the schema applies");
    }
    connection
}

/// The SQLSTATE and message PostgreSQL reports for `write`, run on the
/// replica of `schema`, or `None` when it runs.
fn refusal(schema: &str, write: &str) -> Option<(sqlstate::SqlState, String)> {
    let mut connection = replica(schema);
    let built = Pg2Sqlite::default().sql(schema).expect("the schema parses").build_schema();
    let translated = Pg2Sqlite::default()
        .sql(write)
        .expect("the write parses")
        .translate_to_sql_with_schema(
            &built.expect("the schema builds"),
            &Pg2SqliteOptions::default(),
        )
        .expect("the write translates")
        .into_vec()
        .join(";\n");
    let error = connection.batch_execute(&translated).err()?;
    let diesel::result::Error::DatabaseError(_, information) = &error else {
        panic!("{error:?}");
    };
    let refused = postgres_error(information.message())
        .unwrap_or_else(|| panic!("not a PostgreSQL refusal: {}", information.message()));
    Some((refused.sqlstate(), refused.message().to_owned()))
}

#[test]
fn each_command_is_refused_as_postgresql_refuses_it() {
    for (write, message) in [
        ("INSERT INTO v VALUES (2, 6)", r#"cannot insert into view "v""#),
        ("UPDATE v SET score = 7", r#"cannot update view "v""#),
        ("DELETE FROM v", r#"cannot delete from view "v""#),
    ] {
        assert_eq!(
            refusal(SCHEMA, write),
            Some((pg::OBJECT_NOT_IN_PREREQUISITE_STATE, message.to_owned())),
            "{write}"
        );
    }
}

/// An `INSTEAD OF UPDATE` trigger created after the view takes the updates,
/// and the inserts stay refused.
#[test]
fn an_instead_of_trigger_takes_the_writes_of_its_command() {
    let schema = format!(
        "{SCHEMA}
         CREATE FUNCTION rescore() RETURNS TRIGGER LANGUAGE plpgsql AS $$
         BEGIN UPDATE base SET score = NEW.score WHERE id = OLD.id; RETURN NEW; END $$;
         CREATE TRIGGER v_rescore INSTEAD OF UPDATE ON v FOR EACH ROW EXECUTE FUNCTION rescore();"
    );
    assert_eq!(refusal(&schema, "UPDATE v SET score = 7"), None);
    assert_eq!(
        refusal(&schema, "INSERT INTO v VALUES (2, 6)"),
        Some((pg::OBJECT_NOT_IN_PREREQUISITE_STATE, r#"cannot insert into view "v""#.to_owned()))
    );
}

/// PostgreSQL refuses the write when a trigger body runs it, so the trigger
/// translates and the write firing it fails.
#[test]
fn a_trigger_writing_the_view_fails_when_it_fires() {
    let schema = format!(
        "{SCHEMA}
         CREATE TABLE resets (id INTEGER PRIMARY KEY);
         CREATE FUNCTION wipe() RETURNS TRIGGER LANGUAGE plpgsql AS $$
         BEGIN DELETE FROM v; RETURN NEW; END $$;
         CREATE TRIGGER resets_wipe AFTER INSERT ON resets FOR EACH ROW EXECUTE FUNCTION wipe();"
    );
    assert_eq!(
        refusal(&schema, "INSERT INTO resets VALUES (1)"),
        Some((pg::OBJECT_NOT_IN_PREREQUISITE_STATE, r#"cannot delete from view "v""#.to_owned()))
    );
}
