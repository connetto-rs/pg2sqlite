//! A replica that syncs its writes to PostgreSQL marks the writes the server
//! derives, under `with_server_derived_write_function`.
//!
//! The replica uploads the rows its transaction changed, and the server runs
//! its own triggers while applying the upload. Every trigger runs whole on the
//! replica, so the checks reading what a trigger writes judge the rows the
//! server will hold, and each statement a trigger writes with runs between
//! calls of the named function, so the consumer keeps those rows out of the
//! upload. A foreign key whose action writes rows is left to the server
//! whole, since SQLite runs the action where no call can mark it.

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicI32, Ordering};

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, postgres_error},
    warnings::TranslationWarning,
};
use sql_traits::structs::Deployment;
use sqlstate::pg;

use crate::helpers::establish_connection;

diesel::table! {
    /// The organizations of the fixtures.
    organizations (id) {
        /// The organization.
        id -> Integer,
    }
}

diesel::table! {
    /// Who belongs to which organization.
    memberships (organization_id, user_id) {
        /// The organization.
        organization_id -> Integer,
        /// The member.
        user_id -> Integer,
    }
}

diesel::table! {
    /// The teams a member sits in, removed with the membership.
    team_members (team_id, user_id) {
        /// The team.
        team_id -> Integer,
        /// The organization of the membership.
        organization_id -> Integer,
        /// The member.
        user_id -> Integer,
    }
}

diesel::table! {
    /// The roles granted to a member.
    grants (organization_id, user_id) {
        /// The organization.
        organization_id -> Integer,
        /// The member.
        user_id -> Integer,
        /// The role granted.
        role -> Text,
    }
}

diesel::table! {
    /// The rows the observing triggers record.
    observed (relation, user_id) {
        /// The table written.
        relation -> Text,
        /// The row's member.
        user_id -> Integer,
        /// How many marked writes were open when the row was written.
        depth -> Integer,
    }
}

diesel::table! {
    /// The parent of the referential-action fixtures.
    folders (id) {
        /// The folder.
        id -> Integer,
    }
}

diesel::table! {
    /// The child of the referential-action fixtures.
    documents (id) {
        /// The document.
        id -> Integer,
        /// The folder the document sits in, set to NULL by the server.
        folder_id -> Nullable<Integer>,
        /// The folder the document was created in, a plain key.
        origin_id -> Integer,
    }
}

diesel::define_sql_function! {
    /// Opens a marked write for `1` and closes one for `-1`.
    fn derived(step: diesel::sql_types::Integer) -> diesel::sql_types::Integer;
}

diesel::define_sql_function! {
    /// How many marked writes are open.
    fn derived_depth() -> diesel::sql_types::Integer;
}

/// The founding schema of the request: a membership insert runs a definer
/// trigger granting the founder the admin role, and a deferred check refuses
/// an organization without an admin at commit.
const FOUNDING: &str = "
CREATE ROLE founder_writer NOLOGIN BYPASSRLS;
CREATE TABLE organizations (
  id INTEGER PRIMARY KEY,
  created_by_id INTEGER NOT NULL
);
CREATE TABLE memberships (
  organization_id INTEGER REFERENCES organizations,
  user_id INTEGER NOT NULL,
  PRIMARY KEY (organization_id, user_id)
);
CREATE TABLE grants (
  organization_id INTEGER REFERENCES organizations,
  user_id INTEGER NOT NULL,
  role TEXT NOT NULL,
  PRIMARY KEY (organization_id, user_id)
);
GRANT USAGE ON SCHEMA public TO founder_writer;
GRANT SELECT ON organizations TO founder_writer;
GRANT INSERT ON grants TO founder_writer;
CREATE FUNCTION grant_founder() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public AS $$
BEGIN
  INSERT INTO grants (organization_id, user_id, role)
    SELECT id, NEW.user_id, 'admin' FROM organizations
    WHERE id = NEW.organization_id AND created_by_id = NEW.user_id;
  RETURN NULL;
END
$$;
ALTER FUNCTION grant_founder() OWNER TO founder_writer;
CREATE TRIGGER memberships_founder AFTER INSERT ON memberships
  FOR EACH ROW
  EXECUTE FUNCTION grant_founder();
CREATE FUNCTION require_admin() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM grants
    WHERE organization_id = NEW.id AND role = 'admin'
  ) THEN
    RAISE EXCEPTION 'organization requires an admin'
      USING ERRCODE = '23514';
  END IF;
  RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER organizations_admin AFTER INSERT ON organizations
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION require_admin();
";

/// A membership whose delete removes its grants through a trigger and its
/// team memberships through a cascade, and whose insert grants the member
/// through two PL/pgSQL helpers.
const MEMBERSHIPS: &str = "
CREATE TABLE organizations (id INTEGER PRIMARY KEY);
CREATE TABLE memberships (organization_id INTEGER NOT NULL REFERENCES organizations (id),
  user_id INTEGER NOT NULL, PRIMARY KEY (organization_id, user_id));
CREATE TABLE team_members (team_id INTEGER NOT NULL, organization_id INTEGER NOT NULL,
  user_id INTEGER NOT NULL, PRIMARY KEY (team_id, user_id),
  FOREIGN KEY (organization_id, user_id) REFERENCES memberships (organization_id, user_id)
    ON DELETE CASCADE);
CREATE TABLE grants (organization_id INTEGER NOT NULL, user_id INTEGER NOT NULL,
  role TEXT NOT NULL, PRIMARY KEY (organization_id, user_id));
CREATE FUNCTION record_grant(organization INTEGER, member INTEGER) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO grants (organization_id, user_id, role) VALUES (organization, member, 'member');
END
$$;
CREATE FUNCTION grant_member(organization INTEGER, member INTEGER) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM record_grant(organization, member);
END
$$;
CREATE FUNCTION membership_granted() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  PERFORM grant_member(NEW.organization_id, NEW.user_id);
  RETURN NULL;
END
$$;
CREATE TRIGGER memberships_granted AFTER INSERT ON memberships
  FOR EACH ROW EXECUTE FUNCTION membership_granted();
CREATE FUNCTION delete_membership_grants() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  DELETE FROM grants WHERE organization_id = OLD.organization_id AND user_id = OLD.user_id;
  RETURN NULL;
END
$$;
CREATE TRIGGER memberships_grants_removed AFTER DELETE ON memberships
  FOR EACH ROW EXECUTE FUNCTION delete_membership_grants();
";

fn replica_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_server_derived_write_function("derived")
}

/// A replica of `pg` translated under `options`.
struct Replica {
    connection: crate::helpers::Replica,
    /// How many marked writes are open, as `derived` counts them.
    depth: Arc<AtomicI32>,
}

impl Replica {
    fn new(pg: &str, options: &Pg2SqliteOptions) -> Self {
        let script = Pg2Sqlite::default()
            .sql(pg)
            .expect("fixture parses")
            .translate_to_sql(options)
            .expect("fixture translates");
        let mut connection = establish_connection();
        let depth = Arc::new(AtomicI32::new(0));
        let counted = Arc::clone(&depth);
        {
            let mut registration = connection.registration().expect("registration");
            derived_utils::register_nondeterministic_impl(
                registration.sqlite(),
                move |step: i32| counted.fetch_add(step, Ordering::Relaxed) + step,
            )
            .expect("register derived");
            let read = Arc::clone(&depth);
            derived_depth_utils::register_nondeterministic_impl(registration.sqlite(), move || {
                read.load(Ordering::Relaxed)
            })
            .expect("register derived_depth");
        }
        script
            .initialize_managed(&mut connection, pg2sqlite::script::FreshFiles::Both)
            .expect("initialize replica");
        connection
            .batch_execute("CREATE TABLE observed (relation TEXT, user_id INTEGER, depth INTEGER);")
            .expect("observed");
        Self { connection, depth }
    }

    /// Records each row written into `table` on `event` with the depth of
    /// the marked writes open then.
    fn observe(&mut self, table: &str, event: &str) {
        let row = if event == "DELETE" { "OLD" } else { "NEW" };
        self.connection
            .batch_execute(&format!(
                "CREATE TRIGGER {table}_{event}_observed AFTER {event} ON {table} BEGIN
                   INSERT INTO observed VALUES ('{table}', {row}.user_id, derived_depth());
                 END;"
            ))
            .expect("observer");
    }

    fn observed(&mut self) -> Vec<(String, i32, i32)> {
        observed::table
            .select((observed::relation, observed::user_id, observed::depth))
            .load(&mut self.connection)
            .expect("observed rows")
    }

    fn transaction(&mut self, writes: &str) -> QueryResult<()> {
        self.connection.transaction(|connection| connection.batch_execute(writes))
    }
}

/// The construct, name and table of every object the replica leaves to the
/// server, in report order.
fn left_to_server_with(pg: &str, options: &Pg2SqliteOptions) -> Vec<(String, String, String)> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(options)
        .expect("fixture translates")
        .warnings
        .into_iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LeftToServer { construct, name, table, .. } => {
                    Some((construct, name, table))
                }
                _ => None,
            }
        })
        .collect()
}

fn left_to_server(pg: &str) -> Vec<(String, String, String)> {
    left_to_server_with(pg, &replica_options())
}

fn owned(entries: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    entries
        .iter()
        .map(|(construct, name, table)| {
            ((*construct).to_owned(), (*name).to_owned(), (*table).to_owned())
        })
        .collect()
}

fn granted(connection: &mut crate::helpers::Replica) -> Vec<(i32, i32, String)> {
    grants::table
        .select((grants::organization_id, grants::user_id, grants::role))
        .order((grants::organization_id, grants::user_id))
        .load(connection)
        .expect("grants")
}

#[test]
fn the_founding_grant_is_derived_on_the_replica_between_the_marks() {
    let mut replica = Replica::new(FOUNDING, &replica_options());
    replica.observe("grants", "INSERT");
    replica
        .transaction(
            "INSERT INTO organizations (id, created_by_id) VALUES (1, 2);
             INSERT INTO memberships (organization_id, user_id) VALUES (1, 2);",
        )
        .expect("the founding commits");
    replica
        .transaction(
            "INSERT INTO organizations (id, created_by_id) VALUES (3, 9);
             INSERT INTO grants (organization_id, user_id, role) VALUES (3, 5, 'admin');",
        )
        .expect("a grant the client writes satisfies the check");

    assert_eq!(
        granted(&mut replica.connection),
        [(1, 2, "admin".to_owned()), (3, 5, "admin".to_owned())]
    );
    assert_eq!(replica.observed(), [("grants".to_owned(), 2, 1), ("grants".to_owned(), 5, 0)]);
    assert_eq!(replica.depth.load(Ordering::Relaxed), 0);
}

#[test]
fn the_founding_check_still_refuses_an_organization_nobody_founds() {
    let mut replica = Replica::new(FOUNDING, &replica_options());
    let error = replica
        .transaction(
            "INSERT INTO organizations (id, created_by_id) VALUES (1, 2);
             INSERT INTO memberships (organization_id, user_id) VALUES (1, 7);",
        )
        .expect_err("a member who did not create the organization gets no grant")
        .to_string();
    let refused = postgres_error(&error).unwrap_or_else(|| panic!("no refusal: {error}"));
    assert_eq!(
        (refused.sqlstate(), refused.message()),
        (pg::CHECK_VIOLATION, "organization requires an admin")
    );
}

#[test]
fn every_trigger_runs_its_writes_between_the_marks_through_its_helpers() {
    let mut replica = Replica::new(MEMBERSHIPS, &replica_options());
    replica.observe("grants", "INSERT");
    replica.observe("grants", "DELETE");
    replica
        .transaction(
            "INSERT INTO organizations (id) VALUES (1);
             INSERT INTO memberships (organization_id, user_id) VALUES (1, 7);
             INSERT INTO team_members (team_id, organization_id, user_id) VALUES (3, 1, 7);",
        )
        .expect("seed");
    assert_eq!(granted(&mut replica.connection), [(1, 7, "member".to_owned())]);

    replica
        .transaction("DELETE FROM memberships WHERE organization_id = 1 AND user_id = 7;")
        .expect("the membership goes");
    assert_eq!(granted(&mut replica.connection), Vec::new());
    let teamed: i64 =
        team_members::table.count().get_result(&mut replica.connection).expect("count");
    assert_eq!(teamed, 1, "the server cascades the team membership");
    assert_eq!(replica.observed(), [("grants".to_owned(), 7, 1), ("grants".to_owned(), 7, 1)]);
    assert_eq!(replica.depth.load(Ordering::Relaxed), 0);
}

#[test]
fn only_the_key_whose_action_writes_rows_is_left_to_the_server() {
    assert_eq!(
        left_to_server(MEMBERSHIPS),
        owned(&[(
            "FOREIGN KEY",
            "(organization_id, user_id) REFERENCES memberships",
            "team_members"
        )]),
    );
}

#[test]
fn without_the_option_the_replica_runs_the_cascade_too() {
    let mut replica = Replica::new(MEMBERSHIPS, &Pg2SqliteOptions::default());
    replica.observe("grants", "INSERT");
    replica
        .transaction(
            "INSERT INTO organizations (id) VALUES (1);
             INSERT INTO memberships (organization_id, user_id) VALUES (1, 7);
             INSERT INTO team_members (team_id, organization_id, user_id) VALUES (3, 1, 7);
             DELETE FROM memberships WHERE organization_id = 1 AND user_id = 7;",
        )
        .expect("the membership comes and goes");
    let teamed: i64 =
        team_members::table.count().get_result(&mut replica.connection).expect("count");
    assert_eq!(teamed, 0);
    assert_eq!(replica.observed(), [("grants".to_owned(), 7, 0)], "nothing is marked");
}

#[test]
fn an_instead_of_trigger_writes_the_views_own_rows_unmarked() {
    let pg = "
CREATE TABLE organizations (id INTEGER PRIMARY KEY);
CREATE TABLE grants (organization_id INTEGER NOT NULL, user_id INTEGER NOT NULL,
  role TEXT NOT NULL, PRIMARY KEY (organization_id, user_id));
CREATE VIEW members AS SELECT organization_id, user_id FROM grants;
CREATE FUNCTION insert_member() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO grants (organization_id, user_id, role) VALUES (NEW.organization_id, NEW.user_id, 'member');
  RETURN NEW;
END
$$;
CREATE TRIGGER members_insert INSTEAD OF INSERT ON members
  FOR EACH ROW EXECUTE FUNCTION insert_member();
";
    let mut replica = Replica::new(pg, &replica_options());
    replica.observe("grants", "INSERT");
    replica
        .transaction("INSERT INTO members (organization_id, user_id) VALUES (1, 7);")
        .expect("a write through the view");
    assert_eq!(replica.observed(), [("grants".to_owned(), 7, 0)]);
}

/// Whether parsing or translating `pg` for a replica fails.
fn replica_refuses(pg: &str) -> bool {
    Pg2Sqlite::default()
        .sql(pg)
        .and_then(|parsed| parsed.translate_to_sql(&replica_options()))
        .is_err()
}

#[test]
fn a_trigger_sqlite_cannot_run_is_refused() {
    let in_other_language = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE FUNCTION scripted() RETURNS TRIGGER LANGUAGE plv8 AS $$ return NEW; $$;
CREATE TRIGGER notes_scripted BEFORE INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION scripted();
";
    assert!(replica_refuses(in_other_language));
    let unparsable_plpgsql = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE FUNCTION broken() RETURNS TRIGGER LANGUAGE plpgsql AS $$ BEGIN INSERT INTO; END $$;
CREATE TRIGGER notes_broken BEFORE INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION broken();
";
    assert!(replica_refuses(unparsable_plpgsql));
    let unparsable_sql = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE FUNCTION broken_helper() RETURNS VOID LANGUAGE sql AS $$ INSERT INTO $$;
CREATE FUNCTION calls_broken() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  PERFORM broken_helper();
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_broken AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION calls_broken();
";
    assert!(replica_refuses(unparsable_sql));
    let built_at_run_time = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE FUNCTION built() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE 'DELETE FROM notes WHERE id = ' || NEW.id;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_built AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION built();
";
    assert!(replica_refuses(built_at_run_time));
    let missing_function = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TRIGGER notes_missing AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION missing_function();
";
    assert!(replica_refuses(missing_function));
}

#[test]
fn a_key_that_changes_child_rows_is_left_out_and_a_plain_key_kept() {
    let pg = "
CREATE TABLE folders (id INTEGER PRIMARY KEY);
CREATE TABLE documents (id INTEGER PRIMARY KEY,
  folder_id INTEGER REFERENCES folders (id) ON DELETE SET NULL,
  origin_id INTEGER NOT NULL CONSTRAINT documents_origin REFERENCES folders (id) ON UPDATE RESTRICT);
";
    assert_eq!(
        left_to_server(pg),
        owned(&[("FOREIGN KEY", "(folder_id) REFERENCES folders", "documents")]),
    );

    let mut connection = Replica::new(pg, &replica_options()).connection;
    diesel::insert_into(folders::table)
        .values(&vec![folders::id.eq(1), folders::id.eq(2)])
        .execute(&mut connection)
        .expect("folders");
    diesel::insert_into(documents::table)
        .values((documents::id.eq(1), documents::folder_id.eq(1), documents::origin_id.eq(2)))
        .execute(&mut connection)
        .expect("document");
    diesel::delete(folders::table.find(1)).execute(&mut connection).expect("folder delete");
    let folder: Option<i32> =
        documents::table.find(1).select(documents::folder_id).first(&mut connection).expect("doc");
    assert_eq!(folder, Some(1));

    let refused = diesel::delete(folders::table.find(2))
        .execute(&mut connection)
        .expect_err("documents_origin stays");
    assert!(refused.to_string().contains("FOREIGN KEY constraint failed"), "{refused}");
}

#[test]
fn a_column_keeps_its_other_options_when_its_key_is_left_out() {
    let pg = "
CREATE TABLE folders (id INTEGER PRIMARY KEY);
CREATE TABLE documents (id INTEGER PRIMARY KEY,
  folder_id INTEGER REFERENCES folders (id) ON DELETE CASCADE NOT NULL,
  origin_id INTEGER NOT NULL);
";
    assert_eq!(
        left_to_server(pg),
        owned(&[("FOREIGN KEY", "(folder_id) REFERENCES folders", "documents")]),
    );
    let mut connection = Replica::new(pg, &replica_options()).connection;
    let refused = diesel::insert_into(documents::table)
        .values((
            documents::id.eq(1),
            documents::folder_id.eq(None::<i32>),
            documents::origin_id.eq(1),
        ))
        .execute(&mut connection)
        .expect_err("NOT NULL stays");
    assert!(refused.to_string().contains("NOT NULL constraint failed"), "{refused}");
}

#[test]
fn an_added_key_that_cascades_is_left_out() {
    let pg = "
CREATE TABLE folders (id INTEGER PRIMARY KEY);
CREATE TABLE documents (id INTEGER PRIMARY KEY, folder_id INTEGER, origin_id INTEGER NOT NULL);
ALTER TABLE documents ADD CONSTRAINT documents_folder
  FOREIGN KEY (folder_id) REFERENCES folders (id) ON DELETE CASCADE;
";
    assert_eq!(left_to_server(pg), owned(&[("FOREIGN KEY", "documents_folder", "documents")]));
    Replica::new(pg, &replica_options());
}

/// Parents whose deferred check refuses a parent marked done with an empty
/// child, children whose key into the parent cascades the parent's state,
/// and an acting role writing both.
const CASCADED_STATES: &str = "
CREATE ROLE postgres SUPERUSER;
CREATE ROLE app NOLOGIN;
CREATE TABLE parents (id INT PRIMARY KEY, state TEXT NOT NULL, UNIQUE (id, state));
CREATE TABLE children (
    id INT PRIMARY KEY,
    parent_id INT NOT NULL,
    parent_state TEXT NOT NULL,
    item INT,
    FOREIGN KEY (parent_id, parent_state) REFERENCES parents (id, state) ON UPDATE CASCADE
);
CREATE FUNCTION check_parent() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.state = 'done' AND EXISTS (
        SELECT 1 FROM children child WHERE child.parent_id = NEW.id AND child.item IS NULL
    ) THEN
        RAISE EXCEPTION 'parent % is done with an empty child', NEW.id USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER parents_check AFTER UPDATE ON parents
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_parent();
GRANT SELECT, INSERT, UPDATE (state) ON parents TO app;
GRANT SELECT, INSERT, UPDATE (item) ON children TO app;
";

#[test]
fn a_check_beside_a_cascade_left_to_the_server_keeps_its_refusal() {
    let deployment = Deployment::new("postgres", ["app"], []).expect("a valid deployment");
    let options = replica_options()
        .with_deployment(deployment)
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit");
    assert_eq!(
        left_to_server_with(CASCADED_STATES, &options),
        owned(&[("FOREIGN KEY", "(parent_id, parent_state) REFERENCES parents", "children")]),
    );
    let mut replica = Replica::new(CASCADED_STATES, &options);
    replica
        .transaction(
            "INSERT INTO parents (id, state) VALUES (1, 'open');
             INSERT INTO children (id, parent_id, parent_state, item) VALUES (1, 1, 'open', NULL);",
        )
        .expect("seed");
    let error = replica
        .transaction("UPDATE parents SET state = 'done' WHERE id = 1;")
        .expect_err("a parent with an empty child cannot be done")
        .to_string();
    let refused = postgres_error(&error).unwrap_or_else(|| panic!("no refusal: {error}"));
    assert_eq!(
        (refused.sqlstate(), refused.message()),
        (pg::CHECK_VIOLATION, "parent 1 is done with an empty child"),
    );
}
