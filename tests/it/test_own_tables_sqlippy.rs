//! SQLippy, with its pedantic and restriction rules raised to warn, reports
//! nothing on the tables the translation adds for itself.

use pg2sqlite::prelude::Pg2SqliteOptions;
use sql_traits::structs::Deployment;
use sqlippy::prelude::{
    Constrainer, DatabaseLike, DefaultConstrainer, Error, IdentifierCase, Level, LintGroup,
    ParseOptions, ParserDB, TableLike,
};
use sqlparser::dialect::SQLiteDialect;

use crate::helpers;

const AUDIT_TABLE: &str = "rls_violations";

const SOURCE: &str = "
CREATE ROLE app;
CREATE ROLE note_owner BYPASSRLS;
CREATE TABLE notes (
  id INT PRIMARY KEY,
  author TEXT NOT NULL,
  revision INT NOT NULL DEFAULT 0
);
CREATE TABLE note_log (note_id INT NOT NULL, revision INT NOT NULL);
GRANT SELECT, INSERT, UPDATE, DELETE ON notes TO app;
GRANT SELECT, INSERT ON note_log TO note_owner;
ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
CREATE POLICY notes_read ON notes FOR SELECT USING (author = current_user);
CREATE POLICY notes_write ON notes FOR ALL
  USING (author = current_user) WITH CHECK (author = current_user);
CREATE FUNCTION bump_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  NEW.revision := OLD.revision + 1;
  RETURN NEW;
END
$$;
CREATE TRIGGER notes_bump BEFORE UPDATE ON notes
  FOR EACH ROW EXECUTE FUNCTION bump_revision();
CREATE FUNCTION log_note() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  INSERT INTO public.note_log VALUES (NEW.id, NEW.revision);
  RETURN NULL;
END
$$;
ALTER FUNCTION log_note() OWNER TO note_owner;
CREATE TRIGGER notes_log AFTER UPDATE OF author ON notes
  FOR EACH ROW EXECUTE FUNCTION log_note();
CREATE FUNCTION check_note() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.revision < 0 THEN
    RAISE EXCEPTION 'negative revision';
  END IF;
  RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER notes_check AFTER INSERT ON notes
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_note();
CREATE TABLE note_bodies (
  note_id INT NOT NULL,
  locale TEXT NOT NULL,
  body TEXT NOT NULL,
  PRIMARY KEY (note_id, locale)
);
GRANT SELECT, INSERT, UPDATE, DELETE ON note_bodies TO app;
CREATE INDEX note_bodies_search ON note_bodies USING GIN (to_tsvector('english', body));
";

/// Every table [`SOURCE`] makes the translation add for itself, so a kind
/// the source stops producing fails the test rather than leaving the lint.
const OWN_TABLES: [&str; 9] = [
    "note_bodies_pg2sqlite_fts_keys",
    "notes_check_pg2sqlite_rows",
    "notes_pg2sqlite_forwards",
    "notes_pg2sqlite_records",
    "notes_pg2sqlite_reentries",
    "pg2sqlite_deferred_barriers",
    "pg2sqlite_deferred_checks",
    "pg2sqlite_definer_frames",
    AUDIT_TABLE,
];

fn is_own(table: &str) -> bool {
    table.contains("pg2sqlite") || table == AUDIT_TABLE
}

#[test]
fn sqlippy_reports_nothing_on_the_tables_the_translation_adds() {
    let options = Pg2SqliteOptions::default()
        .with_deployment(Deployment::new("app", ["app"], []).expect("a valid deployment"))
        .with_acting_role("app")
        .with_rls_audit_table_name(AUDIT_TABLE)
        .with_strict_rls_validation();
    let statements = helpers::translate_pg(SOURCE, &options).expect("the source translates");
    let script = format!("{};", statements.join(";\n"));
    // SQLite compares names case-insensitively and never clips them.
    let database = ParseOptions::default()
        .with_identifier_case(IdentifierCase::Folded)
        .parse::<SQLiteDialect>(&script)
        .expect("the translation parses as SQLite");

    let mut own: Vec<&str> =
        database.tables().map(TableLike::table_name).filter(|table| is_own(table)).collect();
    own.sort_unstable();
    assert_eq!(own, OWN_TABLES, "{script}");

    let mut constrainer = DefaultConstrainer::<ParserDB>::default();
    for group in [LintGroup::Pedantic, LintGroup::Restriction] {
        constrainer.set_group_level(group, Level::Warn);
    }
    let findings: Vec<String> = constrainer
        .diagnostics(&database)
        .into_iter()
        .filter(|diagnostic| {
            match diagnostic {
                Error::Violation(violation) => {
                    is_own(violation.object().split('.').next().unwrap_or_default())
                }
                Error::Inactive(_) => false,
                _ => true,
            }
        })
        .map(|diagnostic| diagnostic.to_string())
        .collect();
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}
