//! Translation policies for table membership and PostgreSQL-only statements.

use pg2sqlite::{
    errors::{Error, RefusalCategory, TranslationDirection},
    prelude::{Pg2Sqlite, Pg2SqliteOptions, Translator},
    warnings::TranslationWarning,
};
use sqlparser::{
    ast::{Expr, InTarget, SelectItem, SetExpr, Statement},
    dialect::SQLiteDialect,
    parser::Parser,
};

const SCHEMA: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY);";

fn assert_refusal(error: Error, direction: TranslationDirection, construct: &str) {
    let Error::TranslationRefusal(refusal) = error else {
        panic!("expected translation refusal, got {error}");
    };
    assert_eq!(refusal.direction(), direction);
    assert_eq!(refusal.category(), RefusalCategory::UnrepresentableSemantics);
    assert!(refusal.detail().contains(construct), "{refusal}");
}

#[test]
fn foreign_data_wrapper_is_reported_as_a_lossy_drop() {
    let report = Pg2Sqlite::default()
        .sql("CREATE FOREIGN DATA WRAPPER remote NO HANDLER NO VALIDATOR;")
        .unwrap()
        .translate_with_report(&Pg2SqliteOptions::default())
        .unwrap();
    assert_eq!(report.script.into_vec(), Vec::<Statement>::new());
    assert!(matches!(
        report.warnings.as_slice(),
        [TranslationWarning::LossyDrop { construct, .. }] if construct == "CREATE FOREIGN DATA WRAPPER"
    ));
}

#[test]
fn rules_and_trigger_alterations_are_refused() {
    for (sql, construct) in [
        ("CREATE RULE r AS ON DELETE TO t DO INSTEAD NOTHING;", "CREATE RULE"),
        ("ALTER TRIGGER tr ON t RENAME TO renamed;", "ALTER TRIGGER"),
        ("ALTER TRIGGER tr ON t DEPENDS ON EXTENSION ext;", "ALTER TRIGGER"),
    ] {
        let error = Pg2Sqlite::default()
            .sql(SCHEMA)
            .unwrap()
            .sql(sql)
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .unwrap_err();
        assert_refusal(error, TranslationDirection::PostgreSqlToSqlite, construct);
    }
}

#[test]
fn reverse_table_membership_is_refused() {
    let translator = Pg2Sqlite::default().sql(SCHEMA).unwrap();
    let schema = translator.build_schema().unwrap();
    for sql in [
        "SELECT 1 IN t",
        "SELECT 1 NOT IN main.t",
        "SELECT 1 IN json_each('[1,2]')",
        "UPDATE t SET id = 2 WHERE id IN t",
        "DELETE FROM t WHERE id NOT IN t",
        "SELECT 1 IN (SELECT id FROM t WHERE id IN t)",
    ] {
        let error = translator.reverse_sql(sql, &schema, &Pg2SqliteOptions::default()).unwrap_err();
        assert_refusal(error, TranslationDirection::SqliteToPostgreSql, "IN (SELECT");
    }
}

#[test]
fn forward_table_membership_ast_is_refused() {
    let translator = Pg2Sqlite::default().sql(SCHEMA).unwrap();
    let schema = translator.build_schema().unwrap();
    for sql in ["SELECT 1 IN t", "SELECT 1 NOT IN json_each('[1,2]')"] {
        let Statement::Query(query) = Parser::parse_sql(&SQLiteDialect {}, sql).unwrap().remove(0)
        else {
            panic!("expected query");
        };
        let SetExpr::Select(select) = *query.body else {
            panic!("expected select");
        };
        let SelectItem::UnnamedExpr(expr) = &select.projection[0] else {
            panic!("expected expression");
        };
        assert!(matches!(expr, Expr::In { target: InTarget::Table { .. }, .. }));
        let error = expr.translate(&schema, &Pg2SqliteOptions::default()).unwrap_err();
        assert_refusal(error, TranslationDirection::PostgreSqlToSqlite, "IN (SELECT");
    }
}
