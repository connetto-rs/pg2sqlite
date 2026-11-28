//! Constraints reading a column that only a `BEFORE` row trigger the replica
//! leaves to the server fills.
//!
//! PostgreSQL checks the row after its `BEFORE` triggers fill it, and the
//! replica runs no trigger it leaves to the server, so such a constraint
//! leaves the replica's table, is enforced on the writes that trigger does
//! not fire on, and is reported.

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
  CONSTRAINT children_moved CHECK (previous_kind <> 'archive'),
  UNIQUE (parent_id, id)
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
CREATE FUNCTION require_parent() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM public.parents parent WHERE parent.id = NEW.parent_id) THEN
    RAISE EXCEPTION 'no parent';
  END IF;
  RETURN NEW;
END $$;
ALTER FUNCTION copy_parent() OWNER TO archivist;
ALTER FUNCTION remember_kind() OWNER TO archivist;
ALTER FUNCTION require_parent() OWNER TO archivist;
CREATE TRIGGER children_guard BEFORE INSERT ON children
  FOR EACH ROW EXECUTE FUNCTION require_parent();
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

/// The constraints and triggers on `items` left to the server, by construct
/// and name, when its `BEFORE INSERT` trigger runs `body` as a definer
/// reading parents the acting role cannot see.
fn left_on_items(body: &str) -> Vec<(String, String)> {
    let schema = format!(
        "
CREATE ROLE app;
CREATE ROLE archivist BYPASSRLS;
CREATE TABLE parents (id INT PRIMARY KEY, kind TEXT NOT NULL, owner_id INT NOT NULL);
ALTER TABLE parents ENABLE ROW LEVEL SECURITY;
CREATE POLICY parents_own ON parents TO app USING (owner_id = 1);
CREATE TABLE items (
  id INT PRIMARY KEY,
  parent_id INT NOT NULL,
  kind TEXT CHECK (kind <> ''),
  tally BIGINT CHECK (tally >= 0),
  label TEXT
);
GRANT SELECT ON parents TO app, archivist;
GRANT SELECT, INSERT ON items TO app;
CREATE FUNCTION fill() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
{body}
$$;
ALTER FUNCTION fill() OWNER TO archivist;
CREATE TRIGGER items_fill BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION fill();
"
    );
    Pg2Sqlite::default()
        .sql(&schema)
        .expect("parse")
        .translate_with_report(&options())
        .unwrap_or_else(|error| panic!("{error}\n{body}"))
        .warnings
        .into_iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LeftToServer { construct, name, table, .. }
                    if table == "items" =>
                {
                    Some((construct, name))
                }
                _ => None,
            }
        })
        .collect()
}

#[test]
fn every_form_assigning_new_leaves_the_constraints_on_what_it_fills() {
    let looped = "
BEGIN
  FOR NEW.tally IN SELECT count(*) FROM public.parents parent WHERE parent.id = NEW.parent_id LOOP
    NULL;
  END LOOP;
  RETURN NEW;
END";
    let whole_row = "
BEGIN
  SELECT NEW.id, NEW.parent_id, parent.kind, NEW.tally, NEW.label INTO NEW
    FROM public.parents parent WHERE parent.id = NEW.parent_id;
  RETURN NEW;
END";
    let unchecked = "
BEGIN
  SELECT parent.kind INTO NEW.label FROM public.parents parent WHERE parent.id = NEW.parent_id;
  RETURN NEW;
END";
    let tally = ("CHECK", "items_tally_check");
    let kind = ("CHECK", "items_kind_check");
    for (body, constraints) in [
        (looped, &[tally][..]),
        (whole_row, &[kind, tally, ("NOT NULL", "parent_id")]),
        (unchecked, &[]),
    ] {
        let expected: Vec<(String, String)> = constraints
            .iter()
            .chain(&[("TRIGGER", "items_fill")])
            .map(|(construct, name)| ((*construct).to_owned(), (*name).to_owned()))
            .collect();
        assert_eq!(left_on_items(body), expected, "{body}");
    }
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
    let filling = |trigger: &str, column: &str, events: &str, enforced: &str| {
        format!(
            "TRIGGER {trigger} on children fills {column} on {events} before PostgreSQL checks \
             the row, and the replica leaves it to the server, so it enforces the constraint \
             {enforced}"
        )
    };
    let kind = filling("children_copy", "parent_kind", "INSERT", "on UPDATE changing parent_kind");
    let archived = filling(
        "children_copy",
        "parent_archived",
        "INSERT",
        "on UPDATE changing parent_archived or note",
    );
    let moved = filling(
        "children_move",
        "previous_kind",
        "UPDATE where its WHEN holds",
        "on INSERT and on UPDATE changing previous_kind unless TRIGGER children_move fires",
    );
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

fn update_kind(connection: &mut SqliteConnection, kind: Option<&str>) -> QueryResult<usize> {
    diesel::update(children::table).set(children::parent_kind.eq(kind)).execute(connection)
}

#[test]
fn an_update_changing_a_column_filled_on_insert_alone_is_checked() {
    let mut connection =
        helpers::apply_with_options(SCHEMA, &options()).expect("apply the replica");
    insert(&mut connection, Some("first")).expect("PostgreSQL fills parent_kind before its checks");
    let error = update_kind(&mut connection, Some("ownable"))
        .expect_err("children_copy does not fire on UPDATE, so PostgreSQL checks the kind");
    assert!(
        error.to_string().contains(
            "new row for relation \"children\" violates check constraint \
             \"children_parent_kind_check\""
        ),
        "{error}"
    );
    update_kind(&mut connection, Some("folder")).expect("a folder child passes the check");
    let error = update_kind(&mut connection, None).expect_err("parent_kind is NOT NULL on UPDATE");
    assert!(
        error.to_string().contains(
            "null value in column \"parent_kind\" of relation \"children\" violates not-null \
             constraint"
        ),
        "{error}"
    );
    let stored: Option<String> =
        children::table.select(children::parent_kind).first(&mut connection).expect("stored");
    assert_eq!(stored.as_deref(), Some("folder"));
}

#[test]
fn an_update_leaving_the_checked_columns_alone_lands_before_the_server_row() {
    let mut connection =
        helpers::apply_with_options(SCHEMA, &options()).expect("apply the replica");
    diesel::insert_into(children::table)
        .values((
            children::id.eq(1),
            children::parent_id.eq(7),
            children::parent_kind.eq("ownable"),
            children::previous_kind.eq(""),
        ))
        .execute(&mut connection)
        .expect("children_copy overwrites parent_kind on INSERT");
    diesel::update(children::table)
        .set(children::note.eq("second"))
        .execute(&mut connection)
        .expect("PostgreSQL judges the kind children_copy filled, which the update leaves alone");
}

#[test]
fn a_server_trigger_s_when_narrows_the_updates_left_to_it() {
    let mut connection =
        helpers::apply_with_options(SCHEMA, &options()).expect("apply the replica");
    let refused = |error: diesel::result::Error| {
        assert!(
            error.to_string().contains(
                "new row for relation \"children\" violates check constraint \"children_moved\""
            ),
            "{error}"
        );
    };
    let archived = diesel::insert_into(children::table)
        .values((
            children::id.eq(2),
            children::parent_id.eq(7),
            children::previous_kind.eq("archive"),
        ))
        .execute(&mut connection)
        .expect_err("children_move does not fire on INSERT");
    refused(archived);
    insert(&mut connection, None).expect("an empty previous kind passes");
    let kept_parent = diesel::update(children::table)
        .set(children::previous_kind.eq("archive"))
        .execute(&mut connection)
        .expect_err("children_move fires only when parent_id changes");
    refused(kept_parent);
    diesel::update(children::table)
        .set((children::parent_id.eq(8), children::previous_kind.eq("archive")))
        .execute(&mut connection)
        .expect("children_move fills previous_kind before PostgreSQL checks the moved child");
}

diesel::table! {
    sqlite_schema (name) {
        #[sql_name = "type"]
        kind -> Text,
        name -> Text,
    }
}

/// The names of the triggers the replica of `schema` holds.
fn triggers(schema: &str) -> Vec<String> {
    let mut connection =
        helpers::apply_with_options(schema, &options()).expect("apply the replica");
    sqlite_schema::table
        .filter(sqlite_schema::kind.eq("trigger"))
        .select(sqlite_schema::name)
        .load(&mut connection)
        .expect("list the triggers")
}

#[test]
fn dropping_a_checked_column_takes_the_triggers_enforcing_its_constraints() {
    let enforcing = [
        "children_children_parent_kind_check_update",
        "children_children_parent_kind_not_null_update",
        "children_children_moved_insert",
        "children_children_moved_update",
    ];
    let kept = "children_children_open_update";
    let created = triggers(SCHEMA);
    assert!(
        enforcing.iter().chain([&kept]).all(|name| created.iter().any(|held| held == name)),
        "{created:?}"
    );
    let dropped = triggers(&format!(
        "{SCHEMA}
ALTER TABLE children DROP COLUMN previous_kind;
ALTER TABLE children DROP COLUMN parent_kind;"
    ));
    assert!(dropped.iter().any(|name| name == kept), "{dropped:?}");
    assert!(dropped.iter().all(|name| !enforcing.contains(&name.as_str())), "{dropped:?}");
}
