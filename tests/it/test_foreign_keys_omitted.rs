//! A replica holding a partial copy of the server's rows leaves every foreign
//! key to the server under `with_foreign_keys_omitted`, while the translation
//! still reads the keys the schema declares.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::{
    prelude::{
        Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, TranslationReport, UuidRepresentation,
    },
    warnings::TranslationWarning,
};

use crate::helpers;

diesel::table! {
    /// The referenced table.
    folders (id) {
        /// The folder.
        id -> Integer,
    }
}

diesel::table! {
    /// The referencing table.
    files (id) {
        /// The file.
        id -> Integer,
        /// The folder it sits in, removed with it by the server.
        folder -> Integer,
        /// The folder it was made in, checked at commit by the server.
        origin -> Nullable<Integer>,
    }
}

const GRANTEES: &str = "
CREATE ROLE app;
CREATE ROLE authz_owner BYPASSRLS;
CREATE TABLE memberships (user_id UUID PRIMARY KEY, active BOOLEAN NOT NULL);
CREATE TABLE teams (id UUID PRIMARY KEY);
CREATE TABLE team_members (
  team_id UUID NOT NULL REFERENCES teams(id),
  user_id UUID NOT NULL REFERENCES memberships(user_id),
  PRIMARY KEY (team_id, user_id)
);
CREATE TABLE team_parents (
  id UUID PRIMARY KEY REFERENCES teams(id),
  parent_id UUID NOT NULL REFERENCES teams(id)
);
GRANT SELECT ON memberships, teams, team_members, team_parents TO authz_owner;
GRANT SELECT ON memberships, teams, team_members, team_parents TO app;
CREATE FUNCTION caller_grantees() RETURNS SETOF UUID
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  WITH RECURSIVE reached(id) AS (
    SELECT member.team_id
    FROM team_members member
    JOIN memberships membership ON membership.user_id = member.user_id
    WHERE member.user_id = current_setting('app.user_id')::uuid
      AND membership.active = true
    UNION
    SELECT parent.parent_id
    FROM team_parents parent JOIN reached ON parent.id = reached.id
  )
  SELECT id FROM reached
$$;
ALTER FUNCTION caller_grantees() OWNER TO authz_owner;
GRANT EXECUTE ON FUNCTION caller_grantees() TO app;
ALTER TABLE teams ENABLE ROW LEVEL SECURITY;
CREATE POLICY teams_read ON teams FOR SELECT TO app
  USING (id IN (SELECT caller_grantees()));
";

const FOLDERS: &str = "
CREATE TABLE folders (id INT PRIMARY KEY);
CREATE TABLE files (
  id INT PRIMARY KEY,
  folder INT NOT NULL REFERENCES folders (id) ON DELETE CASCADE,
  origin INT,
  CONSTRAINT files_origin FOREIGN KEY (origin) REFERENCES folders (id) MATCH FULL
    DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO folders VALUES (1);
INSERT INTO files VALUES (1, 1, 1);
";

fn omitted() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_foreign_keys_omitted()
}

fn report(pg: &str, options: &Pg2SqliteOptions) -> TranslationReport {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_with_report(options)
        .unwrap_or_else(|error| panic!("{error}"))
}

/// Each foreign key the report names as left to the server, as its table and
/// name.
fn keys_left_to_server(report: &TranslationReport) -> Vec<(String, String)> {
    report
        .warnings
        .iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LeftToServer { construct, name, table, .. }
                    if construct == "FOREIGN KEY" =>
                {
                    Some((table.clone(), name.clone()))
                }
                _ => None,
            }
        })
        .collect()
}

fn owned(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries.iter().map(|(table, name)| ((*table).to_owned(), (*name).to_owned())).collect()
}

fn declares_no_key(report: &TranslationReport) {
    let script: Vec<String> = report.script.iter().map(ToString::to_string).collect();
    for statement in &script {
        assert!(!statement.contains("REFERENCES"), "{statement}");
        assert!(!statement.contains("foreign_keys"), "{statement}");
        assert!(!statement.contains("_pg2sqlite_key"), "{statement}");
    }
}

#[test]
fn a_helper_joining_through_keys_translates_and_emits_none_of_them() {
    let deployment = helpers::deployment::deployment(
        ["app"],
        [helpers::deployment::caller("app.user_id", "uuid")],
    );
    let options = omitted()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_deployment(deployment)
        .with_acting_role("app")
        .with_session_variable(SessionVariableMapping::new("app.user_id", "app_user_id"))
        .with_rls_audit_table_name("rls_violations");

    let report = report(GRANTEES, &options);

    declares_no_key(&report);
    assert_eq!(
        keys_left_to_server(&report),
        owned(&[
            ("team_members", "(team_id) REFERENCES teams"),
            ("team_members", "(user_id) REFERENCES memberships"),
            ("team_parents", "(id) REFERENCES teams"),
            ("team_parents", "(parent_id) REFERENCES teams"),
        ])
    );
    assert!(
        !report.warnings.iter().any(|warning| {
            matches!(
                warning,
                TranslationWarning::LossyDowngrade { construct, .. } if construct == "FOREIGN KEY"
            )
        }),
        "{:?}",
        report.warnings
    );
}

#[test]
fn the_replica_takes_an_orphan_and_keeps_the_children_of_a_deleted_parent() {
    let mut connection = helpers::apply_with_options(FOLDERS, &omitted()).expect("apply");

    diesel::insert_into(files::table)
        .values((files::id.eq(2), files::folder.eq(9), files::origin.eq(Some(9))))
        .execute(&mut connection)
        .expect("the orphan is taken");
    diesel::insert_into(files::table)
        .values((files::id.eq(3), files::folder.eq(1), files::origin.eq(None::<i32>)))
        .execute(&mut connection)
        .expect("the key MATCH FULL refuses is taken");
    diesel::delete(folders::table.find(1)).execute(&mut connection).expect("the parent deletes");

    let kept: Vec<i32> =
        files::table.select(files::id).order(files::id).load(&mut connection).expect("files");
    assert_eq!(kept, [1, 2, 3]);
    assert_eq!(
        keys_left_to_server(&report(FOLDERS, &omitted())),
        owned(&[("files", "(folder) REFERENCES folders"), ("files", "files_origin")])
    );
}

#[test]
fn a_key_into_rows_a_policy_hides_keeps_no_referenced_side() {
    let pg = "CREATE ROLE app;
         CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL);
         ALTER TABLE users ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, UPDATE, DELETE ON users TO app;
         CREATE POLICY users_select ON users FOR SELECT TO app USING (name = 'visible');
         CREATE POLICY users_delete ON users FOR DELETE TO app USING (true);
         CREATE TABLE memberships (id INT PRIMARY KEY,
           user_id INT REFERENCES users (id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED);
         GRANT SELECT, INSERT, UPDATE, DELETE ON memberships TO app;
         ALTER TABLE memberships RENAME TO members;
         ALTER TABLE members DROP COLUMN user_id;";
    let options = omitted()
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_rls_audit_table_name("rls_audit")
        .with_acting_role("app");

    let report = report(pg, &options);

    declares_no_key(&report);
    assert!(report.script.before_commit.is_empty(), "{:?}", report.script.before_commit);
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| matches!(warning, TranslationWarning::HiddenReferencedRows { .. })),
        "{:?}",
        report.warnings
    );
    assert_eq!(
        keys_left_to_server(&report),
        owned(&[("memberships", "(user_id) REFERENCES users")])
    );
}

#[test]
fn an_added_key_adds_nothing() {
    let pg = "CREATE TABLE folders (id INT PRIMARY KEY);
         CREATE TABLE files (id INT PRIMARY KEY, folder INT, origin INT);
         ALTER TABLE files ADD CONSTRAINT files_folder FOREIGN KEY (folder) REFERENCES folders (id);
         ALTER TABLE files ADD COLUMN parent INT REFERENCES files (id) ON DELETE SET NULL;";

    let report = report(pg, &omitted());

    declares_no_key(&report);
    assert_eq!(
        keys_left_to_server(&report),
        owned(&[("files", "files_folder"), ("files", "(parent) REFERENCES files")])
    );
    let mut connection = helpers::apply_with_options(pg, &omitted()).expect("apply");
    connection
        .batch_execute("INSERT INTO files (id, folder, parent) VALUES (1, 9, 9)")
        .expect("the orphan is taken");
}

#[test]
fn a_key_over_interval_columns_is_not_refused() {
    let pg = "CREATE TABLE spans (id INT, length INTERVAL UNIQUE);
         CREATE TABLE uses (length INTERVAL, FOREIGN KEY (length) REFERENCES spans (length));";

    let report = report(pg, &omitted());

    declares_no_key(&report);
    assert_eq!(keys_left_to_server(&report), owned(&[("uses", "(length) REFERENCES spans")]));
}

#[test]
fn a_write_leaving_with_its_table_is_not_refused_for_a_key_acting_on_visible_rows() {
    let pg = "CREATE ROLE app;
         CREATE TABLE grantees (id UUID PRIMARY KEY, kind TEXT NOT NULL);
         CREATE TABLE memberships (id INTEGER PRIMARY KEY,
           grantee UUID REFERENCES grantees (id) ON DELETE CASCADE ON UPDATE SET NULL);
         GRANT ALL ON memberships TO app;
         DELETE FROM grantees;
         UPDATE grantees SET id = gen_random_uuid();";
    let options = omitted()
        .with_uuid_representation(UuidRepresentation::Text)
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_acting_role("app");

    let report = report(pg, &options);

    declares_no_key(&report);
}

#[test]
fn a_referenced_table_drops_a_column_inside_a_transaction() {
    let pg = "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, b INT UNIQUE);
         CREATE TABLE u (x INT REFERENCES t (id) ON DELETE CASCADE);
         INSERT INTO t VALUES (1, 1, 1);
         BEGIN; ALTER TABLE t DROP COLUMN b; COMMIT;";

    let report = report(pg, &omitted());

    declares_no_key(&report);
    let mut connection = helpers::apply_with_options(pg, &omitted()).expect("apply");
    connection.batch_execute("INSERT INTO t (id, a) VALUES (2, 2)").expect("t keeps a and id");
}
