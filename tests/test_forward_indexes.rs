//! Tests for forward index translation covering FTS and other index types
//! in `src/impls/translator_impls/create_index.rs`.

mod helpers;
#[path = "helpers/run_translated.rs"]
mod run_translated_helper;
#[path = "helpers/translate.rs"]
mod translate_helpers;
use helpers::translate_default_statements as translate_result;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;
use translate_helpers::translate_default as translate;

/// Executes all statements emitted by translating `sql` against real in-memory
/// SQLite. Used after substring asserts to prove the translated output runs
/// without error.
fn execute_translated(sql: &str) {
    helpers::execute_all(sql, &Pg2SqliteOptions::default());
}

#[test]
fn basic_btree_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_users_name ON users (name);
    ";
    let output = translate(sql);
    assert!(output.contains("CREATE INDEX"), "Expected CREATE INDEX: {output}");
    assert!(output.contains("idx_users_name"), "Expected index name: {output}");
    execute_translated(sql);
}

#[test]
fn unique_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, email TEXT);
        CREATE UNIQUE INDEX idx_unique_email ON users (email);
    ";
    let output = translate(sql);
    assert!(output.contains("UNIQUE"), "Expected UNIQUE index: {output}");
    execute_translated(sql);
}

mod catalogue {
    diesel::table! {
        sqlite_master (name) {
            name -> Text,
            tbl_name -> Text,
        }
    }
}

/// An index on a secured table indexes its backing table, since SQLite cannot
/// index the view.
#[test]
fn index_on_a_secured_table_lands_on_its_backing_table() {
    use catalogue::sqlite_master;
    use diesel::{connection::SimpleConnection, prelude::*};

    let sql = "
        CREATE TABLE notes (id INT PRIMARY KEY, title TEXT);
        ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
        CREATE INDEX notes_title ON notes (title);
    ";
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let mut connection = SqliteConnection::establish(":memory:").expect("in-memory SQLite");
    for statement in helpers::translate_pg(sql, &options).expect("translate") {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    let indexed: String = sqlite_master::table
        .filter(sqlite_master::name.eq("notes_title"))
        .select(sqlite_master::tbl_name)
        .first(&mut connection)
        .expect("the index exists");
    assert_eq!(indexed, "notes_rls");
}

#[test]
fn index_if_not_exists() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX IF NOT EXISTS idx_name ON users (name);
    ";
    let output = translate(sql);
    assert!(output.contains("IF NOT EXISTS"), "Expected IF NOT EXISTS: {output}");
    execute_translated(sql);
}

#[test]
fn multi_column_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_name_age ON users (name, age);
    ";
    let output = translate(sql);
    assert!(output.contains("name"), "Expected name column: {output}");
    assert!(output.contains("age"), "Expected age column: {output}");
    execute_translated(sql);
}

#[test]
fn gin_tsvector_index_to_fts5() {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_articles_fts ON articles USING GIN (to_tsvector('english', title || ' ' || body));
    ";
    let output = translate(sql);
    // Should be translated to FTS5 virtual table or similar
    assert!(
        output.contains("fts5") || output.contains("FTS") || output.contains("articles"),
        "Expected FTS5 or table reference: {output}"
    );
    execute_translated(sql);
}

#[test]
fn gin_tsvector_single_column() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, content TEXT);
        CREATE INDEX idx_docs_fts ON docs USING GIN (to_tsvector('english', content));
    ";
    let output = translate(sql);
    assert!(
        output.contains("fts5") || output.contains("content") || output.contains("docs"),
        "Expected FTS5 or column reference: {output}"
    );
    execute_translated(sql);
}

#[test]
fn gist_tsvector_index() {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, title TEXT);
        CREATE INDEX idx_gist ON articles USING GiST (to_tsvector('english', title));
    ";
    let output = translate(sql);
    // GiST with tsvector should also translate to FTS5
    assert!(
        output.contains("fts5") || output.contains("articles"),
        "Expected FTS5 translation: {output}"
    );
    execute_translated(sql);
}

#[test]
fn hash_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_hash ON users USING HASH (name);
    ";
    let output = translate(sql);
    // Hash indexes can't be directly translated - should become regular index
    // or be dropped
    assert!(output.contains("users"), "Expected table still present: {output}");
    execute_translated(sql);
}

#[test]
fn expression_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_lower_name ON users ((lower(name)));
    ";
    let output = translate(sql);
    assert!(output.contains("users"), "Expected output: {output}");
    execute_translated(sql);
}

#[test]
fn partial_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, active BOOLEAN);
        CREATE INDEX idx_active ON users (name) WHERE active = true;
    ";
    let output = translate(sql);
    assert!(
        output.contains("WHERE") || output.contains("users"),
        "Expected index or table: {output}"
    );
    execute_translated(sql);
}

#[test]
fn concurrently_is_dropped() {
    // CONCURRENTLY is not valid in SQLite
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX CONCURRENTLY idx_name ON users (name);
    ";
    let output = translate(sql);
    assert!(!output.contains("CONCURRENTLY"), "CONCURRENTLY must be stripped: {output}");
    assert!(output.contains("idx_name"), "Index name should be preserved: {output}");
    execute_translated(sql);
}

#[test]
fn include_clause_is_dropped() {
    // INCLUDE (covering index) is PostgreSQL-only
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_name ON users (name) INCLUDE (age);
    ";
    let output = translate(sql);
    assert!(!output.contains("INCLUDE"), "INCLUDE clause must be stripped: {output}");
    assert!(output.contains("idx_name"), "Index name should be preserved: {output}");
    execute_translated(sql);
}

#[test]
fn using_btree_is_dropped() {
    // USING BTREE is the default and is not emitted in SQLite syntax
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_name ON users USING BTREE (name);
    ";
    let output = translate(sql);
    assert!(!output.contains("USING"), "USING clause must be stripped: {output}");
    assert!(output.contains("idx_name"), "Index name should be preserved: {output}");
    execute_translated(sql);
}

#[test]
fn gin_index_non_tsvector_causes_error() {
    let sql = "
        CREATE TABLE documents (id SERIAL PRIMARY KEY, content TEXT);
        CREATE INDEX idx_content ON documents USING GIN (content);
    ";
    let result = translate_result(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("to_tsvector"), "Error should mention to_tsvector: {}", err);
}

#[test]
fn gist_index_non_tsvector_causes_error() {
    let sql = "
        CREATE TABLE locations (id SERIAL PRIMARY KEY, point TEXT);
        CREATE INDEX idx_point ON locations USING GiST (point);
    ";
    let result = translate_result(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("GiST"), "Error should mention GiST index: {}", err);
}

#[test]
fn gist_tsvector_translates_to_fts5() {
    let sql = "
        CREATE TABLE articles (id SERIAL PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_search ON articles USING GiST (to_tsvector('english', title || ' ' || body));
    ";
    let translated_sql = translate_result(sql).unwrap();

    // one pragma + table + FTS5 virtual table + 3 triggers + 1 backfill INSERT
    // = 7 statements
    assert_eq!(translated_sql.len(), 7, "one pragma plus the 6 FTS5 statements");

    // Second statement (index 1) is the table.
    assert!(translated_sql[1].contains("CREATE TABLE articles"));

    // Third statement (index 2) should be CREATE VIRTUAL TABLE ... USING fts5
    assert!(
        translated_sql[2].contains("CREATE VIRTUAL TABLE"),
        "Expected FTS5 virtual table, got: {}",
        translated_sql[2]
    );
    assert!(translated_sql[2].contains("fts5"), "Expected fts5 module, got: {}", translated_sql[2]);
    assert!(
        translated_sql[2].contains("articles_fts"),
        "Expected articles_fts table name, got: {}",
        translated_sql[2]
    );
    execute_translated(sql);
}

#[test]
fn gin_tsvector_translates_to_fts5() {
    let sql = "
        CREATE TABLE documents (id SERIAL PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_search ON documents USING GIN (to_tsvector('english', title || ' ' || body));
    ";
    let translated_sql = translate_result(sql).unwrap();

    // one pragma + table + FTS5 virtual table + 3 triggers + 1 backfill INSERT
    // = 7 statements
    assert_eq!(translated_sql.len(), 7, "one pragma plus the 6 FTS5 statements");

    // Second statement (index 1) is the table.
    assert!(translated_sql[1].contains("CREATE TABLE documents"));

    // Third statement (index 2) should be CREATE VIRTUAL TABLE ... USING fts5
    assert!(
        translated_sql[2].contains("CREATE VIRTUAL TABLE"),
        "Expected FTS5 virtual table, got: {}",
        translated_sql[2]
    );
    assert!(translated_sql[2].contains("fts5"), "Expected fts5 module, got: {}", translated_sql[2]);
    assert!(
        translated_sql[2].contains("documents_fts"),
        "Expected documents_fts table name, got: {}",
        translated_sql[2]
    );
    assert!(
        translated_sql[2].contains("title"),
        "Expected title column, got: {}",
        translated_sql[2]
    );
    assert!(translated_sql[2].contains("body"), "Expected body column, got: {}", translated_sql[2]);

    // Statements 4-6 (indices 3-5) should be triggers
    assert!(
        translated_sql[3].contains("CREATE TRIGGER") && translated_sql[3].contains("AFTER INSERT"),
        "Expected INSERT trigger, got: {}",
        translated_sql[3]
    );
    assert!(
        translated_sql[4].contains("CREATE TRIGGER") && translated_sql[4].contains("AFTER DELETE"),
        "Expected DELETE trigger, got: {}",
        translated_sql[4]
    );
    assert!(
        translated_sql[5].contains("CREATE TRIGGER") && translated_sql[5].contains("AFTER UPDATE"),
        "Expected UPDATE trigger, got: {}",
        translated_sql[5]
    );
}

mod keyed {
    diesel::table! {
        t (id) {
            id -> Integer,
            s -> Nullable<Text>,
            live -> Integer,
        }
    }

    diesel::table! {
        notes (id) {
            id -> Integer,
            title -> Nullable<Text>,
        }
    }

    diesel::table! {
        tags (id) {
            id -> Integer,
            label -> Nullable<Text>,
        }
    }

    diesel::table! {
        pairs (id) {
            id -> Integer,
            a -> Nullable<Integer>,
            s -> Nullable<Text>,
        }
    }
}

const KEYED_TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT, live INT NOT NULL);";

/// Inserts one row into `t` of the translated schema.
fn insert_keyed(
    connection: &mut diesel::SqliteConnection,
    id: i32,
    s: Option<&str>,
    live: i32,
) -> diesel::QueryResult<usize> {
    use diesel::prelude::*;
    diesel::insert_into(keyed::t::table)
        .values((keyed::t::id.eq(id), keyed::t::s.eq(s), keyed::t::live.eq(live)))
        .execute(connection)
}

fn is_unique_violation(result: &diesel::QueryResult<usize>) -> bool {
    matches!(
        result,
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _
        ))
    )
}

/// A repeated NULL key is a unique violation, among predicate rows only.
#[test]
fn unique_index_nulls_not_distinct_refuses_a_repeated_null() {
    let mut connection = helpers::apply_translated_ddl(
        &format!("{KEYED_TABLE} CREATE UNIQUE INDEX i ON t (s) NULLS NOT DISTINCT WHERE live = 1;"),
        &Pg2SqliteOptions::default(),
    );
    insert_keyed(&mut connection, 1, None, 1).expect("the first NULL key is accepted");
    insert_keyed(&mut connection, 2, None, 0).expect("a NULL outside the predicate is accepted");
    let repeated = insert_keyed(&mut connection, 3, None, 1);
    assert!(is_unique_violation(&repeated), "a repeated NULL key must collide: {repeated:?}");
}

/// A `COLLATE` written on an index key decides which values collide, beside a
/// NULL too.
#[test]
fn unique_index_nulls_not_distinct_keeps_an_explicit_collation() {
    use diesel::prelude::*;

    let mut connection = helpers::apply_translated_ddl(
        r#"CREATE TABLE pairs (id INT PRIMARY KEY, a INT, s TEXT);
           CREATE UNIQUE INDEX pairs_a_s ON pairs (a, s COLLATE "NOCASE") NULLS NOT DISTINCT;"#,
        &Pg2SqliteOptions::default(),
    );
    let mut pair = |id: i32, s: &str| {
        diesel::insert_into(keyed::pairs::table)
            .values((
                keyed::pairs::id.eq(id),
                keyed::pairs::a.eq(None::<i32>),
                keyed::pairs::s.eq(s),
            ))
            .execute(&mut connection)
    };
    pair(1, "x").expect("the first key is accepted");
    let folded = pair(2, "X");
    assert!(is_unique_violation(&folded), "NOCASE makes x and X the same key: {folded:?}");
}

/// Dropping the index lifts its rule, in the same batch and in a migration.
#[test]
fn dropping_a_nulls_not_distinct_index_lifts_its_rule() {
    let create = format!("{KEYED_TABLE} CREATE UNIQUE INDEX i ON t (s) NULLS NOT DISTINCT;");
    let options = Pg2SqliteOptions::default();

    let mut same_batch =
        helpers::apply_translated_ddl(&format!("{create} DROP INDEX i;"), &options);
    insert_keyed(&mut same_batch, 1, None, 1).expect("the first NULL key is accepted");
    insert_keyed(&mut same_batch, 2, None, 1).expect("the index no longer collides NULLs");

    let mut migrated = helpers::apply_translated_ddl(&create, &options);
    let schema = Pg2Sqlite::default().sql(&create).unwrap().build_schema().unwrap();
    let migration = Pg2Sqlite::default()
        .sql("DROP INDEX i;")
        .unwrap()
        .translate_with_schema(&schema, &options)
        .expect("translate the migration")
        .into_vec();
    for statement in &migration {
        // Translated DDL, which the query DSL cannot express.
        diesel::RunQueryDsl::execute(diesel::sql_query(statement.to_string()), &mut migrated)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    insert_keyed(&mut migrated, 1, None, 1).expect("the first NULL key is accepted");
    insert_keyed(&mut migrated, 2, None, 1).expect("the index no longer collides NULLs");
}

/// An unnamed index enforces the rule, and `DROP INDEX` under the name
/// PostgreSQL gives it lifts the rule with its companion.
#[test]
fn unnamed_unique_index_nulls_not_distinct_follows_its_postgresql_name() {
    let create = format!("{KEYED_TABLE} CREATE UNIQUE INDEX ON t (s) NULLS NOT DISTINCT;");
    let options = Pg2SqliteOptions::default();

    let mut created = helpers::apply_translated_ddl(&create, &options);
    insert_keyed(&mut created, 1, None, 1).expect("the first NULL key is accepted");
    let repeated = insert_keyed(&mut created, 2, None, 1);
    assert!(is_unique_violation(&repeated), "a repeated NULL key must collide: {repeated:?}");

    let mut dropped =
        helpers::apply_translated_ddl(&format!("{create} DROP INDEX t_s_idx;"), &options);
    insert_keyed(&mut dropped, 1, None, 1).expect("the first NULL key is accepted");
    insert_keyed(&mut dropped, 2, None, 1).expect("the index no longer collides NULLs");
}

/// An index whose PostgreSQL name cannot be predicted is refused rather than
/// emitted without one.
#[test]
fn unnamed_index_without_a_predictable_name_is_refused() {
    let error =
        translate_result(&format!("{KEYED_TABLE} CREATE INDEX ON t (((s || 'x')::nvarchar));"))
            .expect_err("an index SQLite would reject must be refused");
    assert!(error.contains("Name the index"), "expected the error to ask for a name: {error}");
}

/// On a secured table the rule holds for writes through the view.
#[test]
fn nulls_not_distinct_on_a_secured_table_holds_on_its_backing_table() {
    use diesel::prelude::*;

    let mut connection = helpers::apply_translated_ddl(
        "CREATE TABLE notes (id INT PRIMARY KEY, title TEXT, UNIQUE NULLS NOT DISTINCT (title));
         ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
         CREATE POLICY anyone ON notes USING (true) WITH CHECK (true);
         CREATE TABLE tags (id INT PRIMARY KEY, label TEXT);
         ALTER TABLE tags ENABLE ROW LEVEL SECURITY;
         CREATE POLICY anyone ON tags USING (true) WITH CHECK (true);
         CREATE UNIQUE INDEX tags_label ON tags (label) NULLS NOT DISTINCT;",
        &Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"),
    );
    let mut note = |id: i32| {
        diesel::insert_into(keyed::notes::table)
            .values((keyed::notes::id.eq(id), keyed::notes::title.eq(None::<&str>)))
            .execute(&mut connection)
    };
    note(1).expect("the first NULL title is accepted");
    let repeated = note(2);
    assert!(is_unique_violation(&repeated), "a repeated NULL title must collide: {repeated:?}");

    let mut tag = |id: i32| {
        diesel::insert_into(keyed::tags::table)
            .values((keyed::tags::id.eq(id), keyed::tags::label.eq(None::<&str>)))
            .execute(&mut connection)
    };
    tag(1).expect("the first NULL label is accepted");
    let repeated = tag(2);
    assert!(is_unique_violation(&repeated), "a repeated NULL label must collide: {repeated:?}");
}

/// The default spelling matches SQLite, so the index still translates.
#[test]
fn unique_index_nulls_distinct_is_translated() {
    let sql = translate(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         CREATE UNIQUE INDEX i ON t (s) NULLS DISTINCT;",
    );
    assert!(
        !sql.to_uppercase().contains("NULLS"),
        "the clause has no SQLite form and must not reach the output: {sql}"
    );
    execute_translated(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         CREATE UNIQUE INDEX i ON t (s) NULLS DISTINCT;",
    );
}

/// SQLite rejects a `NULLS` qualifier inside an index definition, at every
/// version: measured on 3.51.1, `CREATE INDEX i ON t (n DESC NULLS LAST)`
/// answers `unsupported use of NULLS LAST`. The clause used to be copied
/// through, so the index could not be created at all.
#[test]
fn an_index_column_nulls_qualifier_is_dropped() {
    // Both NULL and non-NULL rows, so the index is exercised over the values
    // whose ordering the dropped qualifier was about.
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         CREATE INDEX i ON t (n DESC NULLS LAST);
         INSERT INTO t VALUES (1, 5), (2, NULL);
         SELECT count(*) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2".to_string())]);

    let sql = translate(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         CREATE INDEX i ON t (n DESC NULLS LAST);",
    );
    assert!(sql.to_uppercase().contains("DESC"), "the direction IS legal and must survive: {sql}");
}

/// Dropping it is reported, since the index then serves fewer orderings than
/// the one PostgreSQL would have built.
#[test]
fn dropping_an_index_nulls_qualifier_warns() {
    let warnings = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, n INT);
              CREATE INDEX i ON t (n DESC NULLS LAST);",
        )
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate")
        .warnings;

    assert!(
        warnings.iter().any(|warning| {
            matches!(
                warning,
                pg2sqlite::warnings::TranslationWarning::LossyDrop { construct, .. }
                    if *construct == "NULLS FIRST/LAST"
            )
        }),
        "expected a LossyDrop naming the qualifier, got {warnings:?}"
    );
}

/// An index without the qualifier loses nothing, so it must not warn. Guards
/// the warning from firing on every index.
#[test]
fn an_ordinary_index_does_not_warn() {
    let warnings = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, n INT);
              CREATE INDEX i ON t (n DESC);",
        )
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate")
        .warnings;

    assert!(warnings.is_empty(), "an ordinary index has nothing to report: {warnings:?}");
}

/// SQLite has no operator classes: `CREATE INDEX i ON t (s text_pattern_ops)`
/// answers `near "text_pattern_ops": syntax error`, so the clause was making
/// the index unbuildable.
///
/// The spelling here is UNIQUE on purpose. That is the only case where an
/// opclass could decide something other than plan shape, since PostgreSQL's
/// pattern classes compare bitwise while the default class compares by
/// collation. The two only disagree under a nondeterministic collation, and
/// `Expr::Collate` already refuses every collation but SQLite's three, all
/// deterministic, so no column that reaches SQLite can tell them apart.
#[test]
fn an_index_operator_class_is_dropped() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         CREATE UNIQUE INDEX i ON t (s text_pattern_ops);
         INSERT INTO t VALUES (1, 'a'), (2, 'A');
         SELECT count(*) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2".to_string())], "the two spellings must stay distinct");
}

/// Dropping it is reported, since the index then serves fewer queries than the
/// PostgreSQL one. The report is a downgrade rather than a drop: the index is
/// still emitted, and it names which column lost which class, where the old
/// warning could only say that some index somewhere had one.
#[test]
fn dropping_an_index_operator_class_warns() {
    let warnings = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
             CREATE INDEX i ON t (s text_pattern_ops);",
        )
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate")
        .warnings;

    assert!(
        warnings.iter().any(|warning| {
            matches!(
                warning,
                pg2sqlite::warnings::TranslationWarning::LossyDowngrade { construct, from, location, .. }
                    if construct == "index operator class"
                        && from.contains("text_pattern_ops")
                        && location == "s"
            )
        }),
        "expected a LossyDowngrade naming the column and the class, got {warnings:?}"
    );
}

/// An index carrying an ordering qualifier is ingested by the schema builder.
///
/// `sql-traits` used to reject every one of these as having no columns, because
/// it rendered each column back to a string including the `DESC` and handed the
/// result to `parse_expr`. Fixed upstream; this holds the fix.
#[test]
fn an_index_with_an_ordering_qualifier_is_ingested() {
    fn build(index: &str) -> Result<(), String> {
        let sql = format!("CREATE TABLE t (id INT PRIMARY KEY, n INT, m INT);\n{index}");
        let statements =
            sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &sql)
                .expect("the fixture must parse");
        sql_traits::structs::ParserDB::from_statements(statements, "pin".to_owned())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    for index in [
        "CREATE INDEX i ON t (n);",
        "CREATE INDEX i ON t (n DESC);",
        "CREATE INDEX i ON t (n ASC);",
        "CREATE INDEX i ON t (n NULLS FIRST);",
        "CREATE INDEX i ON t (n, m DESC);",
        "CREATE UNIQUE INDEX i ON t (n DESC);",
    ] {
        build(index).unwrap_or_else(|e| panic!("{index} must ingest, got: {e}"));
    }
}
