//! A trigger function a migration makes `SECURITY DEFINER`.
//!
//! From then on PostgreSQL runs the body as the function's owner, and the
//! invoker triggers its writes fire as that owner too, so the tables it
//! writes take what the owner may write even though their grants never
//! changed.

use diesel::{connection::SimpleConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::{Deployment, ParserDB};

diesel::table! {
    /// What `audit_entry()` records of each entry.
    audit (entry) {
        /// The entry audited.
        entry -> Integer,
        /// The role the body ran as.
        actor -> Nullable<Text>,
    }
}

diesel::table! {
    /// What `trail_audit()` records of each audit row.
    audit_trail (entry) {
        /// The entry audited.
        entry -> Integer,
        /// The role the trigger ran as.
        actor -> Nullable<Text>,
    }
}

/// `app` inserts entries and reads the rest, `auditor` may write `audit` and
/// `audit_trail`, `audit_entry()` runs as `app` and records each entry, and
/// `trail_audit()` records each audit row.
const SCHEMA: &str = "CREATE ROLE app;
     CREATE ROLE auditor;
     CREATE TABLE entries (id INT PRIMARY KEY);
     CREATE TABLE audit (entry INT NOT NULL, actor TEXT);
     CREATE TABLE audit_trail (entry INT NOT NULL, actor TEXT);
     GRANT SELECT, INSERT ON entries TO app;
     GRANT SELECT ON audit, audit_trail TO app;
     GRANT INSERT ON audit, audit_trail TO auditor;
     CREATE FUNCTION audit_entry() RETURNS trigger
     LANGUAGE plpgsql SET search_path = '' AS $$
     BEGIN
       INSERT INTO public.audit VALUES (NEW.id, current_user);
       RETURN NULL;
     END
     $$;
     CREATE TRIGGER entries_audited AFTER INSERT ON entries
       FOR EACH ROW EXECUTE FUNCTION audit_entry();
     CREATE FUNCTION trail_audit() RETURNS trigger
     LANGUAGE plpgsql SET search_path = '' AS $$
     BEGIN
       INSERT INTO public.audit_trail VALUES (NEW.entry, current_user);
       RETURN NULL;
     END
     $$;
     CREATE TRIGGER audit_trailed AFTER INSERT ON audit
       FOR EACH ROW EXECUTE FUNCTION trail_audit();";

/// Makes `audit_entry()` run as `auditor`.
const MADE_DEFINER: &str = "CREATE OR REPLACE FUNCTION audit_entry() RETURNS trigger
     LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
     BEGIN
       INSERT INTO public.audit VALUES (NEW.id, current_user);
       RETURN NULL;
     END
     $$;
     ALTER FUNCTION audit_entry() OWNER TO auditor;";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(Deployment::new("app", ["app"], []).expect("a valid deployment"))
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit")
}

fn apply(connection: &mut SqliteConnection, statements: Vec<String>) {
    for statement in statements {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
}

/// A replica of [`SCHEMA`], with the schema it holds.
fn replica() -> (SqliteConnection, ParserDB) {
    let script = Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("the schema parses")
        .translate_to_sql(&options())
        .expect("the schema translates");
    let mut connection = SqliteConnection::establish(":memory:").expect("open an in-memory SQLite");
    script.register_functions(&mut connection).expect("register the functions");
    apply(&mut connection, script.into_vec());
    let schema = Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("the schema parses")
        .build_schema()
        .expect("the schema builds");
    (connection, schema)
}

/// Applies `migration`, translated against `schema`.
fn migrate(connection: &mut SqliteConnection, schema: &ParserDB, migration: &str) {
    let statements = Pg2Sqlite::default()
        .sql(migration)
        .expect("the migration parses")
        .translate_to_sql_with_schema(schema, &options())
        .expect("the migration translates")
        .into_vec();
    apply(connection, statements);
}

#[test]
fn an_invoker_body_is_held_to_the_acting_roles_grants() {
    let (mut connection, _) = replica();
    connection.batch_execute("INSERT INTO entries VALUES (1)").expect_err("app cannot write audit");
}

#[test]
fn a_body_made_definer_writes_as_its_owner() {
    let (mut connection, schema) = replica();
    migrate(&mut connection, &schema, MADE_DEFINER);
    connection.batch_execute("INSERT INTO entries VALUES (1)").expect("auditor writes audit");
    let audited: Vec<(i32, Option<String>)> =
        audit::table.select((audit::entry, audit::actor)).load(&mut connection).expect("audit");
    assert_eq!(audited, [(1, Some("auditor".to_owned()))]);
    let trail: Vec<(i32, Option<String>)> = audit_trail::table
        .select((audit_trail::entry, audit_trail::actor))
        .load(&mut connection)
        .expect("audit_trail");
    assert_eq!(trail, [(1, Some("auditor".to_owned()))]);
}
