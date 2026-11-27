//! Parity for connetto-schema's last-admin guard, `DEFERRABLE INITIALLY
//! DEFERRED` constraint triggers that `PERFORM` a PL/pgSQL helper, which
//! tests a second PL/pgSQL helper and calls a `LANGUAGE sql` one.
//!
//! PostgreSQL runs `require_organization_admin` at commit, so a transaction
//! may remove the last admin and appoint another before it commits, and one
//! that commits without an admin fails and rolls back. An organization
//! removed in the same transaction has no row to lock, and the helper returns
//! before its check. The replica has to accept and refuse the same
//! transactions and keep the same rows.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::{Outcome, apply, fresh_database};

use crate::helpers::postgres as postgres_harness;

/// connetto-schema `f276257`'s tables reduced to the columns the guard reads,
/// with its helpers, bodies and triggers verbatim but for the identifier type.
const SOURCE: &str = "
CREATE SCHEMA authz_write;
CREATE TABLE organizations (id TEXT PRIMARY KEY);
CREATE TABLE grants (
  id TEXT PRIMARY KEY, organization_id TEXT NOT NULL, resource_id TEXT NOT NULL,
  grantee_id TEXT NOT NULL, grantee_kind TEXT NOT NULL, role TEXT NOT NULL,
  expires_at TIMESTAMPTZ
);
CREATE TABLE memberships (
  organization_id TEXT NOT NULL, user_id TEXT NOT NULL, state TEXT NOT NULL,
  PRIMARY KEY (organization_id, user_id)
);

CREATE FUNCTION authz_write.lock_organization(target_organization TEXT) RETURNS BOOLEAN
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  IF pg_catalog.current_setting('transaction_isolation') = 'repeatable read' THEN
    RAISE EXCEPTION 'organization checks require read committed or serializable' USING ERRCODE = 'invalid_transaction_state';
  END IF;
  PERFORM organization.id FROM public.organizations organization
    WHERE organization.id = target_organization FOR NO KEY UPDATE;
  RETURN FOUND;
END
$$;

CREATE FUNCTION authz_write.has_active_admin(target_organization TEXT) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT EXISTS (
    SELECT 1 FROM public.grants permission
    JOIN public.memberships membership
      ON membership.organization_id = permission.organization_id AND membership.user_id = permission.grantee_id
    WHERE permission.resource_id = target_organization AND permission.organization_id = target_organization
      AND permission.role = 'admin' AND permission.expires_at IS NULL AND permission.grantee_kind = 'user'
      AND membership.state = 'active'
  )
$$;

CREATE FUNCTION authz_write.require_organization_admin(target_organization TEXT) RETURNS VOID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  IF NOT authz_write.lock_organization(target_organization) THEN
    RETURN;
  END IF;
  IF NOT authz_write.has_active_admin(target_organization) THEN
    RAISE EXCEPTION 'organization % would lose its last admin', target_organization USING ERRCODE = 'check_violation';
  END IF;
END
$$;

CREATE FUNCTION authz_write.check_last_admin() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  PERFORM authz_write.require_organization_admin(OLD.organization_id);
  RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER grants_last_admin AFTER UPDATE OR DELETE ON grants
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
  WHEN (OLD.role = 'admin' AND OLD.expires_at IS NULL AND OLD.grantee_kind = 'user' AND OLD.resource_id = OLD.organization_id)
  EXECUTE FUNCTION authz_write.check_last_admin();

CREATE FUNCTION authz_write.check_membership_admin() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  PERFORM authz_write.require_organization_admin(OLD.organization_id);
  RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER memberships_last_admin AFTER UPDATE OR DELETE ON memberships
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN (OLD.state = 'active')
  EXECUTE FUNCTION authz_write.check_membership_admin();

GRANT USAGE ON SCHEMA authz_write TO authz_writer;
GRANT SELECT ON organizations, grants, memberships TO authz_writer;
GRANT UPDATE (id) ON organizations TO authz_writer;
ALTER FUNCTION authz_write.lock_organization(TEXT) OWNER TO authz_writer;
ALTER FUNCTION authz_write.has_active_admin(TEXT) OWNER TO authz_writer;
ALTER FUNCTION authz_write.require_organization_admin(TEXT) OWNER TO authz_writer;
ALTER FUNCTION authz_write.check_last_admin() OWNER TO authz_writer;
ALTER FUNCTION authz_write.check_membership_admin() OWNER TO authz_writer;
";

/// The role every definer function runs as, created once per PostgreSQL
/// cluster.
const OWNER: &str = "CREATE ROLE authz_writer NOLOGIN";

/// Organization `o1` with two active admins, `ada` and `bob`, and a reader,
/// `cy`.
const SEED: &str = "
INSERT INTO organizations VALUES ('o1');
INSERT INTO memberships VALUES ('o1', 'ada', 'active'), ('o1', 'bob', 'active'), ('o1', 'cy', 'active');
INSERT INTO grants VALUES
  ('g-ada', 'o1', 'o1', 'ada', 'user', 'admin', NULL),
  ('g-bob', 'o1', 'o1', 'bob', 'user', 'admin', NULL),
  ('g-cy', 'o1', 'o1', 'cy', 'user', 'read', NULL);
";

diesel::table! {
    grants (id) {
        id -> Text,
    }
}

fn replica() -> crate::helpers::Replica {
    let script = Pg2Sqlite::default()
        .sql(&format!("{OWNER};{SOURCE}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap_or_else(|error| panic!("the schema should translate: {error}"));
    let mut connection = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    script
        .initialize_managed(&mut connection, pg2sqlite::prelude::FreshFiles::Both)
        .expect("the schema loads");
    connection
}

fn postgres() -> PgConnection {
    let mut connection = fresh_database();
    let script = format!(
        "DO $$ BEGIN {OWNER}; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$;\
         {SOURCE}"
    );
    apply(&mut connection, &script).expect("apply the schema on PostgreSQL");
    connection
}

/// Each transaction's outcome.
fn run<C: Connection + SimpleConnection>(
    connection: &mut C,
    transactions: &[&str],
) -> Vec<Outcome> {
    transactions
        .iter()
        .map(|writes| {
            Outcome::of(&connection.transaction(|connection| connection.batch_execute(writes)))
        })
        .collect()
}

/// Asserts PostgreSQL and the replica agree on each transaction and on the
/// grants left after the last, and that PostgreSQL gives `expected`.
fn assert_parity(transactions: &[&str], expected: &[Outcome]) {
    let mut postgres = postgres();
    let mut on_postgres = run(&mut postgres, transactions);
    on_postgres.push(Outcome::Rows(
        grants::table
            .select(grants::id)
            .order(grants::id)
            .load(&mut postgres)
            .expect("read the grants on PostgreSQL"),
    ));
    assert_eq!(on_postgres, expected, "PostgreSQL disagrees with the expectation");
    let mut connection = replica();
    let mut on_replica = run(&mut connection, transactions);
    on_replica.push(Outcome::Rows(
        grants::table
            .select(grants::id)
            .order(grants::id)
            .load(&mut connection)
            .expect("read the grants on SQLite"),
    ));
    assert_eq!(on_replica, on_postgres, "the replica disagrees with PostgreSQL");
}

fn grants(ids: &[&str]) -> Outcome {
    Outcome::Rows(ids.iter().map(|id| (*id).to_owned()).collect())
}

const ALL: &[&str] = &["g-ada", "g-bob", "g-cy"];

#[test]
fn revoking_one_of_two_admins_commits() {
    assert_parity(
        &[SEED, "DELETE FROM grants WHERE id = 'g-ada';"],
        &[Outcome::Accepted, Outcome::Accepted, grants(&["g-bob", "g-cy"])],
    );
}

#[test]
fn revoking_the_last_admin_fails_the_commit() {
    assert_parity(
        &[
            SEED,
            "DELETE FROM grants WHERE id = 'g-ada';",
            "UPDATE grants SET role = 'read' WHERE id = 'g-bob';",
        ],
        &[Outcome::Accepted, Outcome::Accepted, Outcome::Refused, grants(&["g-bob", "g-cy"])],
    );
}

#[test]
fn appointing_an_admin_before_commit_keeps_the_last_admin_revocable() {
    assert_parity(
        &[
            SEED,
            "DELETE FROM grants WHERE id IN ('g-ada', 'g-bob');
             UPDATE grants SET role = 'admin' WHERE id = 'g-cy';",
        ],
        &[Outcome::Accepted, Outcome::Accepted, grants(&["g-cy"])],
    );
}

#[test]
fn suspending_the_last_active_admin_fails_the_commit() {
    assert_parity(
        &[SEED, "UPDATE memberships SET state = 'suspended' WHERE user_id IN ('ada', 'bob');"],
        &[Outcome::Accepted, Outcome::Refused, grants(ALL)],
    );
}

#[test]
fn removing_the_organization_with_its_admins_commits() {
    assert_parity(
        &[
            SEED,
            "DELETE FROM grants WHERE organization_id = 'o1';
             DELETE FROM memberships WHERE organization_id = 'o1';
             DELETE FROM organizations WHERE id = 'o1';",
        ],
        &[Outcome::Accepted, Outcome::Accepted, grants(&[])],
    );
}
