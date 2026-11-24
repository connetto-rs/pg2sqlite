//! A `SECURITY DEFINER` trigger runs its body as its owner wherever
//! row-level security on some table exempts that owner. PostgreSQL decides
//! that when the body runs, so a later change to which tables exempt the owner
//! leaves the trigger as it would have been installed after the change.

use diesel::{
    Connection, ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection,
    connection::SimpleConnection,
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    sqlite_master (name) {
        #[sql_name = "type"]
        kind -> Text,
        name -> Text,
        sql -> Nullable<Text>,
    }
}

/// The owners, the tables, and the definer function the trigger runs, whose
/// body reads `log` alone. A body that writes runs with its owner's frame
/// whatever exempts the owner, so this one does not.
fn base(owner_attributes: &str) -> String {
    format!(
        "CREATE ROLE trigger_owner NOLOGIN {owner_attributes};
         CREATE ROLE secrets_owner NOLOGIN;
         CREATE TABLE docs (id INTEGER PRIMARY KEY);
         CREATE TABLE log (id INTEGER);
         CREATE TABLE secrets (id INTEGER);
         GRANT SELECT ON log TO trigger_owner;
         CREATE FUNCTION log_doc() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN
           PERFORM count(*) FROM log WHERE id = NEW.id;
           RETURN NULL;
         END $$;
         ALTER FUNCTION log_doc() OWNER TO trigger_owner;"
    )
}

const TRIGGER: &str =
    "CREATE TRIGGER docs_log AFTER INSERT ON docs FOR EACH ROW EXECUTE FUNCTION log_doc();";

/// A dropped draft table, which ends the stretch of statements translated
/// against one schema.
const DRAFT: &str = "CREATE TABLE drafts (id INTEGER); DROP TABLE drafts;";

/// `source` translated and installed on SQLite.
fn installed(source: &str) -> SqliteConnection {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let script = Pg2Sqlite::default()
        .sql(source)
        .unwrap()
        .translate_to_sql(&options)
        .unwrap_or_else(|error| panic!("{error}\n{source}"));
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    script.register_functions(&mut connection).expect("register helper functions");
    connection.batch_execute(&script.into_vec().join(";\n")).unwrap();
    connection
}

/// The SQL of the SQLite trigger `docs_log` that `source` installs.
fn docs_log(source: &str) -> Option<String> {
    sqlite_master::table
        .filter(sqlite_master::kind.eq("trigger"))
        .filter(sqlite_master::name.eq("docs_log"))
        .select(sqlite_master::sql)
        .first(&mut installed(source))
        .unwrap()
}

/// Each change to whether some table exempts the owner, after the trigger,
/// leaves it as installing the trigger after the change does.
#[test]
fn a_definer_trigger_follows_each_change_to_its_owners_exemptions() {
    const ENABLE: &str = "ALTER TABLE secrets ENABLE ROW LEVEL SECURITY;";
    const SECRETS_OWNED: &str = "ALTER TABLE secrets OWNER TO secrets_owner;";
    let cases: [(&str, String, &str); 8] = [
        ("BYPASSRLS", String::new(), ENABLE),
        ("BYPASSRLS", ENABLE.to_owned(), "ALTER TABLE secrets DISABLE ROW LEVEL SECURITY;"),
        (
            "",
            format!("ALTER TABLE secrets OWNER TO trigger_owner; {ENABLE}"),
            "ALTER TABLE secrets FORCE ROW LEVEL SECURITY;",
        ),
        ("", format!("{SECRETS_OWNED} {ENABLE}"), "ALTER TABLE secrets OWNER TO trigger_owner;"),
        ("BYPASSRLS", ENABLE.to_owned(), "DROP TABLE secrets;"),
        ("", ENABLE.to_owned(), "ALTER ROLE trigger_owner BYPASSRLS;"),
        ("", format!("{SECRETS_OWNED} {ENABLE}"), "GRANT secrets_owner TO trigger_owner;"),
        (
            "",
            format!("{SECRETS_OWNED} {ENABLE} GRANT secrets_owner TO trigger_owner;"),
            "REVOKE secrets_owner FROM trigger_owner;",
        ),
    ];
    for (owner_attributes, setup, change) in cases {
        let base = base(owner_attributes);
        let before = docs_log(&format!("{base}{setup}{DRAFT}{TRIGGER}"));
        let changed_first = docs_log(&format!("{base}{setup}{DRAFT}{change}{TRIGGER}"));
        let changed_later = docs_log(&format!("{base}{setup}{TRIGGER}{DRAFT}{change}"));
        assert_ne!(before, changed_first, "{change} changes nothing after {setup}");
        assert_eq!(changed_later, changed_first, "{change} after {setup}");
    }
}
