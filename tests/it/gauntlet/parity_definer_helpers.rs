//! connetto's R100 hierarchy, whose every policy compares a column to the set a
//! `SECURITY DEFINER` helper returns, shows each caller the same rows and
//! accepts the same writes on both engines.
//!
//! The steps are rows R, W, G and P of R100's behaviour matrix, measured on
//! PostgreSQL, and each read and each refusal is also checked against what the
//! matrix recorded. The matrix's G17 and G18 are left out, since they exercise
//! the last-admin triggers the translated document does not carry, and its
//! creator trigger is stood in for by seeding the creator's admin grant. Every
//! write is followed by a comparison of what both engines store, since SQLite
//! does not count the rows an `INSTEAD OF` trigger writes.

use std::sync::{Arc, Mutex};

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::Pg2Sqlite;
use postgres_harness::Outcome;

use crate::{
    helpers::postgres as postgres_harness,
    test_definer_helper_inlining::{R100, options},
};

/// The reader role the matrix acts as, holding DML on the tables and the right
/// to call the helpers.
const PG_READER_GRANTS: &str = "
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO app;
GRANT USAGE ON SCHEMA authz TO app;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO app;
";

mod schema {
    /// Declares a table under its own name, which PostgreSQL stores and SQLite
    /// guards with a view, and under its backing table's name, which SQLite
    /// stores.
    macro_rules! guarded {
        ($table:ident, $backing:ident, ($($key:ident),+) { $($column:ident -> $type:ty,)+ }) => {
            diesel::table! { $table ($($key),+) { $($column -> $type,)+ } }
            diesel::table! { $backing ($($key),+) { $($column -> $type,)+ } }
        };
    }

    guarded!(organizations, organizations_rls, (id) { id -> Text, created_by -> Text, });
    guarded!(teams, teams_rls, (id) { id -> Text, organization_id -> Text, });
    guarded!(projects, projects_rls, (id) { id -> Text, team_id -> Text, });
    guarded!(orders, orders_rls, (id) {
        id -> Text,
        project_id -> Text,
        title -> Nullable<Text>,
    });
    guarded!(organization_grants, organization_grants_rls, (organization_id, principal_id) {
        organization_id -> Text,
        principal_id -> Text,
        role -> Text,
    });
    guarded!(team_grants, team_grants_rls, (team_id, principal_id) {
        team_id -> Text,
        principal_id -> Text,
        role -> Text,
    });
    guarded!(project_grants, project_grants_rls, (project_id, principal_id) {
        project_id -> Text,
        principal_id -> Text,
        role -> Text,
    });
}

use schema::{orders, organization_grants, project_grants, projects, team_grants};

#[declare_sql_function]
extern "SQL" {
    /// The replica's stand-in for `current_setting('app.user_id', true)`.
    fn app_user() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

/// One step of the matrix.
#[derive(Debug)]
enum Step {
    /// Acts as this caller, or as no caller.
    Caller(Option<&'static str>),
    /// The orders the caller reads.
    ReadOrders,
    /// The team grants the caller reads, as `team:principal`.
    ReadTeamGrants,
    /// The team admins the caller reads.
    ReadTeamAdmins,
    /// The organization admins the caller reads.
    ReadOrganizationAdmins,
    /// Sets the title of every order the caller may update.
    RetitleOrders(&'static str),
    InsertOrder {
        id: &'static str,
        project: &'static str,
    },
    MoveOrder {
        id: &'static str,
        project: &'static str,
    },
    MoveProject {
        id: &'static str,
        team: &'static str,
    },
    InsertTeamGrant {
        team: &'static str,
        principal: &'static str,
        role: &'static str,
    },
    DeleteTeamGrants {
        principal: &'static str,
    },
    DeleteProjectGrants {
        principal: &'static str,
    },
    InsertOrganizationGrant {
        principal: &'static str,
        role: &'static str,
    },
    PromoteOrganizationGrant {
        principal: &'static str,
    },
    DeleteOrganizationGrants {
        principal: &'static str,
    },
    /// Every stored row of every table, read past the policies.
    Stored,
}

fn rows(rows: &[&str]) -> Outcome {
    Outcome::Rows(rows.iter().map(ToString::to_string).collect())
}

/// The steps, each with what the matrix recorded for it, when it recorded a
/// read or a refusal.
fn scenario() -> Vec<(Step, Option<Outcome>)> {
    use Step::{
        Caller, DeleteOrganizationGrants, DeleteProjectGrants, DeleteTeamGrants, InsertOrder,
        InsertOrganizationGrant, InsertTeamGrant, MoveOrder, MoveProject, PromoteOrganizationGrant,
        ReadOrders, ReadOrganizationAdmins, ReadTeamAdmins, ReadTeamGrants, RetitleOrders, Stored,
    };
    let accepted = || Some(Outcome::Accepted);
    let refused = || Some(Outcome::Refused);
    vec![
        // R1 to R9.
        (Caller(Some("alice")), None),
        (ReadOrders, Some(rows(&["oP", "oQ"]))),
        (Caller(Some("olga")), None),
        (ReadOrders, Some(rows(&["oP", "oQ"]))),
        (Caller(Some("tom")), None),
        (ReadOrders, Some(rows(&["oP"]))),
        (Caller(Some("bob")), None),
        (ReadOrders, Some(rows(&["oP"]))),
        (Caller(Some("uma")), None),
        (ReadOrders, Some(rows(&["oQ"]))),
        (Caller(Some("quinn")), None),
        (ReadOrders, Some(rows(&["oQ"]))),
        (Caller(Some("inst-T")), None),
        (ReadOrders, Some(rows(&["oP"]))),
        (Caller(Some("zed")), None),
        (ReadOrders, Some(rows(&[]))),
        (Caller(None), None),
        (ReadOrders, Some(rows(&[]))),
        // W1 to W8, each caller retitling what it may write.
        (Caller(Some("alice")), None),
        (RetitleOrders("W1"), accepted()),
        (Stored, None),
        (Caller(Some("owen")), None),
        (RetitleOrders("W2"), accepted()),
        (Stored, None),
        (Caller(Some("olga")), None),
        (RetitleOrders("W3"), accepted()),
        (Stored, None),
        (Caller(Some("tom")), None),
        (RetitleOrders("W4"), accepted()),
        (Stored, None),
        (Caller(Some("bob")), None),
        (RetitleOrders("W5"), accepted()),
        (Stored, None),
        (Caller(Some("uma")), None),
        (RetitleOrders("W6"), accepted()),
        (Stored, None),
        (Caller(Some("quinn")), None),
        (RetitleOrders("W7"), accepted()),
        (Stored, None),
        (Caller(Some("inst-T")), None),
        (RetitleOrders("W8"), accepted()),
        (Stored, None),
        // W9 to W11.
        (Caller(Some("bob")), None),
        (InsertOrder { id: "oP2", project: "P" }, accepted()),
        (InsertOrder { id: "oQ2", project: "Q" }, refused()),
        (MoveOrder { id: "oP", project: "Q" }, refused()),
        (Stored, None),
        // G1 to G4.
        (Caller(Some("alice")), None),
        (ReadTeamGrants, Some(rows(&["T:bob", "T:inst-T", "T:tom", "U:uma"]))),
        (Caller(Some("tom")), None),
        (ReadTeamGrants, Some(rows(&["T:bob", "T:inst-T", "T:tom"]))),
        (Caller(Some("bob")), None),
        (ReadTeamGrants, Some(rows(&["T:bob"]))),
        (Caller(Some("zed")), None),
        (ReadTeamGrants, Some(rows(&[]))),
        // G5 to G10.
        (Caller(Some("tom")), None),
        (InsertTeamGrant { team: "T", principal: "tina", role: "read" }, accepted()),
        (InsertTeamGrant { team: "T", principal: "tess", role: "admin" }, accepted()),
        (Caller(Some("tess")), None),
        (ReadTeamAdmins, Some(rows(&["tess", "tom"]))),
        (Caller(Some("tom")), None),
        (DeleteTeamGrants { principal: "bob" }, accepted()),
        (DeleteTeamGrants { principal: "uma" }, accepted()),
        (InsertTeamGrant { team: "U", principal: "tom", role: "admin" }, refused()),
        (Stored, None),
        // G11 to G16.
        (Caller(Some("bob")), None),
        (ReadOrders, Some(rows(&[]))),
        (Caller(Some("quinn")), None),
        (DeleteProjectGrants { principal: "quinn" }, accepted()),
        (Caller(Some("olga")), None),
        (PromoteOrganizationGrant { principal: "olga" }, accepted()),
        (Stored, None),
        (Caller(Some("alice")), None),
        (InsertOrganizationGrant { principal: "anna", role: "admin" }, accepted()),
        (Caller(Some("anna")), None),
        (ReadOrganizationAdmins, Some(rows(&["alice", "anna"]))),
        (Caller(Some("alice")), None),
        (DeleteOrganizationGrants { principal: "anna" }, accepted()),
        // G19.
        (DeleteOrganizationGrants { principal: "olga" }, accepted()),
        (Stored, None),
        // P1 to P4.
        (Caller(Some("tom")), None),
        (MoveProject { id: "P", team: "U" }, refused()),
        (Caller(Some("alice")), None),
        (MoveProject { id: "P", team: "U" }, accepted()),
        (Stored, None),
        (Caller(Some("tom")), None),
        (ReadOrders, Some(rows(&[]))),
        (Caller(Some("quinn")), None),
        (ReadOrders, Some(rows(&[]))),
    ]
}

/// Every row of the seven tables, read from the named relations.
macro_rules! stored {
    ($connection:expr, $organizations:ident, $teams:ident, $projects:ident, $orders:ident,
     $organization_grants:ident, $team_grants:ident, $project_grants:ident) => {{
        use schema::{
            $orders, $organization_grants, $organizations, $project_grants, $projects,
            $team_grants, $teams,
        };
        let mut stored = Vec::new();
        let pairs: Vec<(String, String)> = $organizations::table
            .select(($organizations::id, $organizations::created_by))
            .order($organizations::id)
            .load($connection)
            .expect("read organizations");
        stored.extend(pairs.into_iter().map(|(id, by)| format!("organization {id} by {by}")));
        let pairs: Vec<(String, String)> = $teams::table
            .select(($teams::id, $teams::organization_id))
            .order($teams::id)
            .load($connection)
            .expect("read teams");
        stored.extend(pairs.into_iter().map(|(id, parent)| format!("team {id} in {parent}")));
        let pairs: Vec<(String, String)> = $projects::table
            .select(($projects::id, $projects::team_id))
            .order($projects::id)
            .load($connection)
            .expect("read projects");
        stored.extend(pairs.into_iter().map(|(id, parent)| format!("project {id} in {parent}")));
        let orders: Vec<(String, String, Option<String>)> = $orders::table
            .select(($orders::id, $orders::project_id, $orders::title))
            .order($orders::id)
            .load($connection)
            .expect("read orders");
        stored.extend(
            orders
                .into_iter()
                .map(|(id, parent, title)| format!("order {id} in {parent} titled {title:?}")),
        );
        let grants: Vec<(String, String, String)> = $organization_grants::table
            .select((
                $organization_grants::organization_id,
                $organization_grants::principal_id,
                $organization_grants::role,
            ))
            .order(($organization_grants::organization_id, $organization_grants::principal_id))
            .load($connection)
            .expect("read organization grants");
        stored.extend(grants.into_iter().map(|(on, to, role)| format!("{role} on {on} to {to}")));
        let grants: Vec<(String, String, String)> = $team_grants::table
            .select(($team_grants::team_id, $team_grants::principal_id, $team_grants::role))
            .order(($team_grants::team_id, $team_grants::principal_id))
            .load($connection)
            .expect("read team grants");
        stored.extend(grants.into_iter().map(|(on, to, role)| format!("{role} on {on} to {to}")));
        let grants: Vec<(String, String, String)> = $project_grants::table
            .select((
                $project_grants::project_id,
                $project_grants::principal_id,
                $project_grants::role,
            ))
            .order(($project_grants::project_id, $project_grants::principal_id))
            .load($connection)
            .expect("read project grants");
        stored.extend(grants.into_iter().map(|(on, to, role)| format!("{role} on {on} to {to}")));
        Outcome::Rows(stored)
    }};
}

/// Seeds the matrix's rows through the relations that bypass the policies.
macro_rules! seed {
    ($connection:expr, $organizations:ident, $teams:ident, $projects:ident, $orders:ident,
     $organization_grants:ident, $team_grants:ident, $project_grants:ident) => {{
        use schema::{
            $orders, $organization_grants, $organizations, $project_grants, $projects,
            $team_grants, $teams,
        };
        diesel::insert_into($organizations::table)
            .values(($organizations::id.eq("org"), $organizations::created_by.eq("alice")))
            .execute($connection)
            .expect("seed the organization");
        for (id, parent) in [("T", "org"), ("U", "org")] {
            diesel::insert_into($teams::table)
                .values(($teams::id.eq(id), $teams::organization_id.eq(parent)))
                .execute($connection)
                .expect("seed a team");
        }
        for (id, parent) in [("P", "T"), ("Q", "U")] {
            diesel::insert_into($projects::table)
                .values(($projects::id.eq(id), $projects::team_id.eq(parent)))
                .execute($connection)
                .expect("seed a project");
        }
        for (id, parent, title) in [("oP", "P", "a"), ("oQ", "Q", "b")] {
            diesel::insert_into($orders::table)
                .values((
                    $orders::id.eq(id),
                    $orders::project_id.eq(parent),
                    $orders::title.eq(title),
                ))
                .execute($connection)
                .expect("seed an order");
        }
        // The creator's admin grant, which R100's creator trigger writes.
        for (principal, role) in [("alice", "admin"), ("olga", "read"), ("owen", "write")] {
            diesel::insert_into($organization_grants::table)
                .values((
                    $organization_grants::organization_id.eq("org"),
                    $organization_grants::principal_id.eq(principal),
                    $organization_grants::role.eq(role),
                ))
                .execute($connection)
                .expect("seed an organization grant");
        }
        for (team, principal, role) in [
            ("T", "tom", "admin"),
            ("T", "bob", "write"),
            ("U", "uma", "read"),
            ("T", "inst-T", "write"),
        ] {
            diesel::insert_into($team_grants::table)
                .values((
                    $team_grants::team_id.eq(team),
                    $team_grants::principal_id.eq(principal),
                    $team_grants::role.eq(role),
                ))
                .execute($connection)
                .expect("seed a team grant");
        }
        diesel::insert_into($project_grants::table)
            .values((
                $project_grants::project_id.eq("Q"),
                $project_grants::principal_id.eq("quinn"),
                $project_grants::role.eq("read"),
            ))
            .execute($connection)
            .expect("seed a project grant");
    }};
}

/// Writes the runner once per connection type. The statements are the same
/// diesel either way, but each connection type needs its own copy.
macro_rules! runner {
    ($name:ident, $conn:ty { $($(#[$doc:meta])* $field:ident: $type:ty,)* }) => {
        struct $name {
            connection: $conn,
            $($(#[$doc])* $field: $type,)*
        }

        impl $name {
            #[expect(clippy::too_many_lines, reason = "one arm per kind of step")]
            fn step(&mut self, step: &Step) -> Outcome {
                let connection = &mut self.connection;
                match *step {
                    Step::Caller(caller) => {
                        self.act_as(caller);
                        Outcome::Accepted
                    }
                    Step::ReadOrders => {
                        Outcome::Rows(
                            orders::table
                                .select(orders::id)
                                .order(orders::id)
                                .load(connection)
                                .expect("read orders"),
                        )
                    }
                    Step::ReadTeamGrants => {
                        let grants: Vec<(String, String)> = team_grants::table
                            .select((team_grants::team_id, team_grants::principal_id))
                            .order((team_grants::team_id, team_grants::principal_id))
                            .load(connection)
                            .expect("read team grants");
                        Outcome::Rows(
                            grants
                                .into_iter()
                                .map(|(team, principal)| format!("{team}:{principal}"))
                                .collect(),
                        )
                    }
                    Step::ReadTeamAdmins => {
                        Outcome::Rows(
                            team_grants::table
                                .filter(team_grants::role.eq("admin"))
                                .select(team_grants::principal_id)
                                .order(team_grants::principal_id)
                                .load(connection)
                                .expect("read team admins"),
                        )
                    }
                    Step::ReadOrganizationAdmins => {
                        Outcome::Rows(
                            organization_grants::table
                                .filter(organization_grants::role.eq("admin"))
                                .select(organization_grants::principal_id)
                                .order(organization_grants::principal_id)
                                .load(connection)
                                .expect("read organization admins"),
                        )
                    }
                    Step::RetitleOrders(title) => {
                        Outcome::of(
                            &diesel::update(orders::table)
                                .set(orders::title.eq(title))
                                .execute(connection),
                        )
                    }
                    Step::InsertOrder { id, project } => {
                        Outcome::of(
                            &diesel::insert_into(orders::table)
                                .values((
                                    orders::id.eq(id),
                                    orders::project_id.eq(project),
                                    orders::title.eq("c"),
                                ))
                                .execute(connection),
                        )
                    }
                    Step::MoveOrder { id, project } => {
                        Outcome::of(
                            &diesel::update(orders::table.filter(orders::id.eq(id)))
                                .set(orders::project_id.eq(project))
                                .execute(connection),
                        )
                    }
                    Step::MoveProject { id, team } => {
                        Outcome::of(
                            &diesel::update(projects::table.filter(projects::id.eq(id)))
                                .set(projects::team_id.eq(team))
                                .execute(connection),
                        )
                    }
                    Step::InsertTeamGrant { team, principal, role } => {
                        Outcome::of(
                            &diesel::insert_into(team_grants::table)
                                .values((
                                    team_grants::team_id.eq(team),
                                    team_grants::principal_id.eq(principal),
                                    team_grants::role.eq(role),
                                ))
                                .execute(connection),
                        )
                    }
                    Step::DeleteTeamGrants { principal } => {
                        Outcome::of(
                            &diesel::delete(
                                team_grants::table.filter(team_grants::principal_id.eq(principal)),
                            )
                            .execute(connection),
                        )
                    }
                    Step::DeleteProjectGrants { principal } => {
                        Outcome::of(
                            &diesel::delete(
                                project_grants::table
                                    .filter(project_grants::principal_id.eq(principal)),
                            )
                            .execute(connection),
                        )
                    }
                    Step::InsertOrganizationGrant { principal, role } => {
                        Outcome::of(
                            &diesel::insert_into(organization_grants::table)
                                .values((
                                    organization_grants::organization_id.eq("org"),
                                    organization_grants::principal_id.eq(principal),
                                    organization_grants::role.eq(role),
                                ))
                                .execute(connection),
                        )
                    }
                    Step::PromoteOrganizationGrant { principal } => {
                        Outcome::of(
                            &diesel::update(
                                organization_grants::table
                                    .filter(organization_grants::principal_id.eq(principal)),
                            )
                            .set(organization_grants::role.eq("admin"))
                            .execute(connection),
                        )
                    }
                    Step::DeleteOrganizationGrants { principal } => {
                        Outcome::of(
                            &diesel::delete(
                                organization_grants::table
                                    .filter(organization_grants::principal_id.eq(principal)),
                            )
                            .execute(connection),
                        )
                    }
                    Step::Stored => self.stored(),
                }
            }
        }
    };
}

runner!(PgRunner, PgConnection {});
runner!(SqliteRunner, SqliteConnection {
    /// The caller `app_user()` answers.
    caller: Arc<Mutex<Option<String>>>,
});

impl PgRunner {
    fn act_as(&mut self, caller: Option<&str>) {
        // A session setting has no typed DSL form.
        let set = caller.map_or_else(
            || "RESET app.user_id".to_owned(),
            |caller| format!("SET app.user_id = '{caller}'"),
        );
        diesel::sql_query(set).execute(&mut self.connection).expect("set app.user_id");
    }

    fn stored(&mut self) -> Outcome {
        // A role switch has no typed DSL form. The superuser reads past every
        // policy.
        diesel::sql_query("RESET ROLE").execute(&mut self.connection).expect("reset role");
        let stored = stored!(
            &mut self.connection,
            organizations,
            teams,
            projects,
            orders,
            organization_grants,
            team_grants,
            project_grants
        );
        diesel::sql_query("SET ROLE app").execute(&mut self.connection).expect("set role app");
        stored
    }
}

impl SqliteRunner {
    fn act_as(&mut self, caller: Option<&str>) {
        *self.caller.lock().expect("the caller lock") = caller.map(str::to_owned);
    }

    fn stored(&mut self) -> Outcome {
        stored!(
            &mut self.connection,
            organizations_rls,
            teams_rls,
            projects_rls,
            orders_rls,
            organization_grants_rls,
            team_grants_rls,
            project_grants_rls
        )
    }
}

fn pg_runner() -> PgRunner {
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, R100)
        .unwrap_or_else(|error| panic!("PostgreSQL refused the document: {error}"));
    postgres_harness::apply(&mut connection, PG_READER_GRANTS).expect("grant the reader role");
    seed!(
        &mut connection,
        organizations,
        teams,
        projects,
        orders,
        organization_grants,
        team_grants,
        project_grants
    );
    // A role switch has no typed DSL form.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("set role app");
    PgRunner { connection }
}

fn sqlite_runner() -> SqliteRunner {
    let translated = Pg2Sqlite::default()
        .sql(R100)
        .expect("parse")
        .translate(&options())
        .unwrap_or_else(|error| panic!("pg2sqlite refused the document: {error}"))
        .into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    let caller = Arc::new(Mutex::new(None));
    let answered = Arc::clone(&caller);
    app_user_utils::register_impl(&mut connection, move || {
        answered.lock().expect("the caller lock").clone()
    })
    .expect("register app_user");
    let (triggers, base): (Vec<_>, Vec<_>) =
        translated.iter().map(ToString::to_string).partition(|s| s.starts_with("CREATE TRIGGER"));
    // Translated DDL is runtime text no typed DSL carries. The triggers wait
    // for the seed, which is the replica's counterpart of a bypassing load.
    for statement in &base {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    seed!(
        &mut connection,
        organizations_rls,
        teams_rls,
        projects_rls,
        orders_rls,
        organization_grants_rls,
        team_grants_rls,
        project_grants_rls
    );
    for statement in &triggers {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    SqliteRunner { connection, caller }
}

#[test]
fn the_r100_matrix_reads_and_writes_alike() {
    let mut postgres = pg_runner();
    let mut sqlite = sqlite_runner();
    for (step, recorded) in scenario() {
        let source = postgres.step(&step);
        if let Some(recorded) = recorded {
            assert_eq!(source, recorded, "PostgreSQL against the matrix on {step:?}");
        }
        assert_eq!(sqlite.step(&step), source, "SQLite against PostgreSQL on {step:?}");
    }
}
