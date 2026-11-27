//! With an acting role, the replica registers every function that role may
//! call by its bare name, under that name, so a client reads it in `FROM` as
//! it calls it on PostgreSQL. A scalar function answers one row of one column
//! named after it. A name the replica cannot register for one function alone,
//! and a function whose call does not translate, refuse the translation.

use diesel::{connection::SimpleConnection, prelude::*, sql_types};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::Deployment;

const NOTES: &str = "
CREATE ROLE postgres;
CREATE ROLE app;
CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);
GRANT SELECT, INSERT ON notes TO app;
CREATE FUNCTION matching_notes(term TEXT) RETURNS TABLE (id INTEGER, body TEXT)
  LANGUAGE sql STABLE AS $$ SELECT n.id, n.body FROM notes n WHERE n.body LIKE '%' || term || '%' $$;
CREATE FUNCTION note_count() RETURNS BIGINT
  LANGUAGE sql STABLE AS $$ SELECT count(*) FROM notes $$;
";

fn deployment() -> Deployment {
    Deployment::new("postgres", ["app"], []).expect("a valid deployment")
}

fn options() -> Pg2SqliteOptions {
    crate::helpers::function_options().with_deployment(deployment()).with_acting_role("app")
}

fn translated(source: &str, options: &Pg2SqliteOptions) -> pg2sqlite::script::Script<String> {
    Pg2Sqlite::default()
        .sql(source)
        .expect("parse")
        .translate_to_sql(options)
        .unwrap_or_else(|error| panic!("the schema should translate: {error}"))
}

fn refusal(source: &str) -> String {
    match Pg2Sqlite::default().sql(source).expect("parse").translate_to_sql(&options()) {
        Ok(script) => panic!("translated where a refusal was due: {:#?}", script.functions),
        Err(error) => error.to_string(),
    }
}

/// The names of the functions `script` registers.
fn registered(script: &pg2sqlite::script::Script<String>) -> Vec<&str> {
    script.functions.iter().map(|function| function.name.as_str()).collect()
}

/// A replica of `script` holding the notes `alpha` and `beta`.
fn replica(script: pg2sqlite::script::Script<String>) -> crate::helpers::Replica {
    let mut connection = crate::helpers::establish_connection();
    script.register_managed(&mut connection).expect("register the functions");
    for statement in script.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
        .batch_execute("INSERT INTO notes VALUES (1, 'alpha'), (2, 'beta');")
        .expect("seed the replica");
    connection
}

#[derive(QueryableByName, Debug, PartialEq)]
struct Note {
    #[diesel(sql_type = sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = sql_types::Text)]
    body: String,
}

#[derive(QueryableByName, Debug, PartialEq)]
struct Count {
    #[diesel(sql_type = sql_types::BigInt)]
    note_count: i64,
}

#[test]
fn a_set_returning_function_is_read_by_its_name() {
    let script = translated(NOTES, &options());
    let function = script
        .functions
        .iter()
        .find(|function| function.name == "matching_notes")
        .unwrap_or_else(|| panic!("matching_notes is registered: {:?}", registered(&script)));
    assert_eq!(function.parameters, ["term"]);
    assert_eq!(function.columns, ["id", "body"]);
    let mut connection = replica(script);
    let notes: Vec<Note> = diesel::sql_query("SELECT id, body FROM matching_notes(?) ORDER BY id")
        .bind::<sql_types::Text, _>("ph")
        .load(&mut connection)
        .expect("read matching_notes");
    assert_eq!(notes, [Note { id: 1, body: "alpha".to_owned() }]);
}

#[test]
fn a_scalar_function_answers_one_row_named_after_it() {
    let script = translated(NOTES, &options());
    let function = script
        .functions
        .iter()
        .find(|function| function.name == "note_count")
        .unwrap_or_else(|| panic!("note_count is registered: {:?}", registered(&script)));
    assert!(function.parameters.is_empty(), "{:?}", function.parameters);
    assert_eq!(function.columns, ["note_count"]);
    let mut connection = replica(script);
    let counts: Vec<Count> = diesel::sql_query("SELECT note_count FROM note_count()")
        .load(&mut connection)
        .expect("read note_count");
    assert_eq!(counts, [Count { note_count: 2 }]);
}

/// A call types each parameter as the function declares it, so a temporal
/// parameter reads as its type in a request whose zone the deployment leaves
/// open.
#[test]
fn a_parameter_takes_its_declared_type() {
    let script = translated(
        &format!(
            "{NOTES}CREATE FUNCTION stamped(stamp TIMESTAMPTZ, label TEXT) RETURNS TEXT LANGUAGE \
             sql STABLE AS $$ SELECT CASE WHEN stamp > TIMESTAMPTZ '2020-01-01 00:00:00+00' THEN \
             label END $$;"
        ),
        &options(),
    );
    assert!(registered(&script).contains(&"stamped"), "{:?}", registered(&script));
}

#[test]
fn a_translation_naming_no_acting_role_registers_none() {
    let script = translated(NOTES, &Pg2SqliteOptions::default().with_deployment(deployment()));
    assert!(script.functions.is_empty(), "{:?}", registered(&script));
}

#[test]
fn overloads_are_refused_naming_each_signature() {
    let error = refusal(&format!(
        "{NOTES}CREATE FUNCTION matching_notes(term TEXT, below INTEGER) RETURNS TABLE (id \
         INTEGER, body TEXT) LANGUAGE sql STABLE AS $$ SELECT n.id, n.body FROM notes n WHERE \
         n.id < below $$;"
    ));
    assert!(
        error.contains("matching_notes(text)") && error.contains("matching_notes(text, integer)"),
        "{error}"
    );
}

/// A `VOLATILE` function may write, which the replica cannot host, and a
/// schema keeps such a function from clients by revoking `EXECUTE`.
#[test]
fn a_function_whose_call_does_not_translate_is_refused_naming_it() {
    const TOUCH: &str = "CREATE FUNCTION touched() RETURNS SETOF INTEGER LANGUAGE sql AS $$ \
                         SELECT id FROM notes $$;";
    let error = refusal(&format!("{NOTES}{TOUCH}"));
    assert!(error.contains("touched") && error.contains("app"), "{error}");
    let script = translated(
        &format!("{NOTES}{TOUCH}REVOKE EXECUTE ON FUNCTION touched() FROM PUBLIC;"),
        &options(),
    );
    assert!(!registered(&script).contains(&"touched"), "{:?}", registered(&script));
}

#[test]
fn a_function_in_another_language_is_refused_naming_it() {
    let error = refusal(&format!(
        "{NOTES}CREATE FUNCTION answer() RETURNS INTEGER LANGUAGE plpgsql STABLE AS $$ BEGIN \
         RETURN 42; END $$;"
    ));
    assert!(error.contains("answer") && error.contains("app"), "{error}");
}

/// A policy helper the role must execute stays out of the client's API when
/// its schema is off the role's `search_path`.
#[test]
fn a_function_off_the_search_path_is_not_registered() {
    let script = translated(
        &format!(
            "{NOTES}CREATE SCHEMA authz; GRANT USAGE ON SCHEMA authz TO app;
             CREATE FUNCTION authz.can_read(note INTEGER) RETURNS BOOLEAN LANGUAGE sql AS $$ \
             SELECT note > 0 $$;"
        ),
        &options(),
    );
    assert!(!registered(&script).contains(&"can_read"), "{:?}", registered(&script));
}

/// SQLite reads a table ahead of a table-valued function of the same name.
#[test]
fn a_function_sharing_a_relation_name_is_refused() {
    let error = refusal(&format!(
        "{NOTES}CREATE TABLE note_count (total BIGINT); GRANT SELECT ON note_count TO app;"
    ));
    assert!(error.contains("note_count"), "{error}");
}
