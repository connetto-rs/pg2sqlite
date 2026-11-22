//! A replica leaving the writes the server derives out names every construct
//! it keeps that reads what such a write changes, since it judges those on
//! the rows as they were before the write until the server's rows arrive.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};
use sql_traits::structs::Deployment;

/// Accepting an invitation marks it accepted through a trigger on the
/// membership, and the membership insert policy admits only an invitation
/// nobody accepted yet, through a helper.
const INVITATIONS: &str = "
CREATE ROLE postgres SUPERUSER;
CREATE ROLE app NOLOGIN;
CREATE TABLE users (id INT PRIMARY KEY);
CREATE TABLE invitations (id INT PRIMARY KEY, organization_id INT NOT NULL,
  email TEXT NOT NULL, accepted_by_id INT REFERENCES users (id));
CREATE TABLE memberships (organization_id INT NOT NULL, user_id INT NOT NULL,
  invitation_id INT REFERENCES invitations (id), PRIMARY KEY (organization_id, user_id));
CREATE FUNCTION pending_invitations() RETURNS TABLE (organization_id INT, id INT)
LANGUAGE sql STABLE AS $$
  SELECT invitation.organization_id, invitation.id FROM invitations invitation
  WHERE invitation.accepted_by_id IS NULL
$$;
CREATE FUNCTION accept_invitation() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE invitations SET accepted_by_id = NEW.user_id WHERE id = NEW.invitation_id;
  RETURN NULL;
END
$$;
CREATE TRIGGER memberships_invitation_accepted AFTER INSERT ON memberships
  FOR EACH ROW WHEN (NEW.invitation_id IS NOT NULL) EXECUTE FUNCTION accept_invitation();
ALTER TABLE memberships ENABLE ROW LEVEL SECURITY;
CREATE POLICY memberships_read ON memberships FOR SELECT TO app USING (true);
CREATE POLICY memberships_insert ON memberships FOR INSERT TO app
  WITH CHECK ((organization_id, invitation_id) IN (SELECT * FROM pending_invitations()));
GRANT SELECT ON users, invitations TO app;
GRANT SELECT, INSERT ON memberships TO app;
";

fn options() -> Pg2SqliteOptions {
    let deployment = Deployment::new("postgres", ["app"], []).expect("a valid deployment");
    Pg2SqliteOptions::default()
        .with_server_derived_writes_omitted()
        .with_deployment(deployment)
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit")
}

/// The reports of `warnings` naming a kept construct that reads what a write
/// left to the server changes, as the reader's kind and name, the columns
/// and table it reads, and the writer's name.
fn readers_in(warnings: Vec<TranslationWarning>) -> Vec<(String, String, String, String)> {
    warnings
        .into_iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::ReadsLeftToServer {
                    construct,
                    name,
                    read,
                    columns,
                    writer_name,
                    ..
                } => {
                    Some((
                        construct,
                        name,
                        format!("{} of {read}", columns.join(", ")),
                        writer_name,
                    ))
                }
                _ => None,
            }
        })
        .collect()
}

fn readers(pg: &str) -> Vec<(String, String, String, String)> {
    readers_in(
        Pg2Sqlite::default()
            .sql(pg)
            .expect("fixture parses")
            .translate_with_report(&options())
            .expect("fixture translates")
            .warnings,
    )
}

fn owned(entries: &[(&str, &str, &str, &str)]) -> Vec<(String, String, String, String)> {
    entries
        .iter()
        .map(|(construct, name, read, writer)| {
            ((*construct).to_owned(), (*name).to_owned(), (*read).to_owned(), (*writer).to_owned())
        })
        .collect()
}

#[test]
fn a_policy_reading_the_column_through_a_helper_is_named() {
    let report = Pg2Sqlite::default()
        .sql(INVITATIONS)
        .expect("fixture parses")
        .translate_with_report(&options())
        .expect("fixture translates");
    let shown: Vec<String> = report
        .warnings
        .iter()
        .filter(|warning| matches!(warning, TranslationWarning::ReadsLeftToServer { .. }))
        .map(ToString::to_string)
        .collect();
    assert!(
        shown.contains(
            &"kept POLICY memberships_insert on memberships reads accepted_by_id of invitations \
              through pending_invitations(), which TRIGGER memberships_invitation_accepted on \
              memberships changes on the server alone, so the replica decides on them as they \
              were before"
                .to_owned()
        ),
        "{shown:#?}",
    );
    assert_eq!(
        readers_in(report.warnings),
        owned(&[
            (
                "FOREIGN KEY",
                "(accepted_by_id) REFERENCES users",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
            (
                "POLICY",
                "memberships_insert",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
        ]),
    );
}

/// PostgreSQL rewrites a write through a view with one table in its `FROM`
/// and plain columns onto that table, under the column each view column
/// passes through.
#[test]
fn a_write_through_an_automatically_updatable_view_changes_its_table() {
    let pg = INVITATIONS.replace(
        "UPDATE invitations SET accepted_by_id = NEW.user_id",
        "UPDATE open_invitations SET acceptor = NEW.user_id",
    ) + "CREATE VIEW open_invitations AS
  SELECT id, organization_id, accepted_by_id AS acceptor FROM invitations
  WHERE accepted_by_id IS NULL;";
    assert_eq!(
        readers(&pg),
        owned(&[
            (
                "FOREIGN KEY",
                "(accepted_by_id) REFERENCES users",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
            (
                "POLICY",
                "memberships_insert",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
        ]),
    );
}

/// A view over a view lands its write on the table beneath both, each level
/// renaming the columns it declares.
#[test]
fn a_write_through_nested_views_with_declared_names_changes_their_table() {
    let pg = INVITATIONS.replace(
        "UPDATE invitations SET accepted_by_id = NEW.user_id WHERE id = NEW.invitation_id",
        "UPDATE unaccepted SET acceptor = NEW.user_id WHERE invitation = NEW.invitation_id",
    ) + "CREATE VIEW open_invitations (invitation, organization, accepted) AS
  SELECT id, organization_id, accepted_by_id FROM invitations;
CREATE VIEW unaccepted AS
  SELECT invitation, accepted AS acceptor FROM open_invitations WHERE accepted IS NULL;";
    assert_eq!(
        readers(&pg),
        owned(&[
            (
                "FOREIGN KEY",
                "(accepted_by_id) REFERENCES users",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
            (
                "POLICY",
                "memberships_insert",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
        ]),
    );
}

/// PostgreSQL refuses an update through a view with `DISTINCT` and a
/// `TRUNCATE` of any view, so neither changes the table beneath.
#[test]
fn a_write_postgresql_refuses_on_a_view_changes_no_table() {
    for (write, view) in [
        (
            "UPDATE distinct_invitations SET accepted_by_id = NEW.user_id",
            "CREATE VIEW distinct_invitations AS
  SELECT DISTINCT id, accepted_by_id FROM invitations;",
        ),
        (
            "TRUNCATE open_invitations",
            "CREATE VIEW open_invitations AS SELECT id, accepted_by_id FROM invitations;",
        ),
    ] {
        let pg = INVITATIONS.replace(
            "UPDATE invitations SET accepted_by_id = NEW.user_id WHERE id = NEW.invitation_id",
            write,
        ) + view;
        assert_eq!(readers(&pg), Vec::new(), "{write}");
    }
}

/// An `INSTEAD OF` trigger on the view beneath takes the write, which then
/// changes only what the trigger writes, and the trigger reads the rows of
/// the table beneath its view.
#[test]
fn a_write_reaching_a_view_with_an_instead_of_trigger_changes_what_the_trigger_writes() {
    let pg = INVITATIONS.replace(
        "UPDATE invitations SET accepted_by_id = NEW.user_id",
        "UPDATE open_invitations SET acceptor = NEW.user_id",
    ) + "CREATE VIEW invitation_rows AS SELECT id, organization_id, accepted_by_id FROM invitations;
CREATE FUNCTION move_invitation() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE invitations SET organization_id = 0 WHERE id = OLD.id;
  RETURN NEW;
END
$$;
CREATE TRIGGER invitation_rows_move INSTEAD OF UPDATE ON invitation_rows
  FOR EACH ROW EXECUTE FUNCTION move_invitation();
CREATE VIEW open_invitations AS
  SELECT id, accepted_by_id AS acceptor FROM invitation_rows WHERE accepted_by_id IS NULL;";
    assert_eq!(
        readers(&pg),
        owned(&[
            (
                "POLICY",
                "memberships_insert",
                "organization_id of invitations",
                "memberships_invitation_accepted"
            ),
            (
                "TRIGGER",
                "invitation_rows_move",
                "organization_id of invitations",
                "memberships_invitation_accepted"
            ),
        ]),
    );
}

#[test]
fn without_the_option_nothing_is_named() {
    let deployment = Deployment::new("postgres", ["app"], []).expect("a valid deployment");
    let options = Pg2SqliteOptions::default()
        .with_deployment(deployment)
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit");
    let warnings = Pg2Sqlite::default()
        .sql(INVITATIONS)
        .expect("fixture parses")
        .translate_with_report(&options)
        .expect("fixture translates")
        .warnings;
    assert_eq!(readers_in(warnings), Vec::new());
}

#[test]
fn a_key_into_a_table_off_the_replica_is_not_named() {
    let pg = INVITATIONS.replace(
        "GRANT SELECT ON users, invitations TO app;",
        "GRANT SELECT ON invitations TO app;",
    );
    assert_eq!(
        readers(&pg),
        owned(&[(
            "POLICY",
            "memberships_insert",
            "accepted_by_id of invitations",
            "memberships_invitation_accepted"
        )]),
    );
}

/// The functions a report of `warnings` naming `name` reads through.
fn through(warnings: &[TranslationWarning], name: &str) -> Vec<String> {
    warnings
        .iter()
        .find_map(|warning| {
            match warning {
                TranslationWarning::ReadsLeftToServer { name: named, through, .. }
                    if named == name =>
                {
                    Some(through.clone())
                }
                _ => None,
            }
        })
        .unwrap_or_else(|| panic!("{name} in {warnings:#?}"))
}

#[test]
fn a_trigger_condition_reading_through_a_helper_is_named_without_the_trigger_function() {
    let pg = format!(
        "{INVITATIONS}
CREATE FUNCTION invitation_is_open(invitation INT) RETURNS BOOLEAN LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM invitations WHERE id = invitation AND accepted_by_id IS NULL)
$$;
CREATE FUNCTION refuse_membership() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'membership refused';
END
$$;
CREATE TRIGGER memberships_open_invitation BEFORE INSERT ON memberships
  FOR EACH ROW WHEN (NOT invitation_is_open(NEW.invitation_id))
  EXECUTE FUNCTION refuse_membership();
"
    );
    let warnings = Pg2Sqlite::default()
        .sql(&pg)
        .expect("fixture parses")
        .translate_with_report(&options())
        .expect("fixture translates")
        .warnings;
    assert_eq!(
        through(&warnings, "memberships_open_invitation"),
        vec!["invitation_is_open".to_owned()]
    );
}

#[test]
fn an_instead_of_trigger_reading_the_column_in_its_writes_is_named() {
    let pg = "
CREATE TABLE invitations (id INT PRIMARY KEY, email TEXT NOT NULL, accepted_by_id INT);
CREATE TABLE memberships (user_id INT PRIMARY KEY, invitation_id INT REFERENCES invitations (id));
CREATE FUNCTION accept_invitation() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE invitations SET accepted_by_id = NEW.user_id WHERE id = NEW.invitation_id;
  RETURN NULL;
END
$$;
CREATE TRIGGER memberships_invitation_accepted AFTER INSERT ON memberships
  FOR EACH ROW EXECUTE FUNCTION accept_invitation();
CREATE VIEW invitation_emails AS SELECT id, email FROM invitations;
CREATE FUNCTION readdress_open_invitation() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE invitations SET email = NEW.email WHERE id = OLD.id AND accepted_by_id IS NULL;
  RETURN NEW;
END
$$;
CREATE TRIGGER invitation_emails_readdress INSTEAD OF UPDATE ON invitation_emails
  FOR EACH ROW EXECUTE FUNCTION readdress_open_invitation();
";
    let warnings = Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(&Pg2SqliteOptions::default().with_server_derived_writes_omitted())
        .expect("fixture translates")
        .warnings;
    let triggers: Vec<(String, String, String, String)> =
        readers_in(warnings).into_iter().filter(|(construct, ..)| construct == "TRIGGER").collect();
    assert_eq!(
        triggers,
        owned(&[(
            "TRIGGER",
            "invitation_emails_readdress",
            "accepted_by_id of invitations",
            "memberships_invitation_accepted"
        )]),
    );
}

#[test]
fn a_trigger_reading_the_stored_row_through_the_function_label_is_named() {
    let pg = format!(
        "{INVITATIONS}
CREATE FUNCTION refuse_reaccepting() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF refuse_reaccepting.old.accepted_by_id IS NOT NULL THEN
    RAISE EXCEPTION 'invitation already accepted';
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER invitations_once BEFORE UPDATE ON invitations
  FOR EACH ROW EXECUTE FUNCTION refuse_reaccepting();
"
    );
    let triggers: Vec<(String, String, String, String)> =
        readers(&pg).into_iter().filter(|(construct, ..)| construct == "TRIGGER").collect();
    assert_eq!(
        triggers,
        owned(&[(
            "TRIGGER",
            "invitations_once",
            "accepted_by_id of invitations",
            "memberships_invitation_accepted"
        )]),
    );
}

#[test]
fn an_instead_of_trigger_reading_the_view_row_names_the_tables_beneath() {
    let pg = "
CREATE TABLE invitations (id INT PRIMARY KEY, email TEXT NOT NULL, accepted_by_id INT);
CREATE TABLE memberships (user_id INT PRIMARY KEY, invitation_id INT REFERENCES invitations (id));
CREATE TABLE invitation_requests (invitation_id INT NOT NULL, email TEXT NOT NULL);
CREATE FUNCTION accept_invitation() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE invitations SET accepted_by_id = NEW.user_id WHERE id = NEW.invitation_id;
  RETURN NULL;
END
$$;
CREATE TRIGGER memberships_invitation_accepted AFTER INSERT ON memberships
  FOR EACH ROW EXECUTE FUNCTION accept_invitation();
CREATE VIEW invitation_view AS SELECT id, email, accepted_by_id FROM invitations;
CREATE FUNCTION request_readdress() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF OLD.accepted_by_id IS NOT NULL THEN
    RAISE EXCEPTION 'invitation already accepted';
  END IF;
  INSERT INTO invitation_requests (invitation_id, email) VALUES (OLD.id, NEW.email);
  RETURN NEW;
END
$$;
CREATE TRIGGER invitation_view_readdress INSTEAD OF UPDATE ON invitation_view
  FOR EACH ROW EXECUTE FUNCTION request_readdress();
";
    let warnings = Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(&Pg2SqliteOptions::default().with_server_derived_writes_omitted())
        .expect("fixture translates")
        .warnings;
    let triggers: Vec<(String, String, String, String)> =
        readers_in(warnings).into_iter().filter(|(construct, ..)| construct == "TRIGGER").collect();
    assert_eq!(
        triggers,
        owned(&[(
            "TRIGGER",
            "invitation_view_readdress",
            "accepted_by_id of invitations",
            "memberships_invitation_accepted"
        )]),
    );
}

#[test]
fn only_readers_of_a_column_the_update_sets_are_named() {
    let constrained = INVITATIONS.replace(
        "accepted_by_id INT REFERENCES users (id));",
        "accepted_by_id INT REFERENCES users (id),
  CONSTRAINT invitations_addressed CHECK (email <> ''),
  CONSTRAINT invitations_accepted CHECK (accepted_by_id > 0));",
    );
    let pg = format!(
        "{constrained}
ALTER TABLE invitations ENABLE ROW LEVEL SECURITY;
CREATE POLICY invitations_of_organization ON invitations FOR SELECT TO app
  USING (organization_id > 0);
CREATE POLICY invitations_open ON invitations FOR SELECT TO app USING (accepted_by_id IS NULL);
CREATE UNIQUE INDEX invitations_email ON invitations (email);
CREATE UNIQUE INDEX invitations_acceptor ON invitations (accepted_by_id);
"
    );
    let named: Vec<(String, String)> =
        readers(&pg).into_iter().map(|(construct, name, _, _)| (construct, name)).collect();
    for expected in [
        ("POLICY", "memberships_insert"),
        ("POLICY", "invitations_open"),
        ("CHECK", "invitations_accepted"),
        ("UNIQUE INDEX", "invitations_acceptor"),
    ] {
        assert!(
            named.contains(&(expected.0.to_owned(), expected.1.to_owned())),
            "{expected:?} in {named:#?}"
        );
    }
    for unexpected in ["invitations_of_organization", "invitations_addressed", "invitations_email"]
    {
        assert!(!named.iter().any(|(_, name)| name == unexpected), "{unexpected} in {named:#?}");
    }
}

#[test]
fn a_trigger_reading_the_stored_row_is_named_and_one_reading_an_inserted_row_is_not() {
    let pg = format!(
        "{INVITATIONS}
CREATE FUNCTION refuse_reaccepting() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF OLD.accepted_by_id IS NOT NULL THEN
    RAISE EXCEPTION 'invitation already accepted';
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER invitations_once BEFORE UPDATE ON invitations
  FOR EACH ROW EXECUTE FUNCTION refuse_reaccepting();
CREATE FUNCTION refuse_preaccepted() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.accepted_by_id IS NOT NULL THEN
    RAISE EXCEPTION 'invitations start open';
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER invitations_open BEFORE INSERT ON invitations
  FOR EACH ROW EXECUTE FUNCTION refuse_preaccepted();
CREATE FUNCTION refuse_blank_email() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.email = '' THEN
    RAISE EXCEPTION 'invitations need an email';
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER invitations_addressed BEFORE UPDATE ON invitations
  FOR EACH ROW EXECUTE FUNCTION refuse_blank_email();
CREATE FUNCTION refuse_change() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'invitations do not change';
END
$$;
CREATE TRIGGER invitations_frozen AFTER UPDATE ON invitations
  FOR EACH ROW WHEN (OLD.* IS DISTINCT FROM NEW.*) EXECUTE FUNCTION refuse_change();
"
    );
    let warnings = Pg2Sqlite::default()
        .sql(&pg)
        .expect("fixture parses")
        .translate_with_report(&options())
        .expect("fixture translates")
        .warnings;
    assert_eq!(through(&warnings, "invitations_frozen"), Vec::<String>::new());
    let triggers: Vec<(String, String, String, String)> =
        readers_in(warnings).into_iter().filter(|(construct, ..)| construct == "TRIGGER").collect();
    assert_eq!(
        triggers,
        owned(&[
            (
                "TRIGGER",
                "invitations_once",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
            (
                "TRIGGER",
                "invitations_frozen",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
        ]),
    );
}

#[test]
fn a_left_out_trigger_assigning_the_new_row_changes_its_columns_on_the_server_alone() {
    let pg = "
CREATE TABLE items (id INT PRIMARY KEY, slug TEXT NOT NULL);
CREATE UNIQUE INDEX items_slug_key ON items (slug);
CREATE TABLE audit_events (id INT PRIMARY KEY);
CREATE FUNCTION normalise_slug() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO audit_events (id) VALUES (NEW.id);
  NEW.slug := lower(NEW.slug);
  RETURN NEW;
END
$$;
CREATE TRIGGER items_normalise BEFORE INSERT ON items
  FOR EACH ROW EXECUTE FUNCTION normalise_slug();
";
    let warnings = Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(&Pg2SqliteOptions::default().with_server_derived_writes_omitted())
        .expect("fixture translates")
        .warnings;
    let indexes: Vec<(String, String, String, String)> = readers_in(warnings)
        .into_iter()
        .filter(|(construct, ..)| construct == "UNIQUE INDEX")
        .collect();
    assert_eq!(
        indexes,
        owned(&[("UNIQUE INDEX", "items_slug_key", "slug of items", "items_normalise")]),
    );
}

#[test]
fn readers_of_rows_a_trigger_or_a_cascade_adds_or_removes_are_named() {
    let pg = "
CREATE TABLE memberships (organization_id INT NOT NULL, user_id INT NOT NULL,
  PRIMARY KEY (organization_id, user_id));
CREATE TABLE team_members (team_id INT NOT NULL, organization_id INT NOT NULL,
  user_id INT NOT NULL, PRIMARY KEY (team_id, user_id),
  FOREIGN KEY (organization_id, user_id) REFERENCES memberships (organization_id, user_id)
    ON DELETE CASCADE);
CREATE TABLE audit_events (id INT PRIMARY KEY, table_name TEXT NOT NULL);
CREATE FUNCTION record_audit_event() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO audit_events (id, table_name) VALUES (NEW.user_id, TG_TABLE_NAME);
  RETURN NULL;
END
$$;
CREATE TRIGGER memberships_audit AFTER INSERT ON memberships
  FOR EACH ROW EXECUTE FUNCTION record_audit_event();
";
    let warnings = Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(&Pg2SqliteOptions::default().with_server_derived_writes_omitted())
        .expect("fixture translates")
        .warnings;
    assert_eq!(
        readers_in(warnings),
        owned(&[
            ("PRIMARY KEY", "audit_events_pkey", "id of audit_events", "memberships_audit"),
            (
                "PRIMARY KEY",
                "team_members_pkey",
                "team_id, user_id of team_members",
                "(organization_id, user_id) REFERENCES memberships"
            ),
        ]),
    );
}

/// The readers of `pg`, a schema a trigger on `memberships` audits, under
/// the option alone.
fn audit_readers(pg: &str) -> Vec<(String, String, String, String)> {
    let pg = format!(
        "CREATE TABLE memberships (organization_id INT NOT NULL, user_id INT NOT NULL,
  PRIMARY KEY (organization_id, user_id));
CREATE TABLE audit_events (id INT PRIMARY KEY, table_name TEXT NOT NULL);
{pg}
CREATE TRIGGER memberships_audit AFTER INSERT ON memberships
  FOR EACH ROW EXECUTE FUNCTION record_audit_event();"
    );
    let warnings = Pg2Sqlite::default()
        .sql(&pg)
        .expect("fixture parses")
        .translate_with_report(&Pg2SqliteOptions::default().with_server_derived_writes_omitted())
        .expect("fixture translates")
        .warnings;
    readers_in(warnings)
}

#[test]
fn rows_a_trigger_adds_through_a_view_are_rows_of_its_table() {
    let readers = audit_readers(
        "CREATE VIEW audit_log AS SELECT id, table_name AS source FROM audit_events;
CREATE FUNCTION record_audit_event() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO audit_log (id, source) VALUES (NEW.user_id, TG_TABLE_NAME);
  RETURN NULL;
END
$$;",
    );
    assert_eq!(
        readers,
        owned(&[("PRIMARY KEY", "audit_events_pkey", "id of audit_events", "memberships_audit")]),
    );
}

/// PostgreSQL runs the `INSTEAD OF` trigger again for its own write to its
/// view, which the analysis follows once.
#[test]
fn an_instead_of_trigger_writing_its_own_view_is_followed_once() {
    let readers = audit_readers(
        "CREATE VIEW audit_entries AS SELECT id, table_name FROM audit_events;
CREATE FUNCTION file_audit_entry() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.table_name IS NULL THEN
    INSERT INTO audit_entries (id, table_name) VALUES (NEW.id, 'unknown');
  ELSE
    INSERT INTO audit_events (id, table_name) VALUES (NEW.id, NEW.table_name);
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER audit_entries_file INSTEAD OF INSERT ON audit_entries
  FOR EACH ROW EXECUTE FUNCTION file_audit_entry();
CREATE FUNCTION record_audit_event() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO audit_entries (id, table_name) VALUES (NEW.user_id, NULL);
  RETURN NULL;
END
$$;",
    );
    assert_eq!(
        readers,
        owned(&[("PRIMARY KEY", "audit_events_pkey", "id of audit_events", "memberships_audit")]),
    );
}

#[test]
fn a_translation_names_only_the_pairs_its_statements_bring() {
    let initial = Pg2Sqlite::default()
        .sql(INVITATIONS)
        .expect("fixture parses")
        .build_schema()
        .expect("the starting schema builds");
    let translate = |pg: &str| {
        readers_in(
            Pg2Sqlite::default()
                .sql(pg)
                .expect("fixture parses")
                .translate_with_report_and_schema(&initial, &options())
                .expect("fixture translates")
                .warnings,
        )
    };
    assert_eq!(translate("CREATE TABLE notes (id INT PRIMARY KEY);"), Vec::new());
    assert_eq!(
        translate("CREATE UNIQUE INDEX invitations_acceptor ON invitations (accepted_by_id);"),
        owned(&[(
            "UNIQUE INDEX",
            "invitations_acceptor",
            "accepted_by_id of invitations",
            "memberships_invitation_accepted"
        )]),
    );
    assert_eq!(
        translate("CREATE UNIQUE INDEX ON invitations (accepted_by_id, organization_id);"),
        owned(&[(
            "UNIQUE INDEX",
            "invitations_accepted_by_id_organization_id_idx",
            "accepted_by_id of invitations",
            "memberships_invitation_accepted"
        )]),
    );
}

#[test]
fn a_translation_bringing_the_writer_names_the_readers_already_there() {
    let trigger = "CREATE TRIGGER memberships_invitation_accepted AFTER INSERT ON memberships
  FOR EACH ROW WHEN (NEW.invitation_id IS NOT NULL) EXECUTE FUNCTION accept_invitation();";
    let initial = Pg2Sqlite::default()
        .sql(&INVITATIONS.replace(trigger, ""))
        .expect("fixture parses")
        .build_schema()
        .expect("the starting schema builds");
    let warnings = Pg2Sqlite::default()
        .sql(trigger)
        .expect("trigger parses")
        .translate_with_report_and_schema(&initial, &options())
        .expect("trigger translates")
        .warnings;
    assert_eq!(
        readers_in(warnings),
        owned(&[
            (
                "FOREIGN KEY",
                "(accepted_by_id) REFERENCES users",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
            (
                "POLICY",
                "memberships_insert",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
        ]),
    );
}

#[test]
fn a_trigger_reading_the_stored_row_through_a_quoted_record_is_named() {
    let pg = format!(
        "{INVITATIONS}
CREATE FUNCTION refuse_reaccepting() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF \"old\".accepted_by_id IS NOT NULL THEN
    RAISE EXCEPTION 'invitation already accepted';
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER invitations_once BEFORE UPDATE ON invitations
  FOR EACH ROW EXECUTE FUNCTION refuse_reaccepting();
"
    );
    let triggers: Vec<(String, String, String, String)> =
        readers(&pg).into_iter().filter(|(construct, ..)| construct == "TRIGGER").collect();
    assert_eq!(
        triggers,
        owned(&[(
            "TRIGGER",
            "invitations_once",
            "accepted_by_id of invitations",
            "memberships_invitation_accepted"
        )]),
    );
}

#[test]
fn a_key_declared_twice_is_named_once() {
    let pg = INVITATIONS.replace(
        "accepted_by_id INT REFERENCES users (id));",
        "accepted_by_id INT REFERENCES users (id),
  FOREIGN KEY (accepted_by_id) REFERENCES users (id));",
    );
    assert_eq!(
        readers(&pg),
        owned(&[
            (
                "FOREIGN KEY",
                "(accepted_by_id) REFERENCES users",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
            (
                "POLICY",
                "memberships_insert",
                "accepted_by_id of invitations",
                "memberships_invitation_accepted"
            ),
        ]),
    );
}

#[test]
fn a_policy_judging_the_stored_and_the_written_row_is_named_once() {
    let pg = format!(
        "{INVITATIONS}
CREATE POLICY memberships_move ON memberships FOR UPDATE TO app
  USING (invitation_id IN (SELECT id FROM invitations WHERE accepted_by_id IS NULL))
  WITH CHECK (invitation_id IN (SELECT id FROM invitations WHERE accepted_by_id IS NOT NULL));
GRANT UPDATE ON memberships TO app;
"
    );
    let moves: Vec<(String, String, String, String)> =
        readers(&pg).into_iter().filter(|(_, name, ..)| name == "memberships_move").collect();
    assert_eq!(
        moves,
        owned(&[(
            "POLICY",
            "memberships_move",
            "accepted_by_id of invitations",
            "memberships_invitation_accepted"
        )]),
    );
}

#[test]
fn partial_and_unnamed_keys_are_named_as_postgresql_names_them() {
    let keyed = INVITATIONS.replace(
        "accepted_by_id INT REFERENCES users (id));",
        "accepted_by_id INT REFERENCES users (id),
  CHECK (accepted_by_id > 0), UNIQUE (accepted_by_id, email));",
    );
    let pg = format!(
        "{keyed}
CREATE UNIQUE INDEX invitations_open_email ON invitations (email) WHERE accepted_by_id IS NULL;
"
    );
    let named = readers(&pg);
    for (construct, name) in [
        ("UNIQUE INDEX", "invitations_open_email"),
        ("CHECK", "invitations_accepted_by_id_check"),
        ("UNIQUE", "invitations_accepted_by_id_email_key"),
    ] {
        let expected = owned(&[(
            construct,
            name,
            "accepted_by_id of invitations",
            "memberships_invitation_accepted",
        )]);
        assert!(named.contains(&expected[0]), "{expected:?} in {named:#?}");
    }
}

#[test]
fn the_policies_of_a_role_exempt_from_row_security_are_not_named() {
    let warnings = Pg2Sqlite::default()
        .sql(&INVITATIONS.replace("CREATE ROLE app NOLOGIN;", "CREATE ROLE app NOLOGIN BYPASSRLS;"))
        .expect("fixture parses")
        .translate_with_report(&options())
        .expect("fixture translates")
        .warnings;
    assert_eq!(
        readers_in(warnings),
        owned(&[(
            "FOREIGN KEY",
            "(accepted_by_id) REFERENCES users",
            "accepted_by_id of invitations",
            "memberships_invitation_accepted"
        )]),
    );
}

#[test]
fn a_reader_of_whole_rows_a_trigger_adds_is_told_it_reads_the_rows() {
    let pg = "
CREATE ROLE postgres SUPERUSER;
CREATE ROLE app NOLOGIN;
CREATE TABLE memberships (organization_id INT NOT NULL, user_id INT NOT NULL,
  PRIMARY KEY (organization_id, user_id));
CREATE TABLE audit_events (id INT PRIMARY KEY, table_name TEXT NOT NULL);
CREATE VIEW membership_events AS
  SELECT id FROM audit_events WHERE table_name = 'memberships';
ALTER VIEW membership_events OWNER TO postgres;
CREATE FUNCTION record_audit_event() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO audit_events (id, table_name) VALUES (NEW.user_id, TG_TABLE_NAME);
  RETURN NULL;
END
$$;
CREATE TRIGGER memberships_audit AFTER INSERT ON memberships
  FOR EACH ROW EXECUTE FUNCTION record_audit_event();
ALTER TABLE memberships ENABLE ROW LEVEL SECURITY;
CREATE POLICY memberships_audited ON memberships FOR SELECT TO app
  USING (EXISTS (SELECT 1 FROM membership_events WHERE id = user_id));
GRANT SELECT ON audit_events, membership_events TO app;
GRANT SELECT, INSERT ON memberships TO app;
";
    let shown: Vec<String> = Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(&options())
        .expect("fixture translates")
        .warnings
        .iter()
        .filter(|warning| {
            matches!(warning, TranslationWarning::ReadsLeftToServer { construct, .. }
                if construct == "POLICY")
        })
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        shown,
        ["kept POLICY memberships_audited on memberships reads rows of audit_events, which \
          TRIGGER memberships_audit on memberships changes on the server alone, so the replica \
          decides on them as they were before"],
    );
}
