//! Constraints reading a column that only a `BEFORE` row trigger the replica
//! leaves to the server fills.
//!
//! PostgreSQL checks the row after its `BEFORE` triggers fill it, and the
//! replica runs no trigger it leaves to the server, so such a constraint
//! leaves the replica with the trigger and is reported.

use diesel::{QueryResult, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationWarning};

use crate::helpers;

diesel::table! {
    children (id) {
        id -> Integer,
        parent_id -> Integer,
        parent_kind -> Nullable<Text>,
        parent_archived -> Bool,
        note -> Nullable<Text>,
        previous_kind -> Text,
    }
}

const SCHEMA: &str = "
CREATE ROLE app;
CREATE ROLE archivist BYPASSRLS;
CREATE TABLE parents (id INT PRIMARY KEY, kind TEXT NOT NULL, archived BOOLEAN NOT NULL,
  owner_id INT NOT NULL);
ALTER TABLE parents ENABLE ROW LEVEL SECURITY;
CREATE POLICY parents_own ON parents TO app USING (owner_id = 1);
CREATE TABLE children (
  id INT PRIMARY KEY,
  parent_id INT NOT NULL,
  parent_kind TEXT NOT NULL CHECK (parent_kind <> 'ownable'),
  parent_archived BOOLEAN NOT NULL DEFAULT FALSE,
  note TEXT CHECK (note <> ''),
  previous_kind TEXT NOT NULL,
  CONSTRAINT children_open CHECK (NOT children.parent_archived OR note IS NULL),
  CONSTRAINT children_moved CHECK (previous_kind <> 'archive')
);
ALTER TABLE children ENABLE ROW LEVEL SECURITY;
CREATE POLICY children_all ON children TO app USING (true) WITH CHECK (true);
GRANT SELECT ON parents TO app, archivist;
GRANT SELECT, INSERT, UPDATE ON children TO app;
CREATE FUNCTION copy_parent() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  SELECT parent.kind, parent.archived INTO NEW.parent_kind, NEW.parent_archived
    FROM public.parents parent WHERE parent.id = NEW.parent_id;
  RETURN NEW;
END $$;
CREATE FUNCTION remember_kind() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  NEW.previous_kind := (SELECT parent.kind FROM public.parents parent WHERE parent.id = OLD.parent_id);
  RETURN NEW;
END $$;
ALTER FUNCTION copy_parent() OWNER TO archivist;
ALTER FUNCTION remember_kind() OWNER TO archivist;
CREATE TRIGGER children_copy BEFORE INSERT ON children
  FOR EACH ROW EXECUTE FUNCTION copy_parent();
CREATE TRIGGER children_move BEFORE UPDATE ON children
  FOR EACH ROW WHEN (OLD.parent_id IS DISTINCT FROM NEW.parent_id)
  EXECUTE FUNCTION remember_kind();
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_rls_audit_table_name("rls_audit")
        .with_acting_role("app")
}

fn insert(connection: &mut SqliteConnection, note: Option<&str>) -> QueryResult<usize> {
    diesel::insert_into(children::table)
        .values((
            children::id.eq(1),
            children::parent_id.eq(7),
            children::note.eq(note),
            children::previous_kind.eq(""),
        ))
        .execute(connection)
}

#[test]
fn the_report_names_each_constraint_left_with_the_trigger_filling_its_column() {
    let warnings = Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("parse")
        .translate_with_report(&options())
        .expect("translate");
    let left: Vec<(&str, &str, &str)> = warnings
        .warnings
        .iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LeftToServer { construct, name, table, reason }
                    if table == "children" && construct != "TRIGGER" =>
                {
                    Some((construct.as_str(), name.as_str(), reason.as_str()))
                }
                _ => None,
            }
        })
        .collect();
    let filling = |trigger: &str, column: &str| {
        format!(
            "TRIGGER {trigger} on children, which the replica leaves to the server, fills \
             {column} before PostgreSQL checks the row"
        )
    };
    let kind = filling("children_copy", "parent_kind");
    let archived = filling("children_copy", "parent_archived");
    let moved = filling("children_move", "previous_kind");
    assert_eq!(
        left,
        [
            ("CHECK", "children_parent_kind_check", kind.as_str()),
            ("CHECK", "children_open", archived.as_str()),
            ("CHECK", "children_moved", moved.as_str()),
            ("NOT NULL", "parent_kind", kind.as_str()),
        ]
    );
}

#[test]
fn an_insert_leaving_the_filled_columns_out_lands_until_the_server_fills_them() {
    let mut connection =
        helpers::apply_with_options(SCHEMA, &options()).expect("apply the replica");
    insert(&mut connection, Some("first")).expect("PostgreSQL fills parent_kind before its checks");
    let stored: (Option<String>, bool) = children::table
        .select((children::parent_kind, children::parent_archived))
        .first(&mut connection)
        .expect("the child is stored");
    assert_eq!(stored, (None, false));
}

#[test]
fn a_constraint_reading_no_filled_column_stays() {
    let mut connection =
        helpers::apply_with_options(SCHEMA, &options()).expect("apply the replica");
    let error = insert(&mut connection, Some("")).expect_err("an empty note is refused");
    assert!(error.to_string().contains("note <> ''"), "{error}");
    assert_eq!(children::table.count().get_result::<i64>(&mut connection), Ok(0));
}
