//! A replica leaving foreign key actions to the server names every construct it
//! keeps that reads what such an action changes.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};
use sql_traits::structs::Deployment;

/// Removing a user clears invitation acceptance, which the membership policy
/// reads through a helper.
const INVITATIONS: &str = "
CREATE ROLE postgres SUPERUSER;
CREATE ROLE app NOLOGIN;
CREATE TABLE users (id INT PRIMARY KEY);
CREATE TABLE invitations (id INT PRIMARY KEY, organization_id INT NOT NULL,
  email TEXT NOT NULL, accepted_by_id INT REFERENCES users (id) ON DELETE SET NULL);
CREATE TABLE memberships (organization_id INT NOT NULL, user_id INT NOT NULL,
  invitation_id INT REFERENCES invitations (id), PRIMARY KEY (organization_id, user_id));
CREATE FUNCTION pending_invitations() RETURNS TABLE (organization_id INT, id INT)
LANGUAGE sql STABLE AS $$
  SELECT invitation.organization_id, invitation.id FROM invitations invitation
  WHERE invitation.accepted_by_id IS NULL
$$;
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
        .with_server_derived_write_function("derived")
        .with_deployment(deployment)
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit")
}

/// Returns each kept reader, the columns and table it reads, and the foreign
/// key writer.
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
                    writer,
                    writer_name,
                    ..
                } => {
                    assert_eq!(writer, "FOREIGN KEY");
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
              through pending_invitations(), which FOREIGN KEY (accepted_by_id) REFERENCES users on \
              invitations changes on the server alone, so the replica decides on them as they \
              were before"
                .to_owned()
        ),
        "{shown:#?}",
    );
    assert_eq!(
        readers_in(report.warnings),
        owned(&[(
            "POLICY",
            "memberships_insert",
            "accepted_by_id of invitations",
            "(accepted_by_id) REFERENCES users"
        )]),
    );
}

/// A trigger reads through its function after a rename.
#[test]
fn a_trigger_whose_function_is_renamed_is_named_as_before() {
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
  FOR EACH ROW EXECUTE FUNCTION refuse_reaccepting();"
    );
    let renamed = format!("{pg}ALTER FUNCTION refuse_reaccepting() RENAME TO refuse_accepted;");
    assert_eq!(readers(&renamed), readers(&pg));
    let warnings = Pg2Sqlite::default()
        .sql(&renamed)
        .expect("fixture parses")
        .translate_with_report(&options())
        .expect("fixture translates")
        .warnings;
    assert_eq!(through(&warnings, "invitations_once"), vec!["refuse_accepted".to_owned()]);
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
    assert_eq!(readers(&pg), Vec::new());
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
CREATE TABLE users (id INT PRIMARY KEY);
CREATE TABLE invitations (id INT PRIMARY KEY, email TEXT NOT NULL,
  accepted_by_id INT REFERENCES users (id) ON DELETE SET NULL);
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
        .translate_with_report(
            &Pg2SqliteOptions::default().with_server_derived_write_function("derived"),
        )
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
            "(accepted_by_id) REFERENCES users"
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
            "(accepted_by_id) REFERENCES users"
        )]),
    );
}

#[test]
fn an_instead_of_trigger_reading_the_view_row_names_the_tables_beneath() {
    let pg = "
CREATE TABLE users (id INT PRIMARY KEY);
CREATE TABLE invitations (id INT PRIMARY KEY, email TEXT NOT NULL,
  accepted_by_id INT REFERENCES users (id) ON DELETE SET NULL);
CREATE TABLE invitation_requests (invitation_id INT NOT NULL, email TEXT NOT NULL);
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
        .translate_with_report(
            &Pg2SqliteOptions::default().with_server_derived_write_function("derived"),
        )
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
            "(accepted_by_id) REFERENCES users"
        )]),
    );
}

#[test]
fn only_readers_of_a_column_the_action_sets_are_named() {
    let constrained = INVITATIONS.replace(
        "accepted_by_id INT REFERENCES users (id) ON DELETE SET NULL);",
        "accepted_by_id INT REFERENCES users (id) ON DELETE SET NULL,
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
                "(accepted_by_id) REFERENCES users"
            ),
            (
                "TRIGGER",
                "invitations_frozen",
                "accepted_by_id of invitations",
                "(accepted_by_id) REFERENCES users"
            ),
        ]),
    );
}

#[test]
fn readers_of_rows_a_cascade_removes_are_named() {
    let pg = "
CREATE TABLE memberships (organization_id INT NOT NULL, user_id INT NOT NULL,
  PRIMARY KEY (organization_id, user_id));
CREATE TABLE team_members (team_id INT NOT NULL, organization_id INT NOT NULL,
  user_id INT NOT NULL, PRIMARY KEY (team_id, user_id),
  FOREIGN KEY (organization_id, user_id) REFERENCES memberships (organization_id, user_id)
    ON DELETE CASCADE);
";
    for options in [
        Pg2SqliteOptions::default().with_server_derived_write_function("derived"),
        Pg2SqliteOptions::default().with_foreign_keys_omitted(),
    ] {
        let warnings = Pg2Sqlite::default()
            .sql(pg)
            .expect("fixture parses")
            .translate_with_report(&options)
            .expect("fixture translates")
            .warnings;
        assert_eq!(
            readers_in(warnings),
            owned(&[(
                "PRIMARY KEY",
                "team_members_pkey",
                "team_id, user_id of team_members",
                "(organization_id, user_id) REFERENCES memberships"
            )]),
        );
    }
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
            "(accepted_by_id) REFERENCES users"
        )]),
    );
    assert_eq!(
        translate("CREATE UNIQUE INDEX ON invitations (accepted_by_id, organization_id);"),
        owned(&[(
            "UNIQUE INDEX",
            "invitations_accepted_by_id_organization_id_idx",
            "accepted_by_id of invitations",
            "(accepted_by_id) REFERENCES users"
        )]),
    );
}

#[test]
fn a_translation_bringing_the_writer_names_the_readers_already_there() {
    let reference = " REFERENCES users (id) ON DELETE SET NULL";
    let key = "ALTER TABLE invitations ADD FOREIGN KEY (accepted_by_id) REFERENCES users (id)
  ON DELETE SET NULL;";
    let initial = Pg2Sqlite::default()
        .sql(&INVITATIONS.replace(reference, ""))
        .expect("fixture parses")
        .build_schema()
        .expect("the starting schema builds");
    let warnings = Pg2Sqlite::default()
        .sql(key)
        .expect("key parses")
        .translate_with_report_and_schema(&initial, &options())
        .expect("key translates")
        .warnings;
    assert_eq!(
        readers_in(warnings),
        owned(&[(
            "POLICY",
            "memberships_insert",
            "accepted_by_id of invitations",
            "(accepted_by_id) REFERENCES users"
        )]),
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
            "(accepted_by_id) REFERENCES users"
        )]),
    );
}

#[test]
fn a_key_declared_twice_is_named_once() {
    let pg = INVITATIONS.replace(
        "accepted_by_id INT REFERENCES users (id) ON DELETE SET NULL);",
        "accepted_by_id INT REFERENCES users (id) ON DELETE SET NULL,
  FOREIGN KEY (accepted_by_id) REFERENCES users (id),
  FOREIGN KEY (accepted_by_id) REFERENCES users (id));",
    );
    assert_eq!(
        readers(&pg),
        owned(&[
            (
                "FOREIGN KEY",
                "(accepted_by_id) REFERENCES users",
                "accepted_by_id of invitations",
                "(accepted_by_id) REFERENCES users"
            ),
            (
                "POLICY",
                "memberships_insert",
                "accepted_by_id of invitations",
                "(accepted_by_id) REFERENCES users"
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
            "(accepted_by_id) REFERENCES users"
        )]),
    );
}

#[test]
fn partial_and_unnamed_keys_are_named_as_postgresql_names_them() {
    let keyed = INVITATIONS.replace(
        "accepted_by_id INT REFERENCES users (id) ON DELETE SET NULL);",
        "accepted_by_id INT REFERENCES users (id) ON DELETE SET NULL,
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
            "(accepted_by_id) REFERENCES users",
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
    assert_eq!(readers_in(warnings), Vec::new());
}

#[test]
fn a_reader_of_whole_rows_a_cascade_removes_is_told_it_reads_the_rows() {
    let pg = "
CREATE ROLE postgres SUPERUSER;
CREATE ROLE app NOLOGIN;
CREATE TABLE memberships (organization_id INT NOT NULL, user_id INT NOT NULL,
  PRIMARY KEY (organization_id, user_id));
CREATE TABLE audit_events (id INT PRIMARY KEY, table_name TEXT NOT NULL,
  organization_id INT NOT NULL, user_id INT NOT NULL,
  CONSTRAINT audit_membership FOREIGN KEY (organization_id, user_id)
    REFERENCES memberships (organization_id, user_id) ON DELETE CASCADE);
CREATE VIEW membership_events AS
  SELECT id FROM audit_events WHERE table_name = 'memberships';
ALTER VIEW membership_events OWNER TO postgres;
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
          FOREIGN KEY audit_membership on audit_events changes on the server alone, so the replica \
          decides on them as they were before"],
    );
}

fn helper_read_warnings(helpers: &str, statements: &str) -> Vec<TranslationWarning> {
    let initial = Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE users (id INT PRIMARY KEY);
CREATE TABLE invitations (id INT PRIMARY KEY, accepted_by_id INT);
CREATE TABLE inspection_requests (id INT PRIMARY KEY);
{helpers}
CREATE FUNCTION check_invitation() RETURNS BOOLEAN LANGUAGE plpgsql AS $$
BEGIN
  {statements}
  RETURN true;
END
$$;
CREATE FUNCTION pass_invitation() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN RETURN NEW; END $$;
CREATE TRIGGER invitations_checked BEFORE UPDATE ON inspection_requests
  FOR EACH ROW WHEN (check_invitation()) EXECUTE FUNCTION pass_invitation();"
        ))
        .expect("fixture parses")
        .build_schema()
        .expect("the starting schema builds");
    Pg2Sqlite::default()
        .sql(
            "ALTER TABLE invitations ADD FOREIGN KEY (accepted_by_id)
             REFERENCES users (id) ON DELETE SET NULL;",
        )
        .expect("key parses")
        .translate_with_report_and_schema(
            &initial,
            &Pg2SqliteOptions::default().with_foreign_keys_omitted(),
        )
        .expect("key translates")
        .warnings
}

#[test]
fn helper_declarations_branches_returns_and_assertions_name_their_reads() {
    for body in [
        "DECLARE accepted INT DEFAULT (SELECT accepted_by_id FROM invitations LIMIT 1);
         BEGIN RETURN; END",
        "BEGIN CASE WHEN false THEN NULL;
         ELSE PERFORM accepted_by_id FROM invitations; END CASE; RETURN; END",
        "BEGIN RETURN (SELECT accepted_by_id FROM invitations LIMIT 1); END",
        "BEGIN RETURN NEXT (SELECT accepted_by_id FROM invitations LIMIT 1); RETURN; END",
        "BEGIN RETURN QUERY SELECT accepted_by_id FROM invitations; END",
        "BEGIN ASSERT (SELECT accepted_by_id IS NULL FROM invitations LIMIT 1); RETURN; END",
    ] {
        let returns = if body.contains("RETURN NEXT") || body.contains("RETURN QUERY") {
            "SETOF INT"
        } else if body.contains("RETURN (") {
            "INT"
        } else {
            "VOID"
        };
        let warnings = helper_read_warnings(
            &format!(
                "CREATE FUNCTION inspect_invitation() RETURNS {returns}
                 LANGUAGE plpgsql AS $$ {body} $$;"
            ),
            "PERFORM inspect_invitation();",
        );
        assert_eq!(
            through(&warnings, "invitations_checked"),
            ["check_invitation", "inspect_invitation"],
            "{body}",
        );
        assert_eq!(
            readers_in(warnings),
            owned(&[(
                "TRIGGER",
                "invitations_checked",
                "accepted_by_id of invitations",
                "(accepted_by_id) REFERENCES users",
            )]),
            "{body}",
        );
    }
}

#[test]
fn dynamic_return_queries_do_not_claim_reads_of_their_sql_text() {
    let warnings = helper_read_warnings(
        "CREATE FUNCTION inspect_invitation() RETURNS SETOF INT LANGUAGE plpgsql AS $$
         BEGIN RETURN QUERY EXECUTE 'SELECT accepted_by_id FROM invitations'; END $$;",
        "PERFORM inspect_invitation();",
    );
    assert_eq!(readers_in(warnings), owned(&[]));
}

#[test]
fn a_helper_reached_through_two_callers_names_the_first_read_path_once() {
    let warnings = helper_read_warnings(
        "CREATE FUNCTION inspect_invitation() RETURNS INT LANGUAGE sql STABLE AS $$
           SELECT accepted_by_id FROM invitations LIMIT 1
         $$;
         CREATE FUNCTION first_inspection() RETURNS INT LANGUAGE sql STABLE AS $$
           SELECT inspect_invitation()
         $$;
         CREATE FUNCTION second_inspection() RETURNS INT LANGUAGE sql STABLE AS $$
           SELECT inspect_invitation()
         $$;",
        "PERFORM first_inspection(); PERFORM second_inspection();",
    );
    assert_eq!(
        through(&warnings, "invitations_checked"),
        ["check_invitation", "first_inspection", "inspect_invitation"],
    );
    assert_eq!(
        readers_in(warnings),
        owned(&[(
            "TRIGGER",
            "invitations_checked",
            "accepted_by_id of invitations",
            "(accepted_by_id) REFERENCES users",
        )]),
    );
}
