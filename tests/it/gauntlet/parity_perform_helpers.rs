//! Parity for `PERFORM` of a PL/pgSQL helper in a trigger body, which the
//! replica runs as the helper's body written where the call stood.
//!
//! Each scenario runs the same writes on both engines and compares whether
//! each write is accepted and what the tables hold after.

use diesel::{
    pg::PgConnection, prelude::*, result::Error as DieselError, sqlite::SqliteConnection,
};
use pg2sqlite::prelude::{
    Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, SqlState, UuidRepresentation,
    postgres_error,
};
use postgres_harness::{Outcome, apply, fresh_database, set_session_user};
use rosetta_uuid::Uuid;
use sqlstate::pg;

use crate::helpers::{
    deployment::{UTC_REQUEST_ROLE, caller, deployment},
    postgres as postgres_harness,
};

mod schema {
    diesel::table! {
        organizations (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        admins (organization, member) {
            organization -> Integer,
            member -> Text,
        }
    }
    diesel::table! {
        removals (member) {
            member -> Text,
        }
    }
    diesel::table! {
        clubs (id) {
            id -> Text,
            created_by -> Text,
            founded -> Bool,
        }
    }
    diesel::table! {
        clubs_rls (id) {
            id -> Text,
            created_by -> Text,
            founded -> Bool,
        }
    }
    diesel::table! {
        club_members (club, member) {
            club -> Text,
            member -> Text,
        }
    }
    diesel::table! {
        club_members_rls (club, member) {
            club -> Text,
            member -> Text,
        }
    }
    diesel::table! {
        club_log (club, member) {
            club -> Text,
            member -> Text,
        }
    }
    diesel::table! {
        club_log_rls (club, member) {
            club -> Text,
            member -> Text,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        parents (id) {
            id -> Uuid,
            published -> Bool,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        children (id) {
            id -> Uuid,
            parent_id -> Uuid,
        }
    }
    diesel::table! {
        rosters (club) {
            club -> Text,
        }
    }
    diesel::table! {
        roster_members (club, member) {
            club -> Text,
            member -> Text,
        }
    }
}

/// A last-admin check factored as connetto-schema factors it: a `VOID`
/// helper that locks the organization through a `BOOLEAN` helper, returns
/// early when there is none, and raises when the last admin would go.
const LAST_ADMIN: &str = "
CREATE TABLE organizations (id INTEGER PRIMARY KEY);
CREATE TABLE admins (organization INTEGER NOT NULL, member TEXT NOT NULL,
  PRIMARY KEY (organization, member));
CREATE TABLE removals (member TEXT PRIMARY KEY);
CREATE SCHEMA checks;
CREATE FUNCTION checks.lock_organization(target INTEGER) RETURNS BOOLEAN LANGUAGE plpgsql AS $$
BEGIN
  PERFORM 1 FROM public.organizations organization WHERE organization.id = target FOR UPDATE;
  RETURN FOUND;
END
$$;
CREATE FUNCTION checks.require_admin(target INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
  IF NOT checks.lock_organization(target) THEN
    RETURN;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM public.admins admin WHERE admin.organization = target) THEN
    RAISE EXCEPTION 'organization % would lose its last admin', target;
  END IF;
END
$$;
CREATE FUNCTION checks.keep_last_admin() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM checks.require_admin(OLD.organization);
  INSERT INTO public.removals (member) VALUES (OLD.member);
  RETURN NULL;
END
$$;
CREATE TRIGGER admins_keep_last AFTER DELETE ON admins
  FOR EACH ROW EXECUTE FUNCTION checks.keep_last_admin();
";

macro_rules! last_admin_runner {
    ($name:ident, $connection:ty) => {
        struct $name {
            connection: $connection,
        }

        impl $name {
            fn seed(&mut self) {
                use self::schema::{admins, organizations};
                diesel::insert_into(organizations::table)
                    .values(organizations::id.eq(1))
                    .execute(&mut self.connection)
                    .expect("seed organization 1");
                // Organization 2 has admins and no row to lock.
                diesel::insert_into(admins::table)
                    .values(&vec![
                        (admins::organization.eq(1), admins::member.eq("ada")),
                        (admins::organization.eq(1), admins::member.eq("bob")),
                        (admins::organization.eq(2), admins::member.eq("cy")),
                    ])
                    .execute(&mut self.connection)
                    .expect("seed admins");
            }

            fn remove(&mut self, member: &str) -> QueryResult<usize> {
                use self::schema::admins;
                diesel::delete(admins::table.filter(admins::member.eq(member)))
                    .execute(&mut self.connection)
            }

            fn state(&mut self) -> (Vec<String>, Vec<String>) {
                use self::schema::{admins, removals};
                let admins = admins::table
                    .select(admins::member)
                    .order(admins::member)
                    .load(&mut self.connection)
                    .expect("read admins");
                let removals = removals::table
                    .select(removals::member)
                    .order(removals::member)
                    .load(&mut self.connection)
                    .expect("read removals");
                (admins, removals)
            }
        }
    };
}

last_admin_runner!(LastAdminPostgres, PgConnection);
last_admin_runner!(LastAdminSqlite, SqliteConnection);

fn translated(source: &str, options: &Pg2SqliteOptions) -> pg2sqlite::script::Script<String> {
    Pg2Sqlite::default()
        .sql(source)
        .and_then(|parsed| parsed.translate_to_sql(options))
        .unwrap_or_else(|error| panic!("the schema should translate: {error}"))
}

fn replica(statements: &pg2sqlite::script::Script<String>) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    statements.register_functions(&mut connection).expect("register helper functions");
    for statement in statements.iter() {
        diesel::connection::SimpleConnection::batch_execute(&mut connection, statement)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

/// Removing an admin runs the helper's check, the last admin of a locked
/// organization stays, an organization with no row ends the helper early
/// and the trigger body goes on after the call, on both engines.
#[test]
fn a_performed_helper_checks_and_returns_as_postgres_does() {
    let mut postgres = LastAdminPostgres { connection: fresh_database() };
    apply(&mut postgres.connection, LAST_ADMIN).expect("apply the schema on PostgreSQL");
    let mut sqlite = LastAdminSqlite {
        connection: replica(&translated(LAST_ADMIN, &Pg2SqliteOptions::default())),
    };
    postgres.seed();
    sqlite.seed();

    for member in ["ada", "bob", "cy"] {
        let expected = Outcome::of(&postgres.remove(member));
        let actual = sqlite.remove(member);
        assert_eq!(Outcome::of(&actual), expected, "removing {member}");
        if member == "bob" {
            let error = actual.expect_err("bob is the last admin of organization 1");
            assert!(
                error.to_string().contains("organization 1 would lose its last admin"),
                "{error}"
            );
        }
    }
    assert_eq!(sqlite.state(), postgres.state());
    assert_eq!(postgres.state(), (vec!["bob".to_owned()], vec!["ada".to_owned(), "cy".to_owned()]));
}

/// A parent publishes only once it has a child, the check factored into a
/// helper taking the parent's `UUID`, which the replica stores as its 16 bytes.
const PUBLISHED_PARENTS: &str = "
CREATE TABLE parents (id UUID PRIMARY KEY, published BOOLEAN NOT NULL DEFAULT FALSE);
CREATE TABLE children (id UUID PRIMARY KEY, parent_id UUID NOT NULL REFERENCES parents (id));
CREATE FUNCTION require_children(checked_parent_id UUID) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM children child WHERE child.parent_id = checked_parent_id) THEN
    RAISE EXCEPTION 'parent % has no children', checked_parent_id;
  END IF;
END
$$;
CREATE FUNCTION guard_parent() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.published AND NOT OLD.published THEN
    PERFORM require_children(NEW.id);
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER parents_guard BEFORE UPDATE ON parents
  FOR EACH ROW EXECUTE FUNCTION guard_parent();
";

fn uuid(text: &str) -> Uuid {
    text.parse().expect("a uuid")
}

macro_rules! parents_runner {
    ($name:ident, $connection:ty) => {
        struct $name {
            connection: $connection,
        }

        impl $name {
            fn seed(&mut self, parents: &[Uuid], child_of: Uuid) {
                use self::schema::{children, parents};
                for id in parents {
                    diesel::insert_into(parents::table)
                        .values(parents::id.eq(*id))
                        .execute(&mut self.connection)
                        .expect("seed a parent");
                }
                diesel::insert_into(children::table)
                    .values((
                        children::id.eq(uuid("00000000-0000-7000-8000-0000000000c1")),
                        children::parent_id.eq(child_of),
                    ))
                    .execute(&mut self.connection)
                    .expect("seed a child");
            }

            fn publish(&mut self, id: Uuid) -> QueryResult<usize> {
                use self::schema::parents;
                diesel::update(parents::table.filter(parents::id.eq(id)))
                    .set(parents::published.eq(true))
                    .execute(&mut self.connection)
            }

            fn published(&mut self) -> Vec<Uuid> {
                use self::schema::parents;
                parents::table
                    .filter(parents::published)
                    .select(parents::id)
                    .load(&mut self.connection)
                    .expect("read the published parents")
            }
        }
    };
}

parents_runner!(ParentsPostgres, PgConnection);
parents_runner!(ParentsSqlite, SqliteConnection);

/// The helper compares the `UUID` its caller passes as the same uuid, so a
/// parent with a child publishes and one without is refused, and the refusal
/// prints the parent's uuid as PostgreSQL prints it, on both engines.
#[test]
fn a_uuid_argument_reaches_the_helper_as_postgres_passes_it() {
    let with_child = uuid("00000000-0000-7000-8000-000000000001");
    let childless = uuid("00000000-0000-7000-8000-000000000002");
    let mut postgres = ParentsPostgres { connection: fresh_database() };
    apply(&mut postgres.connection, PUBLISHED_PARENTS).expect("apply the schema on PostgreSQL");
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
    let mut sqlite =
        ParentsSqlite { connection: replica(&translated(PUBLISHED_PARENTS, &options)) };
    postgres.seed(&[with_child, childless], with_child);
    sqlite.seed(&[with_child, childless], with_child);

    for id in [with_child, childless] {
        let expected = postgres.publish(id).map_err(postgres_refusal);
        assert_eq!(sqlite.publish(id).map_err(replica_refusal), expected, "publishing {id}");
    }
    assert_eq!(sqlite.published(), postgres.published());
    assert_eq!(postgres.published(), vec![with_child]);
}

/// A club keeps a member, the check factored into a PL/pgSQL helper taking
/// the club as `TEXT` and asking a SQL helper whether it has one.
const ROSTERS: &str = "
CREATE TABLE rosters (club TEXT PRIMARY KEY);
CREATE TABLE roster_members (club TEXT NOT NULL REFERENCES rosters (club), member TEXT NOT NULL,
  PRIMARY KEY (club, member));
CREATE FUNCTION has_member(target TEXT) RETURNS BOOLEAN LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM public.roster_members roster WHERE roster.club = target)
$$;
CREATE FUNCTION require_member(target TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
  IF NOT has_member(target) THEN
    RAISE EXCEPTION 'club % would lose its last member', target;
  END IF;
END
$$;
CREATE FUNCTION keep_a_member() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM require_member(OLD.club);
  RETURN NULL;
END
$$;
CREATE TRIGGER roster_members_keep AFTER DELETE ON roster_members
  FOR EACH ROW EXECUTE FUNCTION keep_a_member();
";

macro_rules! seed_rosters {
    ($connection:expr) => {{
        use self::schema::{roster_members, rosters};
        diesel::insert_into(rosters::table)
            .values(rosters::club.eq("chess"))
            .execute($connection)
            .expect("seed a club");
        diesel::insert_into(roster_members::table)
            .values(&vec![
                (roster_members::club.eq("chess"), roster_members::member.eq("ada")),
                (roster_members::club.eq("chess"), roster_members::member.eq("bob")),
            ])
            .execute($connection)
            .expect("seed the members");
    }};
}

macro_rules! remove_member {
    ($connection:expr, $member:expr, $refusal:expr) => {{
        use self::schema::roster_members;
        diesel::delete(roster_members::table.filter(roster_members::member.eq($member)))
            .execute($connection)
            .map_err($refusal)
    }};
}

/// The helper's `TEXT` parameter reaches the SQL helper's cast with its
/// declared type, so the trigger translates and keeps the last member, on
/// both engines.
#[test]
fn a_text_parameter_reaches_a_sql_helper_as_its_declared_type() {
    let mut postgres = fresh_database();
    apply(&mut postgres, ROSTERS).expect("apply the schema on PostgreSQL");
    let mut sqlite = replica(&translated(ROSTERS, &Pg2SqliteOptions::default()));
    seed_rosters!(&mut postgres);
    seed_rosters!(&mut sqlite);

    let expected =
        ["ada", "bob"].map(|member| remove_member!(&mut postgres, member, postgres_refusal));
    let actual = ["ada", "bob"].map(|member| remove_member!(&mut sqlite, member, replica_refusal));
    assert_eq!(actual, expected);
    assert_eq!(
        expected[1],
        Err((pg::RAISE_EXCEPTION, "club chess would lose its last member".to_owned())),
        "PostgreSQL keeps bob"
    );
}

/// The SQLSTATE and message PostgreSQL refused a write with.
fn postgres_refusal(error: DieselError) -> (SqlState, String) {
    let DieselError::DatabaseError(_, information) = error else {
        panic!("PostgreSQL failed with {error} instead of refusing the write");
    };
    let sqlstate = information.sqlstate().expect("PostgreSQL reports a SQLSTATE");
    (sqlstate, information.message().to_owned())
}

/// The SQLSTATE and message the replica's refusal of a write reads back as.
fn replica_refusal(error: DieselError) -> (SqlState, String) {
    let DieselError::DatabaseError(_, information) = error else {
        panic!("SQLite failed with {error} instead of refusing the write");
    };
    let refusal = postgres_error(information.message())
        .unwrap_or_else(|| panic!("{:?} is no replica refusal", information.message()));
    (refusal.sqlstate(), refusal.message().to_owned())
}

/// The owner every definer function runs as, past row-level security.
const OWNER: &str = "CREATE ROLE helper_owner NOLOGIN BYPASSRLS;";

/// Clubs the caller sees only when she created them, members the caller adds
/// herself, and a log only definer bodies write.
const CLUBS: &str = "
CREATE TABLE clubs (id TEXT PRIMARY KEY, created_by TEXT NOT NULL, founded BOOLEAN NOT NULL);
CREATE TABLE club_members (club TEXT NOT NULL, member TEXT NOT NULL, PRIMARY KEY (club, member));
CREATE TABLE club_log (club TEXT NOT NULL, member TEXT NOT NULL, PRIMARY KEY (club, member));
CREATE SCHEMA club_write;
CREATE FUNCTION club_write.require_unfounded(target TEXT) RETURNS VOID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM public.clubs club WHERE club.id = target AND club.founded) THEN
    RAISE EXCEPTION 'club % is already founded', target USING ERRCODE = 'check_violation';
  END IF;
END
$$;
CREATE FUNCTION club_write.log_member(target TEXT, joined TEXT) RETURNS VOID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  INSERT INTO public.club_log (club, member) VALUES (target, joined);
END
$$;
CREATE FUNCTION club_write.check_founder() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  PERFORM club_write.require_unfounded(NEW.club);
  RETURN NEW;
END
$$;
CREATE FUNCTION club_write.note_member() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
  PERFORM club_write.log_member(NEW.club, NEW.member);
  RETURN NULL;
END
$$;
CREATE TRIGGER club_members_founder BEFORE INSERT ON club_members
  FOR EACH ROW EXECUTE FUNCTION club_write.check_founder();
CREATE TRIGGER club_members_log AFTER INSERT ON club_members
  FOR EACH ROW EXECUTE FUNCTION club_write.note_member();
ALTER FUNCTION club_write.require_unfounded(TEXT) OWNER TO helper_owner;
ALTER FUNCTION club_write.log_member(TEXT, TEXT) OWNER TO helper_owner;
ALTER FUNCTION club_write.check_founder() OWNER TO helper_owner;
ALTER TABLE clubs ENABLE ROW LEVEL SECURITY;
CREATE POLICY clubs_read ON clubs FOR SELECT
  USING (created_by = current_setting('app.user_id', true));
ALTER TABLE club_members ENABLE ROW LEVEL SECURITY;
CREATE POLICY club_members_read ON club_members FOR SELECT
  USING (member = current_setting('app.user_id', true));
CREATE POLICY club_members_join ON club_members FOR INSERT
  WITH CHECK (member = current_setting('app.user_id', true));
ALTER TABLE club_log ENABLE ROW LEVEL SECURITY;
CREATE POLICY club_log_read ON club_log FOR SELECT USING (true);
CREATE POLICY club_log_write ON club_log FOR INSERT WITH CHECK (false);
GRANT USAGE ON SCHEMA club_write TO app, helper_owner;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA club_write TO app, helper_owner;
";

diesel::define_sql_function! {
    /// The signed-in user, `current_setting('app.user_id', true)` on PostgreSQL.
    fn perform_app_user() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

diesel::define_sql_function! {
    /// Whether the connection writes past the row-level-security guards.
    fn perform_write_exempt() -> diesel::sql_types::Bool;
}

fn club_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(deployment([], [caller("app.user_id", "text")]))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "perform_app_user"))
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("perform_write_exempt")
}

/// The clubs `nina` cannot see: `secret`, founded, and `open`, not yet.
const SEEDED_CLUBS: [(&str, &str, bool); 2] = [("secret", "omar", true), ("open", "omar", false)];

fn postgres_clubs() -> PgConnection {
    let mut connection = fresh_database();
    let owner = OWNER.trim_end_matches(';');
    let script = format!(
        "DO $$ BEGIN {owner}; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$;\
         {CLUBS}\
         GRANT SELECT, INSERT ON ALL TABLES IN SCHEMA public TO app, helper_owner;"
    );
    apply(&mut connection, &script).expect("apply the schema on PostgreSQL");
    for (id, created_by, founded) in SEEDED_CLUBS {
        diesel::insert_into(schema::clubs::table)
            .values((
                schema::clubs::id.eq(id),
                schema::clubs::created_by.eq(created_by),
                schema::clubs::founded.eq(founded),
            ))
            .execute(&mut connection)
            .expect("seed a club");
    }
    set_session_user(&mut connection, "nina");
    // Taking on another role is a session command the query DSL has no form
    // for.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("act as app");
    connection
}

fn sqlite_clubs() -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    perform_app_user_utils::register_nondeterministic_impl(&mut connection, || Some("nina"))
        .expect("register the signed-in user");
    perform_write_exempt_utils::register_nondeterministic_impl(&mut connection, || true)
        .expect("exempt the seed");
    let script = translated(&[UTC_REQUEST_ROLE, OWNER, CLUBS].concat(), &club_options());
    script.register_functions(&mut connection).expect("register helper functions");
    for statement in script.iter() {
        diesel::connection::SimpleConnection::batch_execute(&mut connection, statement)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    for (id, created_by, founded) in SEEDED_CLUBS {
        diesel::insert_into(schema::clubs_rls::table)
            .values((
                schema::clubs_rls::id.eq(id),
                schema::clubs_rls::created_by.eq(created_by),
                schema::clubs_rls::founded.eq(founded),
            ))
            .execute(&mut connection)
            .expect("seed a club");
    }
    perform_write_exempt_utils::register_nondeterministic_impl(&mut connection, || false)
        .expect("end the exemption");
    connection
}

fn join<C>(connection: &mut C, club: &'static str) -> Outcome
where
    C: Connection,
    diesel::dsl::Values<
        diesel::dsl::insert_into<schema::club_members::table>,
        (
            diesel::dsl::Eq<schema::club_members::club, &'static str>,
            diesel::dsl::Eq<schema::club_members::member, &'static str>,
        ),
    >: diesel::query_dsl::methods::ExecuteDsl<C>,
{
    Outcome::of(
        &diesel::insert_into(schema::club_members::table)
            .values((schema::club_members::club.eq(club), schema::club_members::member.eq("nina")))
            .execute(connection),
    )
}

fn postgres_log(connection: &mut PgConnection) -> Vec<(String, String)> {
    use schema::club_log::dsl::*;
    club_log.select((club, member)).order(club).load(connection).expect("read the log")
}

fn sqlite_log(connection: &mut SqliteConnection) -> Vec<(String, String)> {
    use schema::club_log_rls::dsl::*;
    club_log_rls.select((club, member)).order(club).load(connection).expect("read the log")
}

/// The founding check reads a club the caller cannot see as the helper's
/// owner, and the log helper writes a table no caller may write as its own
/// owner, from an invoker trigger, on both engines.
#[test]
fn a_definer_helper_reads_and_writes_as_its_owner() {
    let mut postgres = postgres_clubs();
    let mut sqlite = sqlite_clubs();
    for club in ["secret", "open"] {
        assert_eq!(join(&mut sqlite, club), join(&mut postgres, club), "joining {club}");
    }
    assert_eq!(postgres_log(&mut postgres), vec![("open".to_owned(), "nina".to_owned())]);
    assert_eq!(sqlite_log(&mut sqlite), postgres_log(&mut postgres));
}
