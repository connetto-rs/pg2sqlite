//! Lossy-drop warnings emitted during translation.
//!
//! `LISTEN`/`UNLISTEN`/`NOTIFY` have no SQLite equivalent, so they are
//! dropped. The new `translate_with_report` API surfaces a
//! `TranslationWarning::LossyDrop` for each so callers know what was
//! discarded.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::{DeclaredObject, DowngradeLocation, TranslationWarning},
};

fn warnings_for(pg: &str) -> Vec<TranslationWarning> {
    warnings_under(pg, &Pg2SqliteOptions::default())
}

fn warnings_under(pg: &str, options: &Pg2SqliteOptions) -> Vec<TranslationWarning> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_with_report(options)
        .expect("translate")
        .warnings
}

fn assert_lossy_drop(warning: &TranslationWarning, expected_construct: &str) {
    match warning {
        TranslationWarning::LossyDrop { construct, .. } => {
            assert_eq!(*construct, expected_construct);
        }
        other => panic!("expected a LossyDrop warning, got {other:?}"),
    }
}

#[test]
fn listen_emits_lossy_drop_warning() {
    let warns = warnings_for("LISTEN ch;");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "LISTEN");
}

#[test]
fn unlisten_emits_lossy_drop_warning() {
    let warns = warnings_for("UNLISTEN ch;");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "UNLISTEN");
}

#[test]
fn notify_emits_lossy_drop_warning() {
    let warns = warnings_for("NOTIFY ch, 'msg';");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "NOTIFY");
}

#[test]
fn create_table_emits_no_warnings() {
    let warns = warnings_for("CREATE TABLE t (id INTEGER PRIMARY KEY);");
    assert!(warns.is_empty(), "expected no warnings, got {warns:?}");
}

#[test]
fn translate_with_report_preserves_translated_statements() {
    let report = Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INTEGER PRIMARY KEY);")
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate");
    assert_eq!(report.script.files.len(), 1, "the CREATE TABLE alone");
    assert!(report.warnings.is_empty(), "nothing was dropped, got {:?}", report.warnings);
}

#[test]
fn mixed_statements_collect_warnings_in_order() {
    let warns = warnings_for(
        "CREATE TABLE t (id INTEGER PRIMARY KEY);\n\
         LISTEN ch;\n\
         NOTIFY ch, 'msg';",
    );
    assert_eq!(warns.len(), 2);
    assert_lossy_drop(&warns[0], "LISTEN");
    assert_lossy_drop(&warns[1], "NOTIFY");
}

#[test]
fn create_type_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE TYPE address AS (street TEXT, city TEXT);");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE TYPE");
}

/// An enum's labels live on in every column of the type, so its definition
/// drops nothing.
#[test]
fn create_type_enum_warns_nothing() {
    assert_eq!(warnings_for("CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');"), vec![]);
}

#[test]
fn create_domain_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE DOMAIN positive AS INTEGER CHECK (VALUE > 0);");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE DOMAIN");
}

#[test]
fn create_server_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE SERVER remote FOREIGN DATA WRAPPER postgres_fdw;");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE SERVER");
}

#[test]
fn create_foreign_table_emits_lossy_drop_warning() {
    let warns = warnings_for(
        "CREATE SERVER s FOREIGN DATA WRAPPER w;\n\
         CREATE FOREIGN TABLE f (id INT) SERVER s;",
    );
    assert_eq!(warns.len(), 2);
    assert_lossy_drop(&warns[0], "CREATE SERVER");
    assert_lossy_drop(&warns[1], "CREATE FOREIGN TABLE");
}

#[test]
fn create_role_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE ROLE alice;");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE ROLE");
}

#[test]
fn create_user_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE USER bob;");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE USER");
}

#[test]
fn grant_emits_lossy_drop_warning() {
    // GRANT requires the role to exist in the schema, so seed it.
    let warns = warnings_for(
        "CREATE TABLE t (id INTEGER PRIMARY KEY);\n\
         CREATE ROLE alice;\n\
         GRANT SELECT ON t TO alice;",
    );
    assert_eq!(warns.len(), 2);
    assert_lossy_drop(&warns[0], "CREATE ROLE");
    assert_lossy_drop(&warns[1], "GRANT");
}

#[test]
fn revoke_emits_lossy_drop_warning() {
    // REVOKE requires a matching GRANT, so seed both.
    let warns = warnings_for(
        "CREATE TABLE t (id INTEGER PRIMARY KEY);\n\
         CREATE ROLE alice;\n\
         GRANT SELECT ON t TO alice;\n\
         REVOKE SELECT ON t FROM alice;",
    );
    assert_eq!(warns.len(), 3);
    assert_lossy_drop(&warns[0], "CREATE ROLE");
    assert_lossy_drop(&warns[1], "GRANT");
    assert_lossy_drop(&warns[2], "REVOKE");
}

#[test]
fn revoke_of_role_membership_emits_lossy_drop_warning() {
    let report = Pg2Sqlite::default()
        .sql("CREATE ROLE a; CREATE ROLE b; GRANT a TO b; REVOKE a FROM b;")
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate");
    assert_eq!(report.script.iter().count(), 0, "expected no SQLite statement");
    assert_eq!(report.warnings.len(), 4);
    assert_lossy_drop(&report.warnings[0], "CREATE ROLE");
    assert_lossy_drop(&report.warnings[1], "CREATE ROLE");
    assert_lossy_drop(&report.warnings[2], "GRANT");
    assert_lossy_drop(&report.warnings[3], "REVOKE");
}

#[test]
fn alter_default_privileges_emits_lossy_drop_warning() {
    let report = Pg2Sqlite::default()
        .sql(
            "CREATE ROLE app;\n\
             ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO app;\n\
             ALTER DEFAULT PRIVILEGES REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;",
        )
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate");
    assert_eq!(report.script.iter().count(), 0, "expected no SQLite statement");
    assert_eq!(report.warnings.len(), 3);
    assert_lossy_drop(&report.warnings[0], "CREATE ROLE");
    assert_lossy_drop(&report.warnings[1], "ALTER DEFAULT PRIVILEGES");
    assert_lossy_drop(&report.warnings[2], "ALTER DEFAULT PRIVILEGES");
}

#[test]
fn alter_role_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE ROLE alice;\nALTER ROLE alice WITH SUPERUSER;");
    assert_eq!(warns.len(), 2);
    assert_lossy_drop(&warns[0], "CREATE ROLE");
    assert_lossy_drop(&warns[1], "ALTER ROLE");
}

#[test]
fn alter_user_emits_lossy_drop_warning() {
    // Postgres `ALTER USER` is a synonym for `ALTER ROLE` and parses as
    // `Statement::AlterRole` on apache main (upstream #2374), so it
    // collapses onto the ALTER ROLE warning.
    let warns = warnings_for("CREATE USER alice;\nALTER USER alice WITH SUPERUSER;");
    assert_eq!(warns.len(), 2);
    assert_lossy_drop(&warns[0], "CREATE USER");
    assert_lossy_drop(&warns[1], "ALTER ROLE");
}

/// The downgrades a translation reports, each as its location, source and
/// target.
fn downgrades_for(pg: &str) -> Vec<(DowngradeLocation, String, String)> {
    warnings_for(pg)
        .into_iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LossyDowngrade { location, from, to, .. } => {
                    Some((location, from, to))
                }
                _ => None,
            }
        })
        .collect()
}

/// Where each downgrade a translation reports happened.
fn locations_for(pg: &str) -> Vec<DowngradeLocation> {
    locations_under(pg, &Pg2SqliteOptions::default())
}

/// Where each downgrade a translation under `options` reports happened.
fn locations_under(pg: &str, options: &Pg2SqliteOptions) -> Vec<DowngradeLocation> {
    warnings_under(pg, options)
        .into_iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LossyDowngrade { location, .. } => Some(location),
                _ => None,
            }
        })
        .collect()
}

/// The location of a column's downgrade.
fn column(table: &str, column: &str) -> DowngradeLocation {
    DowngradeLocation::Column { table: table.to_owned(), column: column.to_owned() }
}

/// The location of an index's downgrade.
fn index(table: &str, name: &str) -> DowngradeLocation {
    DowngradeLocation::Object(DeclaredObject::Index {
        table: table.to_owned(),
        name: name.to_owned(),
    })
}

/// The location of a downgraded expression held by `object`.
fn expression(object: Option<DeclaredObject>, text: &str) -> DowngradeLocation {
    DowngradeLocation::Expression { object, text: text.to_owned() }
}

/// SQLite has no zone-aware temporal type, so the column becomes TEXT holding
/// whatever offset the writer put in it. Every PostgreSQL spelling that
/// carries a zone loses it, and all four used to say nothing.
#[test]
fn a_zone_carrying_column_reports_the_zone_it_loses() {
    for declared in ["TIMESTAMPTZ", "TIMESTAMP WITH TIME ZONE", "TIMETZ", "TIME WITH TIME ZONE"] {
        let downgrades =
            downgrades_for(&format!("CREATE TABLE events (id INT PRIMARY KEY, at {declared});"));
        assert_eq!(downgrades.len(), 1, "{declared} should report one downgrade: {downgrades:?}");
        assert_eq!(downgrades[0].0, column("events", "at"));
        assert_eq!(downgrades[0].2, "TEXT");
    }
}

/// Guards the report from firing on a temporal type with no zone to lose.
/// This passes before the change too.
#[test]
fn a_zoneless_temporal_column_reports_nothing() {
    for declared in ["TIMESTAMP", "TIMESTAMP WITHOUT TIME ZONE", "TIME", "DATE"] {
        let downgrades =
            downgrades_for(&format!("CREATE TABLE events (id INT PRIMARY KEY, at {declared});"));
        assert!(downgrades.is_empty(), "{declared} loses no zone: {downgrades:?}");
    }
}

/// A column name alone does not identify a column, so the location carries the
/// table too.
#[test]
fn a_column_downgrade_names_its_table() {
    let downgrades = downgrades_for("CREATE TABLE people (id INT PRIMARY KEY, initials CHAR(3));");
    assert_eq!(downgrades.len(), 1, "{downgrades:?}");
    assert_eq!(downgrades[0].0, column("people", "initials"));
}

/// A column added later knows its table just as well.
#[test]
fn an_added_column_downgrade_names_its_table() {
    let downgrades = downgrades_for(
        "CREATE TABLE people (id INT PRIMARY KEY);
         ALTER TABLE people ADD COLUMN seen_at TIMESTAMPTZ;",
    );
    assert_eq!(downgrades.len(), 1, "{downgrades:?}");
    assert_eq!(downgrades[0].0, column("people", "seen_at"));
}

/// An operator class names its index, telling apart same-named columns of two
/// tables.
#[test]
fn an_operator_class_downgrade_names_its_index_and_the_class() {
    let downgrades = downgrades_for(
        "CREATE TABLE notes (id INT PRIMARY KEY, title TEXT);
         CREATE TABLE tags (id INT PRIMARY KEY, title TEXT);
         CREATE INDEX notes_title ON notes (title text_pattern_ops);
         CREATE INDEX tags_title ON tags (title text_pattern_ops);",
    );
    assert!(
        downgrades.iter().all(|(_, from, _)| from.contains("text_pattern_ops")),
        "the warning should name the class that was dropped: {downgrades:?}"
    );
    let locations: Vec<_> = downgrades.into_iter().map(|(location, ..)| location).collect();
    assert_eq!(locations, [index("notes", "notes_title"), index("tags", "tags_title")]);
}

/// PostgreSQL refuses an operator class in a `PRIMARY KEY` or `UNIQUE`
/// constraint.
#[test]
fn an_operator_class_in_a_constraint_is_refused() {
    for constraint in ["UNIQUE", "PRIMARY KEY"] {
        let result = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE TABLE notes (title TEXT, {constraint} (title text_pattern_ops));"
            ))
            .expect("parse")
            .translate_with_report(&Pg2SqliteOptions::default());
        assert!(result.is_err(), "{constraint} with an operator class translated: {result:?}");
    }
}

/// An index method and an `INCLUDE` list are dropped from the index that
/// declares them.
#[test]
fn an_index_clause_downgrade_names_its_index() {
    assert_eq!(
        locations_for(
            "CREATE TABLE notes (id INT PRIMARY KEY, code TEXT);
             CREATE INDEX notes_hash ON notes USING hash (code);
             CREATE INDEX notes_covering ON public.notes (code) INCLUDE (id);",
        ),
        [index("notes", "notes_hash"), index("notes", "notes_covering")]
    );
}

/// A cast read by SQLite's date functions names the view whose definition
/// holds it.
#[test]
fn a_cast_downgrade_in_a_view_names_the_view() {
    assert_eq!(
        locations_for(
            "CREATE TABLE notes (id INT PRIMARY KEY, title TEXT);
             CREATE VIEW recent AS SELECT CAST(title AS timestamptz) AS at FROM notes;",
        ),
        [expression(
            Some(DeclaredObject::View { name: "recent".to_owned() }),
            "CAST(title AS TIMESTAMPTZ)"
        )]
    );
}

/// A cast in a table's `CHECK` names the table.
#[test]
fn a_cast_downgrade_in_a_check_names_the_table() {
    assert_eq!(
        locations_for(
            "CREATE TABLE notes (id INT PRIMARY KEY, title TEXT \
             CHECK (CAST(title AS date) > DATE '2000-01-01'));",
        ),
        [expression(
            Some(DeclaredObject::Table { name: "notes".to_owned() }),
            "CAST(title AS DATE)"
        )]
    );
}

/// A cast in a policy names the policy and its table, whichever statement
/// the policy's view and triggers are built under.
#[test]
fn a_cast_downgrade_in_a_policy_names_the_policy() {
    let locations = locations_under(
        "CREATE TABLE notes (id INT PRIMARY KEY, title TEXT);
         ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
         CREATE POLICY fresh ON notes FOR SELECT USING (CAST(title AS date) > DATE '2000-01-01');",
        &Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"),
    );
    let policy =
        Some(DeclaredObject::Policy { table: "notes".to_owned(), name: "fresh".to_owned() });
    assert!(!locations.is_empty(), "the policy's cast reported nothing");
    assert!(
        locations.iter().all(|location| {
            matches!(location, DowngradeLocation::Expression { object, .. } if *object == policy)
        }),
        "{locations:?} should all name the policy"
    );
}

/// A cast in a trigger's function body names the trigger and its table.
#[test]
fn a_cast_downgrade_in_a_trigger_names_the_trigger() {
    let locations = locations_for(
        "CREATE TABLE notes (id INT PRIMARY KEY, title TEXT, day DATE);
         CREATE FUNCTION stamp() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN NEW.day := CAST(NEW.title AS date); RETURN NEW; END $$;
         CREATE TRIGGER notes_stamp BEFORE INSERT ON notes FOR EACH ROW EXECUTE FUNCTION stamp();",
    );
    let trigger =
        Some(DeclaredObject::Trigger { table: "notes".to_owned(), name: "notes_stamp".to_owned() });
    assert!(!locations.is_empty(), "the trigger's cast reported nothing");
    assert!(
        locations.iter().all(|location| {
            matches!(location, DowngradeLocation::Expression { object, .. } if *object == trigger)
        }),
        "{locations:?} should all name the trigger"
    );
}

/// A query declares nothing, so its cast names no object.
#[test]
fn a_cast_downgrade_in_a_query_names_no_object() {
    assert_eq!(
        locations_for(
            "CREATE TABLE notes (id INT PRIMARY KEY, title TEXT);
             SELECT CAST(title AS date) FROM notes;",
        ),
        [expression(None, "CAST(title AS DATE)")]
    );
}

/// Foreign keys are enforced only under a connection setting, which no
/// table, column or index owns.
#[test]
fn the_foreign_key_pragma_downgrade_names_the_script() {
    assert_eq!(
        locations_for(
            "CREATE TABLE notes (id INT PRIMARY KEY);
             CREATE TABLE tags (id INT PRIMARY KEY, note INT REFERENCES notes (id));",
        ),
        [DowngradeLocation::Script]
    );
}

#[test]
fn nested_warning_contract_is_feature_independent() {
    assert_eq!(
        warnings_for("CREATE TABLE labels (value CHAR(3));"),
        vec![TranslationWarning::LossyDowngrade {
            construct: "CHAR".to_string(),
            from: "CHAR(3)".to_string(),
            to: "TEXT".to_string(),
            location: column("labels", "value"),
            reason: "SQLite stores the value as given, so it is no longer blank padded to the declared width."
                .to_string(),
        }]
    );
}
