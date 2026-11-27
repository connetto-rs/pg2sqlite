//! Shared test helpers for pg2sqlite integration tests.

pub mod case_fold_functions;
pub mod deployment;
pub mod emitted_corpus;
pub mod jsonb_functions;
pub mod postgres;
pub mod replay;
pub mod rls;
pub mod run_translated;
pub mod statistical_aggregates;
pub mod temporal;
pub mod translate;

#[cfg(feature = "sqlitegis")]
#[allow(dead_code, reason = "used by the sqlitegis test binary alone")]
pub mod sqlitegis;

use std::{cell::RefCell, sync::Once};

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    script::{FreshFiles, HelperFunction, Script},
};
use rosetta_uuid::Uuid;
use sqlite_vec::sqlite3_vec_init;
use sqlparser::ast::Statement;

/// The managed connection a replica runs translated SQL on, sampling the
/// platform's UTC clock.
pub type Replica = pg2sqlite_runtime::ManagedConnection<pg2sqlite_runtime::JiffUtcProvider>;

use pg2sqlite_runtime::DynamicRow;
pub use pg2sqlite_runtime::{ExecutionError, SqliteAnyValue};

/// Translates `pg` SQL through
/// `Pg2Sqlite::default().sql(...).translate_to_sql(...)` and returns the
/// resulting SQLite statements as strings. Lets test files avoid the parse/
/// translate boilerplate; callers either unwrap, assert on the returned
/// `Result`, or join via `.join("\n")` for snapshot tests.
pub fn translate_pg(
    pg: &str,
    opts: &Pg2SqliteOptions,
) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    translate_script(pg, opts).map(Script::into_vec)
}

/// The script `pg` translates to under `opts`, its statements and helper
/// function bodies rendered.
pub fn translate_script(
    pg: &str,
    opts: &Pg2SqliteOptions,
) -> Result<Script<String>, pg2sqlite::errors::Error> {
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(opts)
}

/// A connection from [`establish_connection`] initialized with `script`.
///
/// # Panics
///
/// Panics naming the registration or statement SQLite refuses.
pub fn applied_script(script: &Script<String>) -> Replica {
    let mut connection = establish_connection();
    script
        .initialize_managed(&mut connection, FreshFiles::Both)
        .unwrap_or_else(|error| panic!("the script must initialize in SQLite: {error}"));
    connection
}

/// The digits of the hash naming a helper function after its helper.
const HASH_DIGITS: usize = 16;

/// `sql` with the hash in each call of a helper function written `#`, as
/// `teams_with_#()`, so an assertion names the helper a call reads without
/// pinning the hash of its body.
#[must_use]
pub fn unhashed(sql: &str) -> String {
    let mut written = String::with_capacity(sql.len());
    let mut rest = sql;
    while let Some(position) = rest.find('_') {
        written.push_str(&rest[..=position]);
        rest = &rest[position + 1..];
        let hashed = rest.as_bytes().get(..=HASH_DIGITS).is_some_and(|name| {
            name[..HASH_DIGITS].iter().all(|digit| matches!(digit, b'0'..=b'9' | b'a'..=b'f'))
                && name[HASH_DIGITS] == b'('
        });
        if hashed {
            written.push('#');
            rest = &rest[HASH_DIGITS..];
        }
    }
    written.push_str(rest);
    written
}

/// The helper function of `script` that `statement` calls as
/// `<helper>_<hash>(...)`.
///
/// # Panics
///
/// Panics when `statement` calls no function of `script` named after
/// `helper`.
pub fn called_function<'s>(
    script: &'s Script<String>,
    statement: &str,
    helper: &str,
) -> &'s HelperFunction<String> {
    let prefix = format!("{helper}_");
    statement
        .match_indices(&prefix)
        .find_map(|(position, _)| {
            let name = statement.get(position..position + prefix.len() + HASH_DIGITS)?;
            script.functions.iter().find(|function| function.name == name)
        })
        .unwrap_or_else(|| panic!("{statement} calls no function of {helper}"))
}

/// Returns `true` when `stmt` is a translated user-level statement of `kind`
/// (case-insensitive prefix match on `"SELECT"`, `"UPDATE"`, `"DELETE"`, etc.)
/// and not a `SELECT CreateSpatialIndex(...)` call emitted by GiST index
/// translation. The `CreateSpatialIndex` exclusion is a no-op for UPDATE and
/// DELETE since `CreateSpatialIndex` is always emitted as a `SELECT`.
#[must_use]
pub fn is_user_statement(stmt: &str, kind: &str) -> bool {
    stmt.to_ascii_uppercase().trim_start().starts_with(kind) && !stmt.contains("CreateSpatialIndex")
}

/// Finds the first translated user statement of `kind`. Panics with the full
/// statement list if none is found, which gives a useful error message in
/// tests that assert a specific DML kind was emitted.
pub fn user_statement_of<'a>(stmts: &'a [String], kind: &str) -> &'a String {
    stmts
        .iter()
        .find(|s| is_user_statement(s, kind))
        .unwrap_or_else(|| panic!("no user {kind} in:\n{}", stmts.join("\n")))
}

#[declare_sql_function]
extern "SQL" {
    /// Generates a UUIDv7 value as a BLOB.
    fn uuidv7() -> diesel::sql_types::Binary;
    /// Returns the current application user ID.
    fn current_app_user() -> diesel::sql_types::Binary;
    /// Returns the current application username (for current_user mapping).
    fn current_app_username() -> diesel::sql_types::Text;
    /// Returns the current user's department (for department-based RLS).
    fn current_app_department() -> diesel::sql_types::Text;
}

// IMPORTANT: The `*_rls` backing table schemas defined below are ONLY for
// testing. Real applications should define schemas for views, not backing
// tables, which are implementation details.

diesel::table! {
    /// The backing table for users (read-only via view).
    ///
    /// **Testing only** - Real applications should use the `users` view schema.
    users_rls (id) {
        id -> Binary,
        username -> Text,
        email -> Text,
    }
}

diesel::table! {
    /// The backing table for posts (writable via view with RLS).
    ///
    /// **Testing only** - Real applications should use the `posts` view schema.
    posts_rls (id) {
        id -> Binary,
        author_id -> Binary,
        title -> Text,
        content -> Nullable<Text>,
        created_by -> Binary,
    }
}

diesel::table! {
    /// The users view (RLS-filtered).
    ///
    /// **This is what real applications should use** - the view provides transparent
    /// RLS enforcement. All queries and mutations go through this view.
    users (id) {
        id -> Binary,
        username -> Text,
        email -> Text,
    }
}

diesel::table! {
    /// The posts view (RLS-filtered).
    ///
    /// **This is what real applications should use** - the view provides transparent
    /// RLS enforcement. INSTEAD OF triggers handle INSERT/UPDATE/DELETE operations.
    posts (id) {
        id -> Binary,
        author_id -> Binary,
        title -> Text,
        content -> Nullable<Text>,
        created_by -> Binary,
    }
}

diesel::joinable!(posts_rls -> users_rls (author_id));
diesel::joinable!(posts -> users (author_id));
diesel::allow_tables_to_appear_in_same_query!(users_rls, posts_rls, users, posts);

/// A user in the system.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, PartialEq, Eq)]
#[diesel(table_name = users_rls)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct User {
    /// The user's unique identifier.
    pub id: Vec<u8>,
    /// The user's username.
    pub username: String,
    /// The user's email address.
    pub email: String,
}

impl User {
    /// Creates a new user with the given details.
    #[must_use]
    pub fn new(id: Uuid, username: impl Into<String>, email: impl Into<String>) -> Self {
        Self { id: id.as_bytes().to_vec(), username: username.into(), email: email.into() }
    }

    /// Returns the user's ID as a UUID.
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        let bytes: [u8; 16] = self.id.clone().try_into().expect("Invalid UUID length");
        Uuid::from(bytes)
    }
}

/// A post created by a user.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, PartialEq, Eq)]
#[diesel(table_name = posts_rls)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Post {
    /// The post's unique identifier.
    pub id: Vec<u8>,
    /// The author's user ID.
    pub author_id: Vec<u8>,
    /// The post title.
    pub title: String,
    /// The post content (optional).
    pub content: Option<String>,
    /// The user who created this post (for RLS).
    pub created_by: Vec<u8>,
}

impl Post {
    /// Creates a new post with the given details.
    #[must_use]
    pub fn new(
        id: Uuid,
        author_id: Uuid,
        title: impl Into<String>,
        content: Option<String>,
        created_by: Uuid,
    ) -> Self {
        Self {
            id: id.as_bytes().to_vec(),
            author_id: author_id.as_bytes().to_vec(),
            title: title.into(),
            content,
            created_by: created_by.as_bytes().to_vec(),
        }
    }

    /// Returns the post's ID as a UUID.
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        let bytes: [u8; 16] = self.id.clone().try_into().expect("Invalid UUID length");
        Uuid::from(bytes)
    }
}

// Thread-local storage for the current session user ID
thread_local! {
    static SESSION_USER_ID: RefCell<Option<rosetta_uuid::Uuid>> = const { RefCell::new(None) };
    static SESSION_USERNAME: RefCell<Option<String>> = const { RefCell::new(None) };
    static SESSION_DEPARTMENT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Sets the current session user ID for RLS filtering.
pub fn set_session_user_id(user_id: &Uuid) {
    SESSION_USER_ID.with(|u| {
        *u.borrow_mut() = Some(*user_id);
    });
}

/// Sets the current session username for RLS filtering (for current_user
/// mapping).
pub fn set_session_username(username: &str) {
    SESSION_USERNAME.with(|u| {
        *u.borrow_mut() = Some(username.to_string());
    });
}

/// Sets the current session department for RLS filtering.
pub fn set_session_department(department: Option<&str>) {
    SESSION_DEPARTMENT.with(|d| {
        *d.borrow_mut() = department.map(ToString::to_string);
    });
}

/// Implementation of the current_app_username function for SQLite.
/// Returns the current username as text, or panics if not set.
fn current_app_username_impl() -> String {
    SESSION_USERNAME.with(|u| {
        (*u.borrow()).clone().expect("Session username not set - call set_session_username() first")
    })
}

/// Implementation of the current_app_department function for SQLite.
/// Returns the current department as text, or empty string if not set (mimics
/// PostgreSQL's `current_setting(name, true)`).
fn current_app_department_impl() -> String {
    SESSION_DEPARTMENT.with(|d| (*d.borrow()).clone().unwrap_or_default())
}

/// Implementation of the current_app_user function for SQLite.
/// Returns the current user ID as a blob, or panics if not set.
fn current_app_user_impl() -> rosetta_uuid::Uuid {
    SESSION_USER_ID.with(|u| {
        (*u.borrow()).expect("Session user ID not set - call set_session_user_id() first")
    })
}

/// Default options naming the jsonb and temporal functions
/// [`establish_connection`] registers.
#[must_use]
pub fn function_options() -> Pg2SqliteOptions {
    temporal::with_functions(jsonb_functions::with_functions(Pg2SqliteOptions::default()))
}

/// Opens an in-memory managed replica with foreign keys and recursive
/// triggers on, and the test session functions, `jsonb` functions, temporal
/// functions and case folds registered.
pub fn establish_connection() -> Replica {
    let mut connection =
        Replica::establish(":memory:").expect("Error connecting to in-memory SQLite");
    connection
        .batch_execute("PRAGMA foreign_keys = ON; PRAGMA recursive_triggers = ON")
        .expect("Failed to enable foreign keys and recursive triggers");
    register_session_functions(&mut connection);
    connection
}

/// Registers the test session functions, the `jsonb` functions, the temporal
/// functions and the case folds on `connection`.
pub fn register_session_functions(connection: &mut Replica) {
    let mut registration = connection.registration().expect("register on an idle connection");
    let sqlite = registration.sqlite();
    uuidv7_utils::register_impl(sqlite, Uuid::utc_v7).expect("Failed to register uuidv7");
    current_app_user_utils::register_impl(sqlite, current_app_user_impl)
        .expect("Failed to register current_app_user");
    current_app_username_utils::register_impl(sqlite, current_app_username_impl)
        .expect("Failed to register current_app_username");
    current_app_department_utils::register_impl(sqlite, current_app_department_impl)
        .expect("Failed to register current_app_department");
    jsonb_functions::register(sqlite);
    case_fold_functions::register(sqlite);
    temporal::register(sqlite);
}

/// Common query result type for counting rows.
#[derive(QueryableByName, Debug)]
pub struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub count: i64,
}

/// Translates PostgreSQL SQL text into SQLite AST statements.
pub fn translate_statements(
    sql: &str,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|err| err.to_string())?
        .translate(options)
        .map(pg2sqlite::script::Script::into_vec)
        .map_err(|err| err.to_string())
}

/// Translates PostgreSQL SQL text and returns rendered SQLite SQL.
pub fn translate_sql(sql: &str, options: &Pg2SqliteOptions) -> Result<String, String> {
    translate_statements(sql, options)
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
}

/// Translates PostgreSQL SQL text and returns output statement count.
pub fn translate_count(sql: &str, options: &Pg2SqliteOptions) -> Result<usize, String> {
    translate_statements(sql, options).map(|stmts| stmts.len())
}

/// Reverse-translates SQLite SQL text back to PostgreSQL SQL.
///
/// Uses a minimal schema with table `t` for reverse translation context.
pub fn reverse_translate_sql(sql: &str) -> Result<String, String> {
    let ddl = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT, val TEXT, key TEXT, value TEXT);";
    let translator = Pg2Sqlite::default().sql(ddl).map_err(|e| e.to_string())?;
    let schema = translator.build_schema().map_err(|e| e.to_string())?;
    let options = function_options();
    let stmts = translator.reverse_sql(sql, &schema, &options).map_err(|e| e.to_string())?;
    Ok(stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
}

/// Translates `pg` and executes every emitted statement in a fresh in-memory
/// SQLite.
///
/// The proof an emitted script is valid SQLite: the panic names the statement
/// that was rejected and the error SQLite gave for it.
///
/// # Panics
///
/// Panics when translation fails or when an emitted statement will not
/// execute.
pub fn execute_all(pg: &str, options: &Pg2SqliteOptions) {
    applied_script(&translate_script(pg, options).expect("translation should succeed"));
}

/// Applies everything `pg` emits except the query under test, then returns
/// that query once SQLite has accepted it.
///
/// The query is picked with [`user_statement_of`], so a `SELECT
/// CreateSpatialIndex(...)` emitted by a GiST index is applied as setup
/// rather than mistaken for the query the test asserts on.
///
/// # Panics
///
/// Panics when translation fails, when no user `SELECT` is emitted, or when
/// SQLite rejects any emitted statement.
pub fn prepared_user_select(pg: &str, options: &Pg2SqliteOptions) -> String {
    let script = translate_script(pg, options).expect("translation should succeed");
    let statements: Vec<&str> = script.iter().map(String::as_str).collect();
    let select = statements
        .iter()
        .position(|statement| is_user_statement(statement, "SELECT"))
        .unwrap_or_else(|| panic!("no user SELECT in:\n{}", statements.join("\n")));

    let mut connection = establish_connection();
    script
        .register_managed(&mut connection)
        .unwrap_or_else(|error| panic!("SQLite must register the helper functions: {error}"));
    for (index, statement) in statements.iter().enumerate() {
        if index == select {
            continue;
        }
        diesel::sql_query(*statement).execute(&mut connection).unwrap_or_else(|error| {
            panic!("emitted statement must execute in SQLite: {error}\n{statement}")
        });
    }
    diesel::sql_query(format!("EXPLAIN {}", statements[select]))
        .load::<diesel_dynamic_schema::dynamic_value::DynamicRow<
            diesel_dynamic_schema::dynamic_value::NamedField<run_translated::TextCell>,
        >>(&mut connection)
        .unwrap_or_else(|error| {
            panic!("SQLite must accept the query: {error}\n{}", statements[select])
        });

    statements[select].to_owned()
}

/// Registers sqlite-vec once per process, so every later connection has it.
#[allow(dead_code, reason = "used by the sqlite_vec and playground test binaries alone")]
pub fn register_sqlite_vec_once() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: `sqlite3_vec_init` is sqlite-vec's C entry point, whose
        // signature is `(db, pzErrMsg, pApi) -> int`; the transmute restores
        // that type so the C API can store and call it.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite3_vec_init as *const ()
            )));
        }
    });
}

/// Encodes an `f32` as little-endian IEEE 754 binary16.
#[allow(dead_code, reason = "used by the sqlite_vec test binary alone")]
pub fn f32_to_f16_le(value: f32) -> [u8; 2] {
    half::f16::from_f32(value).to_le_bytes()
}

// ── Shared translation helpers ─────────────────────────────────────────────

/// Translates `pg` with default options and returns the emitted statements
/// as strings.
pub fn translate_default_statements(pg: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(pg)
        .map_err(|e| e.to_string())?
        .translate(&function_options())
        .map(|script| script.into_vec().iter().map(ToString::to_string).collect())
        .map_err(|e| e.to_string())
}

/// Translates `pg` with default options and returns the emitted statements
/// joined with newlines.
pub fn translate_default_joined(pg: &str) -> Result<String, String> {
    translate_default_statements(pg).map(|stmts| stmts.join("\n"))
}

/// Translates `pg` with default options and returns the emitted statements
/// joined with newlines, or the translation error message when translation
/// is refused.
#[allow(dead_code, reason = "used by the sqlite_vec test binary alone")]
pub fn translate_default_or_err(pg: &str) -> String {
    match Pg2Sqlite::default().sql(pg).unwrap().translate(&function_options()) {
        Err(e) => e.to_string(),
        Ok(script) => {
            script.into_vec().iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
        }
    }
}

/// Translates `{prefix}\n{pg}` with default options and returns the emitted
/// statements.
pub fn translate_prefixed(prefix: &str, pg: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(&format!("{prefix}\n{pg}"))
        .map_err(|e| e.to_string())?
        .translate_to_sql(&function_options())
        .map(pg2sqlite::script::Script::into_vec)
        .map_err(|e| e.to_string())
}

/// Translates `pg` with default options and returns the emitted statements
/// alongside the collected translation warnings.
pub fn translate_with_default_warnings(
    pg: &str,
) -> Result<(Vec<String>, Vec<pg2sqlite::warnings::TranslationWarning>), pg2sqlite::errors::Error> {
    let report = Pg2Sqlite::default().sql(pg)?.translate_with_report(&function_options())?;
    let stmts = report.script.iter().map(ToString::to_string).collect();
    Ok((stmts, report.warnings))
}

/// An in-memory replica with only the `jsonb` functions and, when
/// `case_folds`, the case folds registered.
fn bare_replica(case_folds: bool) -> Replica {
    let mut connection = Replica::establish(":memory:").expect("connect");
    let mut registration = connection.registration().expect("register on an idle connection");
    jsonb_functions::register(registration.sqlite());
    temporal::register(registration.sqlite());
    if case_folds {
        case_fold_functions::register(registration.sqlite());
    }
    connection
}

/// Translates `pg` with `options` and initializes a fresh in-memory replica
/// with the script. The emitted DDL is the artifact under test.
pub fn apply_with_options(
    pg: &str,
    options: &Pg2SqliteOptions,
) -> Result<Replica, Box<dyn std::error::Error>> {
    let script = Pg2Sqlite::default().sql(pg)?.translate(options)?;
    let mut connection = bare_replica(true);
    script.initialize_managed(&mut connection, FreshFiles::Both)?;
    Ok(connection)
}

/// Translates `pg` with default options and initializes a fresh in-memory
/// replica with the script.
pub fn apply_default(pg: &str) -> Result<Replica, Box<dyn std::error::Error>> {
    apply_with_options(pg, &function_options())
}

/// Translates `pg` with the `rls_audit` audit table name and returns the
/// emitted statements joined with newlines.
pub fn translate_rls_sql(pg: &str) -> Result<String, Box<dyn std::error::Error>> {
    let options = case_fold_functions::with_functions(function_options())
        .with_rls_audit_table_name("rls_audit");
    Ok(translate_pg(pg, &options)?.join("\n"))
}

/// Translates `pg` with the `rls_audit` audit table name and initializes a
/// fresh in-memory replica with the script.
pub fn apply_rls_sql(pg: &str) -> Result<Replica, Box<dyn std::error::Error>> {
    let options = case_fold_functions::with_functions(function_options())
        .with_rls_audit_table_name("rls_audit");
    let script = translate_script(pg, &options)?;
    let mut connection = bare_replica(true);
    script.initialize_managed(&mut connection, FreshFiles::Both)?;
    Ok(connection)
}

/// Translates `pg` with `opts` and initializes a replica from
/// [`establish_connection`] with the script.
pub fn apply_translated_ddl(pg: &str, opts: &Pg2SqliteOptions) -> Replica {
    let script = Pg2Sqlite::default().sql(pg).expect("parse").translate(opts).expect("translate");
    let mut connection = establish_connection();
    script
        .initialize_managed(&mut connection, FreshFiles::Both)
        .unwrap_or_else(|error| panic!("the emitted DDL must initialize in SQLite: {error}"));
    connection
}

/// Runs every statement of `stmts` on a fresh in-memory replica and returns
/// it, panicking on the first failure.
pub fn apply_statements(stmts: &[String]) -> Replica {
    let mut conn = bare_replica(false);
    for statement in stmts {
        // Emitted DDL is not expressible via the typed DSL.
        diesel::sql_query(statement)
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
    conn
}

/// Applies every non-query statement of `stmts` to a fresh in-memory
/// connection and returns it, proving the emitted DDL runs.
///
/// # Panics
///
/// Panics when the connection cannot be opened or a statement is rejected.
pub fn apply_ddl(stmts: &[String]) -> Replica {
    let mut conn = bare_replica(false);
    for stmt in stmts.iter().filter(|s| !s.to_ascii_uppercase().trim_start().starts_with("SELECT"))
    {
        // Emitted DDL is not expressible via the typed DSL.
        diesel::sql_query(stmt)
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("emitted DDL failed: {error}\n{stmt}"));
    }
    conn
}

/// Executes each non-empty newline-separated statement of `output` against
/// an in-memory SQLite, proving the joined output runs.
pub fn execute_joined_statements(output: &str) {
    let mut connection = establish_connection();
    for statement in output.split('\n').filter(|statement| !statement.trim().is_empty()) {
        // Translated output is the artifact under test.
        diesel::sql_query(statement).execute(&mut connection).unwrap();
    }
}

/// Reverse-translates `sqlite_sql` against the schema built from `pg_ddl`
/// and returns the error message. Panics when the translation succeeds.
pub fn reverse_sql_err(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    translator.reverse_sql(sqlite_sql, &schema, &function_options()).unwrap_err().to_string()
}

/// Reverse-translates `sqlite_sql` against `schema` and returns the joined
/// statements, or the translation error.
pub fn reverse_sql_joined(
    sqlite_sql: &str,
    schema: &sql_traits::structs::ParserDB,
) -> Result<String, pg2sqlite::errors::Error> {
    Pg2Sqlite::default()
        .reverse_sql(sqlite_sql, schema, &function_options())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

/// Asserts that `rev(sqlite_sql)` succeeds, the output contains `want`, and
/// the output parses as valid PostgreSQL.
pub fn assert_reverse_emits<F>(rev: F, sqlite_sql: &str, want: &str)
where
    F: Fn(&str) -> Result<String, pg2sqlite::errors::Error>,
{
    let out = rev(sqlite_sql).unwrap_or_else(|e| {
        panic!("{sqlite_sql}\n  expected output containing {want:?}, got Err: {e}")
    });
    assert!(out.contains(want), "{sqlite_sql}\n  expected {want:?} in: {out}");
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &out)
        .unwrap_or_else(|e| {
            panic!("{sqlite_sql}\n  reverse output is not valid PostgreSQL: {e}\n{out}")
        });
}

/// Reverse-translates `sqlite_sql` against `schema`, proves the output
/// parses as PostgreSQL, and returns it.
pub fn reverse_checked(sqlite_sql: &str, schema: &sql_traits::structs::ParserDB) -> String {
    let statements = Pg2Sqlite::default()
        .reverse_sql(sqlite_sql, schema, &function_options())
        .expect("reverse translation");
    let sql = statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &sql)
        .expect("output parses as PostgreSQL");
    sql
}

/// Forward-translates `pg_sql` against `schema` and joins the emitted
/// statements with `; `.
pub fn forward_translate_against(pg_sql: &str, schema: &str) -> String {
    let translator = Pg2Sqlite::default().sql(schema).unwrap();
    let built = translator.build_schema().unwrap();
    let stmts =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, pg_sql)
            .unwrap();
    stmts
        .iter()
        .flat_map(|s| {
            pg2sqlite::prelude::Translator::translate(s, &built, &function_options()).unwrap()
        })
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Reverse-translates `sqlite_sql` against the schema built from `schema`
/// and joins the emitted statements with `; `.
pub fn reverse_translate_against(sqlite_sql: &str, schema: &str) -> String {
    let translator = Pg2Sqlite::default().sql(schema).unwrap();
    let built = translator.build_schema().unwrap();
    translator
        .reverse_sql(sqlite_sql, &built, &function_options())
        .unwrap()
        .into_iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Every row `sql` answers on `connection`, with `parameters` bound to its
/// placeholders in order, each column in its SQLite storage class.
///
/// # Errors
///
/// The managed connection's refusal or SQLite's.
pub fn query_values(
    connection: &mut Replica,
    sql: &str,
    parameters: &[SqliteAnyValue],
) -> Result<Vec<Vec<SqliteAnyValue>>, ExecutionError> {
    connection
        .query::<DynamicRow<SqliteAnyValue>>(sql, parameters)?
        .map(|row| row.map(Vec::from))
        .collect()
}

/// Evaluates `probe` on a fresh managed replica and returns the first
/// column of the first row.
pub fn evaluate_probe(probe: &str) -> SqliteAnyValue {
    let mut connection = Replica::establish(":memory:").expect("in-memory SQLite");
    query_values(&mut connection, probe, &[])
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
        .into_iter()
        .next()
        .and_then(|row| row.into_iter().next())
        .unwrap_or_else(|| panic!("emitted probe answered no value\n{probe}"))
}

/// `value` read as a number the way SQLite reads an `INTEGER` as `REAL`,
/// `None` for SQL `NULL`.
///
/// # Panics
///
/// When `value` is text or a blob.
#[must_use]
#[expect(clippy::cast_precision_loss, reason = "SQLite widens an INTEGER to REAL the same way")]
pub fn numeric_value(value: &SqliteAnyValue) -> Option<f64> {
    match *value {
        SqliteAnyValue::Null => None,
        SqliteAnyValue::Real(real) => Some(real),
        SqliteAnyValue::Integer(integer) => Some(integer as f64),
        ref other => panic!("expected a number, got {other:?}"),
    }
}

/// Translates `pg_sql` with default options and proves every emitted
/// statement runs in SQLite.
pub fn assert_translated_statements_run(pg_sql: &str) {
    let script = Pg2Sqlite::default()
        .sql(pg_sql)
        .expect("parse")
        .translate(&function_options())
        .expect("translate");
    let mut connection = establish_connection();
    script
        .initialize_managed(&mut connection, FreshFiles::Both)
        .unwrap_or_else(|error| panic!("translated statements must run in SQLite: {error}"));
}
