//! Parity for the names PostgreSQL gives an index written without one.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::{helpers, postgres_harness};

const SOURCE: &str = r#"
CREATE TABLE t (s TEXT, "Mixed Col" TEXT, n INTEGER);
CREATE TABLE t_n_idx (x INTEGER);
CREATE INDEX ON t (s);
CREATE UNIQUE INDEX ON t (s);
CREATE INDEX ON t ((s::varchar));
CREATE INDEX ON t ((CAST(s AS text)));
CREATE INDEX ON t ((s COLLATE "C"));
CREATE INDEX ON t (s) WHERE n > 0;
CREATE INDEX ON t (lower(s));
CREATE INDEX ON t (coalesce(s, 'a'));
CREATE INDEX ON t (nullif(s, ''));
CREATE INDEX ON t (greatest(n, 0));
CREATE INDEX ON t ((CASE WHEN n > 0 THEN 1 END));
CREATE INDEX ON t ((s || 'x'));
CREATE INDEX ON t ((-n));
CREATE INDEX ON t ((n + 1), n);
CREATE INDEX ON t (s, s);
CREATE INDEX ON t (lower(s), lower(s));
CREATE INDEX ON t ("Mixed Col");
CREATE INDEX ON t (n);
CREATE UNIQUE INDEX ON t (n) NULLS NOT DISTINCT;
CREATE TABLE "Tab" (a INTEGER);
CREATE INDEX ON "Tab" (a);
CREATE TABLE secured (s TEXT);
ALTER TABLE secured ENABLE ROW LEVEL SECURITY;
CREATE INDEX ON secured (s);
CREATE TABLE aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa (
    bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb INTEGER,
    c INTEGER
);
CREATE INDEX ON aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa (
    bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, c
);
CREATE INDEX ON aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa (
    bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, c
);
CREATE TABLE u (s TEXT);
CREATE INDEX ON u (s);
CREATE INDEX ON u (s);
DROP INDEX u_s_idx;
CREATE INDEX ON u (s);
"#;

/// A later migration that reaches an unnamed index by the name PostgreSQL gave
/// it, and frees that name for the next one.
const MIGRATION: &str = "DROP INDEX t_s_idx; CREATE INDEX ON t (s);";

/// The SQLite-only companion of the unnamed `NULLS NOT DISTINCT` index.
const COMPANION: &str = "t_n_idx2_nnd";

mod catalogue {
    diesel::table! {
        pg_indexes (indexname) {
            schemaname -> Text,
            indexname -> Text,
        }
    }

    diesel::table! {
        sqlite_master (name) {
            #[sql_name = "type"]
            kind -> Text,
            name -> Text,
        }
    }
}

fn postgres_names(connection: &mut PgConnection) -> Vec<String> {
    use catalogue::pg_indexes;
    pg_indexes::table
        .filter(pg_indexes::schemaname.eq("public"))
        .select(pg_indexes::indexname)
        .order(pg_indexes::indexname)
        .load(connection)
        .expect("read pg_indexes")
}

/// The replica's index names, leaving out the companion PostgreSQL has no
/// counterpart for.
fn sqlite_names(connection: &mut SqliteConnection) -> Vec<String> {
    use catalogue::sqlite_master;
    sqlite_master::table
        .filter(sqlite_master::kind.eq("index"))
        .filter(sqlite_master::name.not_like("sqlite_autoindex%"))
        .filter(sqlite_master::name.ne(COMPANION))
        .select(sqlite_master::name)
        .order(sqlite_master::name)
        .load(connection)
        .expect("read sqlite_master")
}

fn execute_ddl(connection: &mut SqliteConnection, statements: &[sqlparser::ast::Statement]) {
    for statement in statements {
        // Translated DDL, which the query DSL cannot express.
        diesel::sql_query(statement.to_string())
            .execute(connection)
            .unwrap_or_else(|error| panic!("SQLite refused {statement}: {error}"));
    }
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit")
}

fn has_companion(connection: &mut SqliteConnection) -> bool {
    use catalogue::sqlite_master;
    diesel::select(diesel::dsl::exists(
        sqlite_master::table.filter(sqlite_master::name.eq(COMPANION)),
    ))
    .get_result(connection)
    .expect("read sqlite_master")
}

#[test]
fn unnamed_indexes_take_postgresql_names() {
    let mut postgres = postgres_harness::fresh_database();
    postgres_harness::apply(&mut postgres, SOURCE)
        .unwrap_or_else(|error| panic!("PostgreSQL refused the source: {error}"));

    let translated = Pg2Sqlite::default()
        .sql(SOURCE)
        .unwrap()
        .translate(&options())
        .expect("translate the source")
        .into_vec();
    let mut sqlite = establish_connection();
    execute_ddl(&mut sqlite, &translated);

    assert_eq!(sqlite_names(&mut sqlite), postgres_names(&mut postgres));
    assert!(has_companion(&mut sqlite), "missing {COMPANION}");

    postgres_harness::apply(&mut postgres, MIGRATION)
        .unwrap_or_else(|error| panic!("PostgreSQL refused the migration: {error}"));
    let schema = Pg2Sqlite::default().sql(SOURCE).unwrap().build_schema().unwrap();
    let migration = Pg2Sqlite::default()
        .sql(MIGRATION)
        .unwrap()
        .translate_with_schema(&schema, &options())
        .expect("translate the migration")
        .into_vec();
    execute_ddl(&mut sqlite, &migration);

    assert_eq!(sqlite_names(&mut sqlite), postgres_names(&mut postgres));
}
