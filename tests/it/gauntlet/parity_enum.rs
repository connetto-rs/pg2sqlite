//! Parity for columns of an enum type: the labels a column admits, the
//! declaration order every comparison, sort and extremum follows, and the
//! label a read returns.
//!
//! The probes are PostgreSQL text the translator reads and SQLite text it
//! writes, so both sides run through `sql_query`. Binds are untyped, since a
//! diesel `Text` bind is a `text` parameter to PostgreSQL, which has no
//! operator against an enum.

use diesel::{
    pg::PgConnection,
    prelude::*,
    sql_types::{Nullable, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite::prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;

use crate::helpers::{establish_connection, postgres as postgres_harness};

/// Two enums whose text order differs from their declaration order, a
/// composite key reaching an enum column, a CHECK and a generated column
/// comparing by declaration order, and a partial unique index filtered by a
/// label.
const SOURCE: &str = "
CREATE TYPE role AS ENUM ('reader', 'writer', 'admin');
CREATE TYPE kind AS ENUM ('organization', 'folder', 'project');
CREATE TABLE resources (id INTEGER PRIMARY KEY, kind kind NOT NULL, UNIQUE (id, kind));
CREATE TABLE grants (
    id INTEGER PRIMARY KEY,
    resource_id INTEGER NOT NULL,
    resource_kind kind NOT NULL CHECK (resource_kind IN ('folder', 'project')),
    role role NOT NULL,
    max_role role,
    shareable BOOLEAN GENERATED ALWAYS AS (role < 'admin') STORED,
    FOREIGN KEY (resource_id, resource_kind) REFERENCES resources (id, kind),
    CHECK (resource_kind = 'folder' OR role < 'admin')
);
CREATE UNIQUE INDEX one_admin_per_resource ON grants (resource_id) WHERE role = 'admin';
INSERT INTO resources (id, kind) VALUES (1, 'folder'), (2, 'project'), (3, 'organization');
INSERT INTO grants (id, resource_id, resource_kind, role, max_role) VALUES
    (1, 1, 'folder', 'admin', 'admin'),
    (2, 1, 'folder', 'reader', 'writer'),
    (3, 2, 'project', 'writer', NULL),
    (4, 2, 'project', 'reader', 'admin'),
    (5, 1, 'folder', 'writer', 'reader');
";

mod schema {
    diesel::table! {
        grants (id) {
            id -> Integer,
            role -> Text,
        }
    }
}

/// One text column of a probe's row.
#[derive(QueryableByName)]
struct Cell {
    #[diesel(sql_type = Nullable<Text>)]
    v: Option<String>,
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json)
}

/// The same source applied to both engines, and the schema the SQLite side
/// translates later statements against.
struct Replicas {
    postgres: PgConnection,
    sqlite: SqliteConnection,
    schema: ParserDB,
}

impl Replicas {
    fn new(source: &str) -> Self {
        let mut postgres = postgres_harness::fresh_database();
        postgres_harness::apply(&mut postgres, source)
            .unwrap_or_else(|error| panic!("PostgreSQL refused the source: {error}"));
        let parsed = Pg2Sqlite::default().sql(source).expect("parse");
        let mut sqlite = establish_connection();
        // The translator's own output, which no typed query builder expresses.
        for statement in parsed.translate(&options()).expect("translate").into_vec() {
            diesel::sql_query(statement.to_string())
                .execute(&mut sqlite)
                .unwrap_or_else(|error| panic!("SQLite refused {statement}: {error}"));
        }
        let schema = parsed.build_schema().expect("schema");
        Self { postgres, sqlite, schema }
    }

    /// The SQLite statement one PostgreSQL statement translates to.
    fn translated(&self, statement: &str) -> Result<String, pg2sqlite::errors::Error> {
        let translated = Pg2Sqlite::default()
            .sql(statement)?
            .translate_with_schema(&self.schema, &options())?
            .into_vec();
        Ok(translated.last().expect("one statement").to_string())
    }

    /// Runs `probe`, whose one column is `v`, on both engines with `bound`
    /// as its text parameters, and asserts they answer the same rows.
    fn assert_same_rows(&mut self, probe: &str, bound: &[&str]) -> Vec<Option<String>> {
        let postgres = bound
            .iter()
            .fold(diesel::sql_query(probe).into_boxed(), |query, value| {
                query.bind::<Text, _>(value.to_string())
            })
            .load::<Cell>(&mut self.postgres)
            .unwrap_or_else(|error| panic!("PostgreSQL refused {probe}: {error}"));
        let translated = self
            .translated(probe)
            .unwrap_or_else(|error| panic!("translating {probe} failed: {error}"));
        let sqlite = bound
            .iter()
            .fold(diesel::sql_query(&translated).into_boxed(), |query, value| {
                query.bind::<Text, _>(value.to_string())
            })
            .load::<Cell>(&mut self.sqlite)
            .unwrap_or_else(|error| panic!("SQLite refused {translated}: {error}"));
        let postgres: Vec<_> = postgres.into_iter().map(|cell| cell.v).collect();
        let sqlite: Vec<_> = sqlite.into_iter().map(|cell| cell.v).collect();
        assert_eq!(sqlite, postgres, "{probe}\ntranslated: {translated}");
        postgres
    }

    /// Asserts PostgreSQL rejects `statement` and the translator refuses it.
    fn assert_refused(&mut self, statement: &str) {
        let postgres = diesel::sql_query(statement).execute(&mut self.postgres);
        assert!(postgres.is_err(), "PostgreSQL ran {statement}");
        let translated = self.translated(statement);
        assert!(translated.is_err(), "{statement} translated to {translated:?}");
    }

    /// Runs the write `statement` on both engines and asserts both accept it
    /// or both refuse it, the translator's refusal counting as a refusal.
    fn assert_same_write(&mut self, statement: &str, accepted: bool) {
        let postgres = diesel::sql_query(statement).execute(&mut self.postgres);
        let sqlite = self.translated(statement).map(|translated| {
            let executed = diesel::sql_query(&translated).execute(&mut self.sqlite);
            (translated, executed)
        });
        let sqlite_accepted = matches!(&sqlite, Ok((_, Ok(_))));
        assert_eq!(
            (postgres.is_ok(), sqlite_accepted),
            (accepted, accepted),
            "{statement}\nPostgreSQL: {postgres:?}\nSQLite: {sqlite:?}"
        );
    }

    /// Asserts both engines store the same `(id, role)` rows.
    fn assert_same_grants(&mut self) {
        let postgres = schema::grants::table
            .select((schema::grants::id, schema::grants::role))
            .order(schema::grants::id)
            .load::<(i32, String)>(&mut self.postgres)
            .expect("PostgreSQL grants");
        let sqlite = schema::grants::table
            .select((schema::grants::id, schema::grants::role))
            .order(schema::grants::id)
            .load::<(i32, String)>(&mut self.sqlite)
            .expect("SQLite grants");
        assert_eq!(sqlite, postgres);
    }
}

fn labels(rows: &[&str]) -> Vec<Option<String>> {
    rows.iter().map(|row| Some((*row).to_string())).collect()
}

#[test]
fn comparisons_follow_declaration_order() {
    let mut replicas = Replicas::new(SOURCE);
    for (probe, expected) in [
        (
            "SELECT id::text AS v FROM grants WHERE role >= 'writer' ORDER BY id",
            &["1", "3", "5"][..],
        ),
        ("SELECT id::text AS v FROM grants WHERE role > 'reader' ORDER BY id", &["1", "3", "5"]),
        (
            "SELECT id::text AS v FROM grants WHERE role < 'admin' ORDER BY id",
            &["2", "3", "4", "5"],
        ),
        ("SELECT id::text AS v FROM grants WHERE role <= 'reader' ORDER BY id", &["2", "4"]),
        ("SELECT id::text AS v FROM grants WHERE 'writer' <= role ORDER BY id", &["1", "3", "5"]),
        (
            "SELECT id::text AS v FROM grants WHERE role >= 'writer'::role ORDER BY id",
            &["1", "3", "5"],
        ),
        ("SELECT id::text AS v FROM grants WHERE max_role >= role ORDER BY id", &["1", "2", "4"]),
        (
            "SELECT id::text AS v FROM grants WHERE role BETWEEN 'reader' AND 'writer' ORDER BY id",
            &["2", "3", "4", "5"],
        ),
        (
            "SELECT id::text AS v FROM grants WHERE role NOT BETWEEN 'writer' AND 'admin' ORDER BY id",
            &["2", "4"],
        ),
        (
            "SELECT id::text AS v FROM grants WHERE role IN ('reader', 'admin') ORDER BY id",
            &["1", "2", "4"],
        ),
        ("SELECT id::text AS v FROM grants WHERE role <> 'writer' ORDER BY id", &["1", "2", "4"]),
        (
            "SELECT id::text AS v FROM grants WHERE coalesce(max_role, 'reader') > role ORDER BY id",
            &["2", "4"],
        ),
    ] {
        assert_eq!(replicas.assert_same_rows(probe, &[]), labels(expected), "{probe}");
    }
}

#[test]
fn a_bound_label_compares_by_declaration_order() {
    let mut replicas = Replicas::new(SOURCE);
    let rows = replicas.assert_same_rows(
        "SELECT id::text AS v FROM grants WHERE role >= $1::role ORDER BY id",
        &["writer"],
    );
    assert_eq!(rows, labels(&["1", "3", "5"]));
}

/// Enum comparisons preserve nulls, labels and declaration order through
/// expression boundaries.
#[test]
fn nested_expressions_keep_nulls_labels_and_declaration_order() {
    let mut replicas = Replicas::new(SOURCE);
    for (probe, expected) in [
        ("SELECT id::text AS v FROM grants WHERE role < NULL ORDER BY id", &[][..]),
        ("SELECT id::text AS v FROM grants WHERE NULL::role < role ORDER BY id", &[][..]),
        (
            "SELECT role::role AS v FROM grants ORDER BY id",
            &["admin", "reader", "writer", "reader", "writer"],
        ),
        (
            "SELECT id::text AS v FROM grants WHERE role = ANY (ARRAY['reader'::role]) ORDER BY id",
            &["2", "4"],
        ),
        (
            "SELECT role AS \"v\" FROM grants UNION SELECT 'writer'::role ORDER BY \"v\"",
            &["reader", "writer", "admin"],
        ),
        (
            "(SELECT role AS v FROM grants) ORDER BY v",
            &["reader", "reader", "writer", "writer", "admin"],
        ),
        (
            "SELECT id::text AS v FROM grants WHERE role < (SELECT 'admin'::role) ORDER BY id",
            &["2", "3", "4", "5"],
        ),
        (
            "SELECT DISTINCT ON (role) role AS v FROM grants ORDER BY role, id",
            &["reader", "writer", "admin"],
        ),
        (
            "SELECT v::text AS v FROM (SELECT DISTINCT ON (resource_id) resource_id AS v FROM grants ORDER BY resource_id, id) AS distinct_resources",
            &["1", "2"],
        ),
        ("SELECT max(role) AS v FROM grants", &["admin"]),
        (
            "SELECT greatest(role, max_role) AS v FROM grants ORDER BY id",
            &["admin", "writer", "writer", "admin", "writer"],
        ),
        (
            "SELECT v FROM (SELECT CASE WHEN id = 1 THEN role ELSE max_role END AS v FROM grants WHERE max_role IS NOT NULL UNION ALL SELECT role FROM grants ORDER BY 1) AS results",
            &[
                "reader", "reader", "reader", "writer", "writer", "writer", "admin", "admin",
                "admin",
            ],
        ),
        (
            "SELECT kind AS v FROM (SELECT * FROM resources UNION ALL SELECT * FROM resources ORDER BY 2) AS results",
            &["organization", "organization", "folder", "folder", "project", "project"],
        ),
        (
            "SELECT v FROM (SELECT resources.*, kind AS v FROM resources UNION ALL SELECT resources.*, kind FROM resources ORDER BY 3) AS results",
            &["organization", "organization", "folder", "folder", "project", "project"],
        ),
        (
            "SELECT v FROM (SELECT 'reader' UNION ALL SELECT role FROM grants ORDER BY 1) AS results(v)",
            &["reader", "reader", "reader", "writer", "writer", "admin"],
        ),
        (
            "SELECT role AS V FROM grants UNION SELECT 'writer'::role ORDER BY v",
            &["reader", "writer", "admin"],
        ),
    ] {
        assert_eq!(replicas.assert_same_rows(probe, &[]), labels(expected), "{probe}");
    }
}

/// A compound select whose first output is a `CASE` sorts it in declaration
/// order under the name PostgreSQL gives it.
#[test]
fn a_compound_select_sorts_a_case_output_by_declaration_order() {
    let mut replicas = Replicas::new(SOURCE);
    replicas.assert_same_rows(
        "SELECT v FROM (SELECT CASE WHEN id = 1 THEN role ELSE max_role END FROM grants \
         UNION ALL SELECT role FROM grants ORDER BY 1) AS results(v)",
        &[],
    );
}

/// `DISTINCT ON` an enum the query does not select keeps one row per label in
/// declaration order.
#[test]
fn distinct_on_an_unselected_enum_follows_declaration_order() {
    let mut replicas = Replicas::new(SOURCE);
    let rows = replicas.assert_same_rows(
        "SELECT DISTINCT ON (role) id::text AS v FROM grants ORDER BY role, id",
        &[],
    );
    assert_eq!(rows, labels(&["2", "3", "1"]));
}

/// Untyped parameters inherit the enum type and compare in declaration order.
#[test]
fn untyped_parameters_compare_by_declaration_order() {
    let mut replicas = Replicas::new(SOURCE);
    let probe = "SELECT id::text AS v FROM grants WHERE role >= $1 ORDER BY id";
    // Preparing SQL leaves parameter inference to PostgreSQL.
    diesel::sql_query(format!("PREPARE enum_probe AS {probe}"))
        .execute(&mut replicas.postgres)
        .expect("prepare");
    let postgres = diesel::sql_query("EXECUTE enum_probe('writer')")
        .load::<Cell>(&mut replicas.postgres)
        .expect("execute");
    let translated = replicas.translated(probe).expect("translate");
    let sqlite = diesel::sql_query(translated)
        .bind::<Text, _>("writer")
        .load::<Cell>(&mut replicas.sqlite)
        .expect("execute");
    let postgres: Vec<_> = postgres.into_iter().map(|cell| cell.v).collect();
    let sqlite: Vec<_> = sqlite.into_iter().map(|cell| cell.v).collect();
    assert_eq!(postgres, labels(&["1", "3", "5"]));
    assert_eq!(sqlite, postgres);
}

/// Dropping an unused enum leaves unrelated stored rows intact.
#[test]
fn dropping_an_unused_type_keeps_the_rows() {
    let mut replicas = Replicas::new(
        "CREATE TYPE unused AS ENUM ('reader', 'admin');
         CREATE TABLE retained (id INTEGER PRIMARY KEY);
         INSERT INTO retained VALUES (7);
         DROP TYPE unused CASCADE;",
    );
    assert_eq!(
        replicas.assert_same_rows("SELECT id::text AS v FROM retained", &[]),
        labels(&["7"])
    );
}

#[test]
fn sorts_and_extremes_follow_declaration_order() {
    let mut replicas = Replicas::new(SOURCE);
    for (probe, expected) in [
        (
            "SELECT role AS v FROM grants ORDER BY role, id",
            &["reader", "reader", "writer", "writer", "admin"][..],
        ),
        (
            "SELECT role AS v FROM grants ORDER BY role DESC, id",
            &["admin", "writer", "writer", "reader", "reader"],
        ),
        (
            "SELECT role AS v FROM grants ORDER BY 1, id",
            &["reader", "reader", "writer", "writer", "admin"],
        ),
        (
            "SELECT r AS v FROM (SELECT role AS r, id FROM grants) AS s ORDER BY r DESC, id",
            &["admin", "writer", "writer", "reader", "reader"],
        ),
        (
            "SELECT v FROM (SELECT role AS v FROM grants WHERE id < 3 \
             UNION SELECT role FROM grants WHERE id >= 3) AS labels ORDER BY v",
            &["reader", "writer", "admin"],
        ),
        (
            "SELECT role AS v FROM grants WHERE id < 3 UNION SELECT role FROM grants WHERE id >= 3 \
             ORDER BY v DESC",
            &["admin", "writer", "reader"],
        ),
        (
            "SELECT role AS v FROM grants WHERE id < 3 UNION SELECT role FROM grants ORDER BY 1 \
             LIMIT 2",
            &["reader", "writer"],
        ),
        (
            "SELECT id::text AS v, role FROM grants WHERE id < 3 \
             UNION SELECT id::text, role FROM grants WHERE id > 3 ORDER BY v",
            &["1", "2", "4", "5"],
        ),
        (
            "SELECT DISTINCT ON (role) role AS v FROM grants ORDER BY role DESC, id",
            &["admin", "writer", "reader"],
        ),
        ("SELECT min(role) AS v FROM grants", &["reader"]),
        ("SELECT max(role) AS v FROM grants", &["admin"]),
        ("SELECT max(max_role) AS v FROM grants WHERE id > 1", &["admin"]),
        ("SELECT min(max_role) AS v FROM grants", &["reader"]),
        (
            "SELECT max(role) AS v FROM grants GROUP BY resource_id ORDER BY resource_id",
            &["admin", "writer"],
        ),
        (
            "SELECT max(role) OVER (PARTITION BY resource_id) AS v FROM grants ORDER BY id",
            &["admin", "admin", "writer", "writer", "admin"],
        ),
        (
            "SELECT (row_number() OVER (ORDER BY role, id))::text AS v FROM grants ORDER BY id",
            &["5", "1", "3", "2", "4"],
        ),
        (
            "SELECT string_agg(role::text, ',' ORDER BY role, id) AS v FROM grants",
            &["reader,reader,writer,writer,admin"],
        ),
    ] {
        assert_eq!(replicas.assert_same_rows(probe, &[]), labels(expected), "{probe}");
    }
    let greatest = replicas
        .assert_same_rows("SELECT greatest(role, max_role) AS v FROM grants ORDER BY id", &[]);
    assert_eq!(greatest, labels(&["admin", "writer", "writer", "admin", "writer"]));
    let least =
        replicas.assert_same_rows("SELECT least(role, max_role) AS v FROM grants ORDER BY id", &[]);
    assert_eq!(least, labels(&["admin", "reader", "writer", "reader", "reader"]));
    // The count keeps the grand-total branch to one row, which the empty
    // grouping set without an aggregate does not.
    let rolled_up = replicas.assert_same_rows(
        "SELECT role AS v, count(*) AS n FROM grants GROUP BY ROLLUP (role) ORDER BY v",
        &[],
    );
    assert_eq!(rolled_up, [labels(&["reader", "writer", "admin"]), vec![None]].concat());
}

#[test]
fn casts_ranges_and_generated_columns_read_labels() {
    let mut replicas = Replicas::new(SOURCE);
    for (probe, expected) in [
        ("SELECT 'writer'::role AS v", &["writer"][..]),
        ("SELECT CAST(NULL AS role) AS v", &[][..]),
        (
            "SELECT CAST(column1 AS role) AS v FROM (VALUES ('admin'), ('reader')) AS given \
             ORDER BY 1",
            &["reader", "admin"],
        ),
        (
            "SELECT x AS v FROM unnest(enum_range(NULL::role)) AS allowed(x)",
            &["reader", "writer", "admin"],
        ),
        (
            "SELECT x AS v FROM unnest(enum_range(NULL::role)) AS allowed(x) ORDER BY x DESC",
            &["admin", "writer", "reader"],
        ),
        (
            "SELECT column1 AS v FROM (VALUES ('writer'::role), ('reader'::role)) AS given \
             ORDER BY column1",
            &["reader", "writer"],
        ),
        (
            "SELECT x AS v FROM unnest(enum_range(NULL::role)) AS allowed(x) WHERE x <> 'admin' \
             UNION SELECT column1 FROM (VALUES ('admin'::role)) AS given ORDER BY v DESC",
            &["admin", "writer", "reader"],
        ),
        (
            "SELECT CASE WHEN shareable THEN 'yes' ELSE 'no' END AS v FROM grants ORDER BY id",
            &["no", "yes", "yes", "yes", "yes"],
        ),
    ] {
        let rows = replicas.assert_same_rows(probe, &[]);
        if expected.is_empty() {
            assert_eq!(rows, vec![None], "{probe}");
        } else {
            assert_eq!(rows, labels(expected), "{probe}");
        }
    }
}

/// A cast of a value that is no label raises on PostgreSQL and reads NULL on
/// SQLite, as every other failing cast does there.
#[test]
fn a_cast_of_an_undeclared_value_reads_null() {
    let mut replicas = Replicas::new(SOURCE);
    let probe = "SELECT CAST(column1 AS role) AS v FROM (VALUES ('owner')) AS given";
    assert!(diesel::sql_query(probe).load::<Cell>(&mut replicas.postgres).is_err());
    let translated = replicas.translated(probe).expect("translate");
    let sqlite = diesel::sql_query(&translated)
        .load::<Cell>(&mut replicas.sqlite)
        .unwrap_or_else(|error| panic!("SQLite refused {translated}: {error}"));
    assert_eq!(sqlite.into_iter().map(|cell| cell.v).collect::<Vec<_>>(), vec![None]);
}

/// Invalid enum comparisons and labels are refused before execution.
#[test]
fn statements_postgresql_rejects_are_refused() {
    let mut replicas = Replicas::new(SOURCE);
    for statement in [
        "SELECT id FROM grants WHERE role = 'owner'",
        "SELECT id FROM grants WHERE role >= 'owner'",
        "SELECT id FROM grants WHERE role IN ('reader', 'owner')",
        "SELECT 'owner'::role",
        "SELECT id FROM grants WHERE role = resource_kind",
        "SELECT id FROM grants WHERE role < resource_kind",
    ] {
        replicas.assert_refused(statement);
    }
    for (statement, problem) in [
        ("SELECT enum_range(1)", "enum_range"),
        ("SELECT role < missing_enum_value() FROM grants", "missing_enum_value"),
        (
            "SELECT id FROM grants WHERE role < 'folder'::kind",
            "operator does not exist: role < kind",
        ),
        ("VALUES ('owner'::role) ORDER BY 1", "invalid input value for enum role"),
        (
            "SELECT role FROM too.many.schema.parts UNION ALL SELECT role FROM grants ORDER BY 1",
            "too.many.schema.parts",
        ),
    ] {
        replicas.assert_refused(statement);
        let error = replicas.translated(statement).expect_err(problem);
        assert!(error.to_string().contains(problem), "{statement}\n{error}");
    }
}

/// A nameless function supplied through the statement API is refused.
#[test]
fn a_nameless_function_call_is_refused() {
    let replicas = Replicas::new(SOURCE);
    let mut statements = sqlparser::parser::Parser::parse_sql(
        &sqlparser::dialect::PostgreSqlDialect {},
        "SELECT abs(1)",
    )
    .expect("parse");
    let sqlparser::ast::Statement::Query(query) = &mut statements[0] else { panic!("query") };
    let sqlparser::ast::SetExpr::Select(select) = query.body.as_mut() else { panic!("select") };
    let sqlparser::ast::SelectItem::UnnamedExpr(sqlparser::ast::Expr::Function(function)) =
        &mut select.projection[0]
    else {
        panic!("function")
    };
    function.name.0.clear();
    let error = Pg2Sqlite::default()
        .statement(statements.remove(0))
        .translate_with_schema(&replicas.schema, &options())
        .expect_err("nameless function");
    assert!(error.to_string().contains("function"), "{error}");
}

/// Two types declaring the same labels are still two types.
#[test]
fn types_with_the_same_labels_do_not_compare() {
    let mut replicas = Replicas::new(
        "CREATE TYPE first AS ENUM ('x', 'y');
         CREATE TYPE second AS ENUM ('x', 'y');
         CREATE TABLE pairs (id INTEGER PRIMARY KEY, p first, q second);",
    );
    for statement in ["SELECT id FROM pairs WHERE p < q", "SELECT CAST(p AS second) FROM pairs"] {
        replicas.assert_refused(statement);
    }
}

/// Orderings whose sort SQLite would run on the stored text are refused
/// rather than answered in text order.
#[test]
fn orderings_sqlite_cannot_key_are_refused() {
    let replicas = Replicas::new(SOURCE);
    for (statement, problem) in [
        ("SELECT id FROM grants WHERE role > ANY (ARRAY['reader'::role])", "ANY"),
        ("SELECT id FROM grants WHERE (role, id) > ('reader', 1)", "row values"),
        ("SELECT enum_range('reader'::role, 'admin'::role)", "two bounds"),
        (
            "SELECT (SELECT * FROM (SELECT role FROM grants LIMIT 1) AS chosen) UNION ALL SELECT role FROM grants ORDER BY 1",
            "unnamed",
        ),
        (
            "SELECT role, count(*) FROM grants GROUP BY ROLLUP(role) ORDER BY role, count(*) + 1",
            "expression",
        ),
        (
            "SELECT * FROM unknown_relation UNION ALL SELECT * FROM unknown_relation ORDER BY 1",
            "columns a * hides",
        ),
        (
            "SELECT *, CASE WHEN id = 1 THEN role ELSE max_role END FROM grants UNION ALL SELECT *, role FROM grants ORDER BY 4",
            "Alias the column or list the columns",
        ),
    ] {
        let error = replicas.translated(statement).expect_err(statement);
        assert!(error.to_string().contains(problem), "{statement}\n{error}");
    }
    let error = replicas.translated("SELECT CURRENT_DATE").expect_err("deployment required");
    assert!(error.to_string().contains("with_deployment"), "{error}");
    let error = replicas
        .translated("SELECT role < 'admin'::unknown FROM grants")
        .expect_err("unsupported pseudo-type")
        .to_string();
    assert!(error.contains("unknown has no SQLite equivalent"), "{error}");
    assert!(!error.contains("operator does not exist"), "{error}");
}

#[test]
fn writes_admit_only_what_postgresql_admits() {
    let mut replicas = Replicas::new(SOURCE);
    for (statement, accepted) in [
        // No such label.
        (
            "INSERT INTO grants (id, resource_id, resource_kind, role) VALUES (6, 1, 'folder', 'owner')",
            false,
        ),
        // The table CHECK keeps an admin off a project.
        (
            "INSERT INTO grants (id, resource_id, resource_kind, role) VALUES (6, 2, 'project', 'admin')",
            false,
        ),
        // The partial unique index holds one admin per resource.
        (
            "INSERT INTO grants (id, resource_id, resource_kind, role) VALUES (6, 1, 'folder', 'admin')",
            false,
        ),
        // The composite key reaches no resource of that kind.
        (
            "INSERT INTO grants (id, resource_id, resource_kind, role) VALUES (6, 3, 'folder', 'reader')",
            false,
        ),
        // The column CHECK admits only two kinds.
        (
            "INSERT INTO grants (id, resource_id, resource_kind, role) VALUES (6, 3, 'organization', 'reader')",
            false,
        ),
        (
            "INSERT INTO grants (id, resource_id, resource_kind, role) VALUES (6, 2, 'project', 'writer')",
            true,
        ),
        ("UPDATE grants SET role = 'owner' WHERE id = 6", false),
        ("UPDATE grants SET role = 'admin' WHERE id = 2", false),
        ("UPDATE grants SET role = 'reader' WHERE role >= 'writer' AND resource_id = 2", true),
    ] {
        replicas.assert_same_write(statement, accepted);
        replicas.assert_same_grants();
    }
}

/// `ADD VALUE` places a label before or after another, and every column of
/// the type admits it and orders by the final declaration.
#[test]
fn added_values_take_their_declared_place() {
    let mut replicas = Replicas::new(
        "CREATE TYPE level AS ENUM ('low', 'high');
         CREATE TABLE readings (id INTEGER PRIMARY KEY, level level NOT NULL);
         ALTER TYPE level ADD VALUE 'medium' BEFORE 'high';
         ALTER TYPE level ADD VALUE 'extreme' AFTER 'high';",
    );
    replicas.assert_same_write(
        "INSERT INTO readings (id, level) VALUES (1, 'extreme'), (2, 'low'), (3, 'high'), (4, 'medium')",
        true,
    );
    let sorted = replicas.assert_same_rows("SELECT level AS v FROM readings ORDER BY level", &[]);
    assert_eq!(sorted, labels(&["low", "medium", "high", "extreme"]));
    let above = replicas.assert_same_rows(
        "SELECT level AS v FROM readings WHERE level > 'medium' ORDER BY level",
        &[],
    );
    assert_eq!(above, labels(&["high", "extreme"]));
}

/// A type statement the columns already translated cannot follow is refused,
/// and one they can follow translates to nothing.
#[test]
fn type_statements_follow_the_columns_already_translated() {
    let late = Pg2Sqlite::default()
        .sql(
            "CREATE TYPE level AS ENUM ('low', 'high');
             CREATE TABLE readings (id INTEGER PRIMARY KEY, level level NOT NULL);
             CREATE TABLE scratch (id INTEGER);
             DROP TABLE scratch;
             ALTER TYPE level ADD VALUE 'medium' BEFORE 'high';",
        )
        .expect("parse")
        .translate(&options())
        .expect_err("readings checks the labels level had before the drop");
    assert!(late.to_string().contains("ADD VALUE"), "{late}");
    let replicas = Replicas::new(SOURCE);
    let resumed = replicas.translated("ALTER TYPE role ADD VALUE 'owner'");
    assert!(resumed.is_err(), "grants checks the starting labels: {resumed:?}");
    let cascade = Pg2Sqlite::default()
        .sql(
            "CREATE TYPE level AS ENUM ('low', 'high');
             CREATE TABLE readings (id INTEGER PRIMARY KEY, level level NOT NULL);
             DROP TYPE level CASCADE;",
        )
        .expect("parse")
        .translate(&options())
        .expect_err("PostgreSQL drops readings.level");
    assert!(cascade.to_string().contains("readings.level"), "{cascade}");
}

/// A renamed type keeps the columns declared under its old name and types
/// those declared under the new one.
#[test]
fn a_renamed_type_keeps_its_columns() {
    let mut replicas = Replicas::new(
        "CREATE TYPE mood AS ENUM ('sad', 'ok');
         CREATE TABLE before_rename (id INTEGER PRIMARY KEY, m mood NOT NULL);
         ALTER TYPE mood RENAME TO feeling;
         CREATE TABLE after_rename (id INTEGER PRIMARY KEY, f feeling NOT NULL);",
    );
    for (statement, accepted) in [
        ("INSERT INTO before_rename (id, m) VALUES (1, 'ok'), (2, 'sad')", true),
        ("INSERT INTO after_rename (id, f) VALUES (1, 'sad')", true),
        ("INSERT INTO before_rename (id, m) VALUES (3, 'meh')", false),
    ] {
        replicas.assert_same_write(statement, accepted);
    }
    let sorted =
        replicas.assert_same_rows("SELECT m::text AS v FROM before_rename ORDER BY m", &[]);
    assert_eq!(sorted, labels(&["sad", "ok"]));
}

/// A trigger casts `TG_OP` to an enum, as an audit log does.
#[test]
fn a_trigger_records_its_operation_as_a_label() {
    let mut replicas = Replicas::new(
        "CREATE TYPE operation AS ENUM ('INSERT', 'UPDATE', 'DELETE');
         CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);
         CREATE TABLE audit (seq SERIAL PRIMARY KEY, operation operation NOT NULL, note_id INTEGER);
         CREATE FUNCTION record_new_operation() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO audit (operation, note_id) VALUES (TG_OP::operation, NEW.id);
           RETURN NULL;
         END $$;
         CREATE FUNCTION record_old_operation() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO audit (operation, note_id) VALUES (TG_OP::operation, OLD.id);
           RETURN NULL;
         END $$;
         CREATE TRIGGER notes_insert_audit AFTER INSERT ON notes
           FOR EACH ROW EXECUTE FUNCTION record_new_operation();
         CREATE TRIGGER notes_update_audit AFTER UPDATE ON notes
           FOR EACH ROW EXECUTE FUNCTION record_new_operation();
         CREATE TRIGGER notes_delete_audit AFTER DELETE ON notes
           FOR EACH ROW EXECUTE FUNCTION record_old_operation();",
    );
    for statement in [
        "INSERT INTO notes (id, body) VALUES (1, 'a')",
        "UPDATE notes SET body = 'b' WHERE id = 1",
        "DELETE FROM notes WHERE id = 1",
    ] {
        replicas.assert_same_write(statement, true);
    }
    let operations = replicas
        .assert_same_rows("SELECT operation || ':' || note_id AS v FROM audit ORDER BY seq", &[]);
    assert_eq!(operations, labels(&["INSERT:1", "UPDATE:1", "DELETE:1"]));
}
