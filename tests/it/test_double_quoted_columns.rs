//! A name PostgreSQL reads as a column stays a column on the replica.
//!
//! SQLite reads a double-quoted name that no column in scope holds as a
//! string literal. The policy view leaves out a column the acting role may not
//! read, so `"body"` would answer the text `'body'` there, and a write would
//! change rows with values the statement never meant. Each statement here is
//! refused at translation with the SQLSTATE and message PostgreSQL 18 answers,
//! and the backing table keeps its rows.

use diesel::{
    Connection, ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection,
    connection::SimpleConnection,
};
use pg2sqlite::prelude::{Error, Pg2Sqlite, Pg2SqliteOptions, RefusalCategory, SqlState};
use sql_traits::structs::ParserDB;
use sqlstate::pg;

use crate::helpers::deployment::deployment;

diesel::table! {
    docs_rls (id) {
        id -> Integer,
        owner -> Text,
        body -> Nullable<Text>,
    }
}

/// `app` reads `id` and `owner` of `docs`, inserts them, updates `owner` and
/// `body`, and deletes, so `body` is unreadable and update-only.
const SCHEMA: &str = "
CREATE ROLE app NOLOGIN;
CREATE TABLE docs (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, body TEXT);
CREATE TABLE notes (id INTEGER PRIMARY KEY, doc_id INTEGER, note TEXT);
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
CREATE POLICY open ON docs USING (true);
GRANT SELECT (id, owner), INSERT (id, owner), UPDATE (owner, body), DELETE ON docs TO app;
GRANT SELECT, INSERT ON notes TO app;
";

const ROWS: [(i32, &str, Option<&str>); 2] = [(1, "alice", Some("a")), (2, "bob", Some("b"))];

fn as_app() -> Pg2SqliteOptions {
    without_acting_role().with_deployment(deployment(["app"], [])).with_acting_role("app")
}

fn without_acting_role() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit")
}

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(SCHEMA).unwrap().build_schema().unwrap()
}

/// The replica of [`SCHEMA`] as `app` sees it, on SQLite with its default
/// flags, holding [`ROWS`].
fn replica() -> SqliteConnection {
    let script = Pg2Sqlite::default().sql(SCHEMA).unwrap().translate_to_sql(&as_app()).unwrap();
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    connection.batch_execute(&script.into_vec().join(";\n")).unwrap();
    diesel::insert_into(docs_rls::table)
        .values(
            ROWS.map(|(id, owner, body)| {
                (docs_rls::id.eq(id), docs_rls::owner.eq(owner), docs_rls::body.eq(body))
            })
            .as_slice(),
        )
        .execute(&mut connection)
        .unwrap();
    connection
}

fn backing_rows(connection: &mut SqliteConnection) -> Vec<(i32, String, Option<String>)> {
    docs_rls::table.order(docs_rls::id).load(connection).unwrap()
}

/// The error translating `statement` as `options` answers. A statement that
/// translates instead runs on a fresh replica, so the panic shows what the
/// replica answered and the rows it left.
fn refusal(statement: &str, options: &Pg2SqliteOptions) -> Error {
    match Pg2Sqlite::default()
        .sql(statement)
        .unwrap()
        .translate_to_sql_with_schema(&schema(), options)
    {
        Err(error) => error,
        Ok(script) => {
            let mut connection = replica();
            let ran = connection.batch_execute(&script.into_vec().join(";\n"));
            panic!(
                "`{statement}` translated where PostgreSQL refuses it, the replica answered \
                 {ran:?} and holds {:?}",
                backing_rows(&mut connection)
            );
        }
    }
}

/// Asserts that `statement`, run as `app`, is refused with `sqlstate` and
/// `message`, and that the backing table keeps [`ROWS`].
fn assert_refused(statement: &str, sqlstate: SqlState, message: &str) {
    let error = refusal(statement, &as_app());
    let refused = error.postgres_error().unwrap_or_else(|| panic!("`{statement}`: {error}"));
    assert_eq!((refused.sqlstate(), refused.message()), (sqlstate, message), "`{statement}`");
    let mut connection = replica();
    let expected: Vec<_> = ROWS
        .iter()
        .map(|(id, owner, body)| (*id, (*owner).to_owned(), body.map(str::to_owned)))
        .collect();
    assert_eq!(backing_rows(&mut connection), expected);
}

fn assert_denied(statement: &str) {
    assert_refused(statement, pg::INSUFFICIENT_PRIVILEGE, "permission denied for table docs");
}

/// Runs `statement` as `app` on the replica, returning what it leaves in the
/// backing table.
fn run(statement: &str) -> Vec<(i32, String, Option<String>)> {
    let script = Pg2Sqlite::default()
        .sql(statement)
        .unwrap()
        .translate_to_sql_with_schema(&schema(), &as_app())
        .unwrap_or_else(|error| panic!("`{statement}`: {error}"));
    let mut connection = replica();
    connection.batch_execute(&script.into_vec().join(";\n")).unwrap();
    backing_rows(&mut connection)
}

#[test]
fn reading_an_unreadable_column_is_denied() {
    assert_denied(r#"SELECT "id", "body" FROM docs"#);
    assert_denied(
        r#"SELECT id FROM notes WHERE doc_id IN (SELECT id FROM docs WHERE "body" = 'a')"#,
    );
    assert_denied(r#"SELECT n.note FROM notes n JOIN docs d ON d."body" = n.note"#);
}

#[test]
fn writing_with_an_unreadable_column_is_denied() {
    assert_denied(r#"UPDATE docs SET owner = "body" WHERE id = 1"#);
    assert_denied(r#"UPDATE docs SET owner = 'x' WHERE "body" = 'body'"#);
    assert_denied(r#"UPDATE docs SET owner = 'x' WHERE id = 1 RETURNING "body""#);
    assert_denied(r#"INSERT INTO docs (id, owner) SELECT id + 10, "body" FROM docs"#);
    assert_denied(r#"DELETE FROM docs WHERE "body" = 'body'"#);
}

#[test]
fn writing_an_unreadable_column_without_its_write_privilege_is_denied() {
    assert_denied("INSERT INTO docs (id, owner, body) VALUES (3, 'c', 'z')");
    assert_denied("INSERT INTO docs VALUES (3, 'c', 'z')");
}

/// The replica holds a readable column, so its write guards answer the
/// missing privilege.
#[test]
fn writing_a_readable_column_without_its_write_privilege_is_denied_on_the_replica() {
    let script = Pg2Sqlite::default()
        .sql("UPDATE docs SET id = 5")
        .unwrap()
        .translate_to_sql_with_schema(&schema(), &as_app())
        .unwrap();
    let mut connection = replica();
    let refused = connection.batch_execute(&script.into_vec().join(";\n")).unwrap_err();
    assert!(refused.to_string().contains("permission denied for table docs"), "{refused}");
    let ids: Vec<i32> = backing_rows(&mut connection).into_iter().map(|row| row.0).collect();
    assert_eq!(ids, [1, 2]);
}

#[test]
fn a_column_no_relation_holds_does_not_exist() {
    let undefined = |statement, message| assert_refused(statement, pg::UNDEFINED_COLUMN, message);
    undefined(r#"SELECT "id", "nosuch" FROM docs"#, r#"column "nosuch" does not exist"#);
    undefined(r#"SELECT d."nosuch" FROM docs d"#, "column d.nosuch does not exist");
    undefined(
        r#"INSERT INTO docs (id, owner) VALUES (3, "body")"#,
        r#"column "body" does not exist"#,
    );
    undefined(
        r#"INSERT INTO docs (id, owner) VALUES (3, 'c') RETURNING "nosuch""#,
        r#"column "nosuch" does not exist"#,
    );
    undefined(
        r#"UPDATE docs SET "nosuch" = 1"#,
        r#"column "nosuch" of relation "docs" does not exist"#,
    );
    assert_refused(
        r#"SELECT x."id" FROM docs"#,
        pg::UNDEFINED_TABLE,
        r#"missing FROM-clause entry for table "x""#,
    );
    assert_refused(
        r#"SELECT "id" FROM docs, notes"#,
        pg::AMBIGUOUS_COLUMN,
        r#"column reference "id" is ambiguous"#,
    );
}

/// PostgreSQL folds an unquoted name to lower case before it looks the name
/// up, and names it folded when the lookup fails.
#[test]
fn an_unquoted_name_is_reported_folded() {
    let undefined = |statement, message| assert_refused(statement, pg::UNDEFINED_COLUMN, message);
    undefined("SELECT NoSuch FROM docs", r#"column "nosuch" does not exist"#);
    undefined(r#"SELECT "NoSuch" FROM docs"#, r#"column "NoSuch" does not exist"#);
    undefined("SELECT D.NoSuch FROM docs d", "column d.nosuch does not exist");
    undefined("UPDATE docs SET NoSuch = 1", r#"column "nosuch" of relation "docs" does not exist"#);
    assert_refused(
        "SELECT X.id FROM docs",
        pg::UNDEFINED_TABLE,
        r#"missing FROM-clause entry for table "x""#,
    );
    assert_refused(
        "SELECT ID FROM docs, notes",
        pg::AMBIGUOUS_COLUMN,
        r#"column reference "id" is ambiguous"#,
    );
}

/// A derived table without `LATERAL` sees none of the `FROM` items beside it.
#[test]
fn a_derived_table_without_lateral_does_not_see_its_siblings() {
    assert_refused(
        r#"SELECT * FROM docs d, (SELECT "id") s"#,
        pg::UNDEFINED_COLUMN,
        r#"column "id" does not exist"#,
    );
    assert_refused(
        "SELECT * FROM docs d, (SELECT d.id) s",
        pg::UNDEFINED_TABLE,
        r#"invalid reference to FROM-clause entry for table "d""#,
    );
    let lateral = refusal(r#"SELECT * FROM docs d, LATERAL (SELECT d."id") s"#, &as_app());
    assert!(lateral.postgres_error().is_none(), "LATERAL sees d, {lateral}");
}

/// Without an acting role every column is readable, and a name no relation
/// holds is refused all the same.
#[test]
fn a_column_no_relation_holds_does_not_exist_without_an_acting_role() {
    let error = refusal(r#"SELECT "nosuch" FROM docs"#, &without_acting_role());
    let refused = error.postgres_error().unwrap_or_else(|| panic!("{error}"));
    assert_eq!(refused.sqlstate(), pg::UNDEFINED_COLUMN);
}

/// PostgreSQL writes a column the role may update but not read, and the
/// replica cannot, since the policy view holds only what the role reads.
#[test]
fn writing_a_write_only_column_is_refused_as_unrepresentable() {
    let error = refusal("UPDATE docs SET body = 'x' WHERE id = 1", &as_app());
    let Error::TranslationRefusal(refusal) = &error else { panic!("{error}") };
    assert_eq!(refusal.category(), RefusalCategory::UnrepresentableSemantics);
    assert!(refusal.detail().contains("body"), "{error}");
}

#[test]
fn readable_columns_resolve() {
    assert_eq!(
        run(r#"UPDATE docs SET owner = 'x' WHERE "id" = 1"#)[..1],
        [(1, "x".to_owned(), Some("a".to_owned()))]
    );
    run("INSERT INTO notes (id, doc_id, note) VALUES (1, 1, DEFAULT)");
}

/// A wildcard answers the readable columns on the replica, where PostgreSQL
/// refuses it. That divergence is accepted, so it is not refused here.
#[test]
fn wildcards_answer_the_readable_columns() {
    let mut connection = replica();
    for statement in ["SELECT * FROM docs", "SELECT d.* FROM docs d"] {
        let script = Pg2Sqlite::default()
            .sql(statement)
            .unwrap()
            .translate_to_sql_with_schema(&schema(), &as_app())
            .unwrap_or_else(|error| panic!("`{statement}`: {error}"));
        connection.batch_execute(&script.into_vec().join(";\n")).unwrap();
    }
}
