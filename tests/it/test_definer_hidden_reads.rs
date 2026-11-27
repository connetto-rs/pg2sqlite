//! Triggers whose `SECURITY DEFINER` bodies read rows the acting role's
//! row-level security hides.
//!
//! A definer body reads as its owner, past the acting role's policies, while
//! the replica holds only the rows the acting role reads. Such a trigger
//! would judge a write on a partial copy, so the replica leaves it to the
//! server and reports it, unless the facts the firing row carries confine
//! every deciding read to rows the acting role reads.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, TranslationWarning};
use sql_traits::structs::Deployment;

use crate::helpers;

diesel::table! {
    /// The members of each organization, as the acting role reads them.
    members (org, member) {
        /// The organization.
        org -> Integer,
        /// The member.
        member -> Integer,
        /// Whether the member administers the organization.
        admin -> Bool,
    }
}

diesel::table! {
    /// The members a definer trigger records as they join.
    admissions (org, member) {
        /// The organization.
        org -> Integer,
        /// The member.
        member -> Integer,
    }
}

diesel::define_sql_function! {
    /// Whether the connection writes past the generated guards.
    fn member_write_exempt() -> diesel::sql_types::Bool;
}

/// `members`, of which the acting role `app` reads its own rows as member 1,
/// and `keep_an_admin()`, which `checker` runs over the organization of each
/// member who leaves. `body` reads the members, `trigger` creates the
/// trigger, and `extra` follows.
fn script(body: &str, trigger: &str, extra: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE ROLE checker BYPASSRLS;
         CREATE TABLE members (org INT NOT NULL, member INT NOT NULL, admin BOOLEAN NOT NULL,
           PRIMARY KEY (org, member));
         ALTER TABLE members ENABLE ROW LEVEL SECURITY;
         CREATE POLICY members_own ON members TO app USING (member = 1);
         GRANT SELECT, INSERT, DELETE ON members TO app;
         GRANT SELECT ON members TO checker;
         CREATE FUNCTION has_admin(target INT) RETURNS BOOLEAN LANGUAGE sql STABLE AS $$
           SELECT EXISTS (SELECT 1 FROM public.members WHERE org = target AND admin)
         $$;
         CREATE FUNCTION keep_an_admin() RETURNS trigger
         LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
         BEGIN
           IF NOT ({body}) THEN
             RAISE EXCEPTION 'organization % would lose its last admin', OLD.org
               USING ERRCODE = 'check_violation';
           END IF;
           RETURN NULL;
         END
         $$;
         ALTER FUNCTION keep_an_admin() OWNER TO checker;
         {trigger}
         {extra}"
    )
}

const DIRECT: &str = "EXISTS (SELECT 1 FROM public.members WHERE org = OLD.org AND admin)";

const THROUGH_HELPER: &str = "public.has_admin(OLD.org)";

const AFTER_DELETE: &str = "CREATE TRIGGER members_keep_admin AFTER DELETE ON members
   FOR EACH ROW EXECUTE FUNCTION keep_an_admin();";

const DEFERRED: &str = "CREATE CONSTRAINT TRIGGER members_keep_admin AFTER DELETE ON members
   DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION keep_an_admin();";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("member_write_exempt")
        .with_acting_role("app")
}

/// `pg` applied to a fresh connection holding the row member 1 reads, a
/// plain member of organization 1, whose admin the server holds alone. Each
/// write runs with the statements a `COMMIT` runs first.
fn replica(pg: &str) -> crate::helpers::Replica {
    let script = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&options())
        .unwrap_or_else(|error| panic!("{error}\n{pg}"));
    let mut connection = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    member_write_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        || true,
    )
    .expect("exempt the delivery");
    script
        .initialize_managed(&mut connection, pg2sqlite::prelude::FreshFiles::Both)
        .expect("initialize replica");
    connection
        .batch_execute("INSERT INTO members_rls (org, member, admin) VALUES (1, 1, false)")
        .expect("deliver member 1");
    member_write_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        || false,
    )
    .expect("end the exemption");
    connection
}

/// Member 1 leaves organization 1 in one transaction.
fn leave(connection: &mut crate::helpers::Replica) -> QueryResult<()> {
    connection.transaction(|connection| {
        diesel::delete(members::table.filter(members::member.eq(1))).execute(connection)?;
        Ok(())
    })
}

fn left_to_server(pg: &str) -> Vec<TranslationWarning> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_with_report(&options())
        .expect("translate")
        .warnings
        .into_iter()
        .filter(|warning| matches!(warning, TranslationWarning::LeftToServer { .. }))
        .collect()
}

#[test]
fn a_definer_trigger_reading_hidden_rows_leaves_its_check_to_the_server() {
    for (body, trigger) in
        [(DIRECT, AFTER_DELETE), (THROUGH_HELPER, AFTER_DELETE), (DIRECT, DEFERRED)]
    {
        let mut connection = replica(&script(body, trigger, ""));

        leave(&mut connection)
            .unwrap_or_else(|error| panic!("the server holds the admin: {error}\n{trigger}"));
    }
}

#[test]
fn the_report_names_the_trigger_and_the_hiding_policy() {
    for (body, trigger) in [(DIRECT, AFTER_DELETE), (THROUGH_HELPER, DEFERRED)] {
        let warnings = left_to_server(&script(body, trigger, ""));

        let [TranslationWarning::LeftToServer { construct, name, table, reason }] =
            warnings.as_slice()
        else {
            panic!("one trigger left to the server: {warnings:?}");
        };
        assert_eq!(
            (construct.as_str(), name.as_str(), table.as_str()),
            ("TRIGGER", "members_keep_admin", "members")
        );
        assert!(reason.contains("members_own"), "{reason}");
    }
}

#[test]
fn a_definer_trigger_reading_rows_the_role_reads_in_full_stays() {
    let pg = script(DIRECT, AFTER_DELETE, "").replace(
        "CREATE POLICY members_own ON members TO app USING (member = 1);",
        "CREATE POLICY members_own ON members TO app USING (true);",
    );
    let mut connection = replica(&pg);

    let refused = leave(&mut connection).expect_err("the replica holds every member and no admin");
    assert!(refused.to_string().contains("would lose its last admin"), "{refused}");
    assert_eq!(left_to_server(&pg), []);
}

#[test]
fn an_invoker_trigger_reads_as_the_acting_role_and_stays() {
    let pg = script(DIRECT, AFTER_DELETE, "")
        .replace("SECURITY DEFINER SET search_path", "SET search_path")
        .replace("ALTER FUNCTION keep_an_admin() OWNER TO checker;", "");
    let mut connection = replica(&pg);

    let refused =
        leave(&mut connection).expect_err("PostgreSQL shows the acting role no admin either");
    assert!(refused.to_string().contains("would lose its last admin"), "{refused}");
    assert_eq!(left_to_server(&pg), []);
}

#[test]
fn dropping_or_replacing_a_trigger_left_to_the_server_drops_nothing() {
    for extra in [
        "DROP TRIGGER members_keep_admin ON members;",
        "CREATE OR REPLACE TRIGGER members_keep_admin AFTER DELETE ON members
           FOR EACH ROW EXECUTE FUNCTION keep_an_admin();",
    ] {
        let mut connection = replica(&script(DIRECT, AFTER_DELETE, extra));

        leave(&mut connection).unwrap_or_else(|error| panic!("{error}\n{extra}"));
    }
}

/// `admissions`, written by the `SECURITY DEFINER` body of
/// `members_admitted`, `timing` each member, with `statement`.
fn admitting(timing: &str, statement: &str) -> String {
    script(
        DIRECT,
        "",
        &format!(
            "CREATE TABLE admissions (org INT NOT NULL, member INT NOT NULL,
               PRIMARY KEY (org, member));
             GRANT SELECT ON admissions TO app;
             GRANT SELECT, INSERT, UPDATE, DELETE ON admissions TO checker;
             CREATE FUNCTION admit() RETURNS trigger
             LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
             BEGIN
               {statement};
               RETURN NULL;
             END
             $$;
             ALTER FUNCTION admit() OWNER TO checker;
             CREATE TRIGGER members_admitted {timing} ON members
               FOR EACH ROW EXECUTE FUNCTION admit();"
        ),
    )
}

#[test]
fn a_definer_write_picking_rows_from_hidden_rows_stays_and_writes_from_the_rows_held() {
    let pg = admitting(
        "AFTER INSERT",
        "INSERT INTO public.admissions SELECT org, member FROM public.members
           WHERE org = NEW.org AND member = NEW.member",
    );
    let mut connection = replica(&pg);

    diesel::insert_into(members::table)
        .values((members::org.eq(2), members::member.eq(1), members::admin.eq(true)))
        .execute(&mut connection)
        .expect("member 1 founds organization 2");
    let admitted: Vec<(i32, i32)> =
        admissions::table.load(&mut connection).expect("read the admissions");
    assert_eq!(admitted, [(2, 1)]);
    assert_eq!(left_to_server(&pg), []);
}

#[test]
fn definer_updates_and_deletes_filtered_by_hidden_rows_stay() {
    for statement in [
        "DELETE FROM public.admissions WHERE org = OLD.org
           AND member IN (SELECT member FROM public.members WHERE org = OLD.org AND admin)",
        "UPDATE public.admissions SET member = OLD.member
           WHERE (org, member) IN (SELECT org, member FROM public.members WHERE admin)",
    ] {
        let pg = admitting("AFTER DELETE", statement);
        let mut connection = replica(&pg);

        leave(&mut connection).unwrap_or_else(|error| panic!("{error}\n{pg}"));
        assert_eq!(left_to_server(&pg), [], "{statement}");
    }
}

#[test]
fn a_definer_check_on_found_after_reading_hidden_rows_is_left_to_the_server() {
    let pg = script(DIRECT, AFTER_DELETE, "").replace(
        &format!("IF NOT ({DIRECT}) THEN"),
        "PERFORM 1 FROM public.members WHERE org = OLD.org AND admin;
           IF NOT FOUND THEN",
    );
    let mut connection = replica(&pg);

    leave(&mut connection).expect("the server holds the admin");
    assert_eq!(left_to_server(&pg).len(), 1, "{pg}");
}

#[test]
fn constructs_reading_what_a_trigger_left_to_the_server_writes_are_reported() {
    let pg = script(
        DIRECT,
        AFTER_DELETE,
        "CREATE TABLE departures (org INT NOT NULL, member INT NOT NULL,
           CONSTRAINT departures_member CHECK (member > 0));
         GRANT SELECT ON departures TO app;
         GRANT INSERT ON departures TO checker;",
    )
    .replace(
        "RETURN NULL;",
        "INSERT INTO public.departures VALUES (OLD.org, OLD.member);\n RETURN NULL;",
    );
    let report = Pg2Sqlite::default()
        .sql(&pg)
        .expect("parse")
        .translate_with_report(&options())
        .expect("translate");

    let stale: Vec<(&str, &str, &str, &str)> = report
        .warnings
        .iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::ReadsLeftToServer {
                    construct,
                    name,
                    read,
                    writer,
                    writer_name,
                    ..
                } => {
                    (writer_name == "members_keep_admin").then_some((
                        construct.as_str(),
                        name.as_str(),
                        read.as_str(),
                        writer.as_str(),
                    ))
                }
                _ => None,
            }
        })
        .collect();
    assert_eq!(stale, [("CHECK", "departures_member", "departures", "TRIGGER")]);
}

diesel::table! {
    /// The runs of each team, as the acting role reads them.
    runs (id) {
        /// The run.
        id -> Integer,
        /// The team running it.
        team_id -> Integer,
        /// Whether the run takes bindings.
        status -> Text,
    }
}

diesel::table! {
    /// The bindings `bindings_guard` checks against their run.
    bindings (id) {
        /// The binding.
        id -> Integer,
        /// The team writing it.
        team_id -> Integer,
        /// The run it binds to.
        run_id -> Integer,
        /// A run it refers to besides.
        other_run_id -> Integer,
    }
}

diesel::define_sql_function! {
    /// The team the request acts for, `current_setting('app.team_id')::INT`
    /// on PostgreSQL.
    fn app_team_id() -> diesel::sql_types::Integer;
}

const TEAM: &str = "team_id = current_setting('app.team_id')::INT";

const COMPOSITE_RUN_KEY: &str = "FOREIGN KEY (run_id, team_id) REFERENCES runs (id, team_id)";

/// `bindings_guard`, refusing a binding whose run `key` names is not open,
/// the run of each binding referenced by `run_key`. `runs_select` admits the
/// runs `readable` holds of, and `bindings_insert` the bindings `writable`
/// holds of, after `extra`.
fn bindings(key: &str, run_key: &str, readable: &str, writable: &str, extra: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE ROLE guard_owner BYPASSRLS;
         {extra}
         CREATE TABLE runs (id INT PRIMARY KEY, team_id INT NOT NULL, status TEXT NOT NULL,
           UNIQUE (id, team_id));
         ALTER TABLE runs ENABLE ROW LEVEL SECURITY;
         CREATE POLICY runs_select ON runs FOR SELECT TO app USING ({readable});
         CREATE TABLE bindings (id INT PRIMARY KEY, team_id INT NOT NULL,
           run_id INT NOT NULL, other_run_id INT NOT NULL REFERENCES runs (id),
           {run_key});
         ALTER TABLE bindings ENABLE ROW LEVEL SECURITY;
         CREATE POLICY bindings_select ON bindings FOR SELECT TO app USING ({readable});
         CREATE POLICY bindings_insert ON bindings FOR INSERT TO app WITH CHECK ({writable});
         GRANT SELECT ON runs TO app, guard_owner;
         GRANT SELECT, INSERT ON bindings TO app;
         CREATE FUNCTION guard_binding() RETURNS trigger
         LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
         BEGIN
           IF EXISTS (SELECT 1 FROM public.runs run WHERE run.id = {key} AND run.status <> 'open')
           THEN
             RAISE EXCEPTION 'run % is closed', {key} USING ERRCODE = 'check_violation';
           END IF;
           RETURN NEW;
         END
         $$;
         ALTER FUNCTION guard_binding() OWNER TO guard_owner;
         REVOKE EXECUTE ON FUNCTION guard_binding() FROM PUBLIC;
         CREATE TRIGGER bindings_guard AFTER INSERT ON bindings
           FOR EACH ROW EXECUTE FUNCTION guard_binding();"
    )
}

/// The translation acting as `app` in `deployment`, with `mapping`.
fn acting_options(deployment: Deployment, mapping: SessionVariableMapping) -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(deployment)
        .with_session_variable(mapping)
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("member_write_exempt")
        .with_acting_role("app")
}

/// The translation acting as `app` for the team `app.team_id` names, read
/// through `app_team_id()`.
fn team_options() -> Pg2SqliteOptions {
    acting_options(
        helpers::deployment::deployment(
            ["app"],
            [helpers::deployment::scalar("app.team_id", "int")],
        ),
        SessionVariableMapping::new("app.team_id", "app_team_id"),
    )
}

/// The triggers `pg` leaves to the server under `options`, each with its
/// reason.
fn triggers_left(pg: &str, options: &Pg2SqliteOptions) -> Vec<(String, String)> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_with_report(options)
        .unwrap_or_else(|error| panic!("{error}\n{pg}"))
        .warnings
        .into_iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LeftToServer { construct, name, reason, .. }
                    if construct == "TRIGGER" =>
                {
                    Some((name, reason))
                }
                _ => None,
            }
        })
        .collect()
}

#[test]
fn a_definer_read_the_firing_rows_key_confines_to_the_team_stays_and_refuses_on_the_replica() {
    let pg = bindings("NEW.run_id", COMPOSITE_RUN_KEY, TEAM, TEAM, "");
    assert_eq!(triggers_left(&pg, &team_options()), []);
    let locking = pg.replace("run.status <> 'open')", "run.status <> 'open' FOR UPDATE)");
    assert_eq!(triggers_left(&locking, &team_options()), [], "{locking}");

    let script = Pg2Sqlite::default()
        .sql(&pg)
        .expect("parse")
        .translate_to_sql(&team_options())
        .expect("translate");
    let mut connection = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    {
        let mut registration = connection.registration().expect("registration");
        app_team_id_utils::register_nondeterministic_impl(registration.sqlite(), || 1)
            .expect("act for team 1");
        member_write_exempt_utils::register_nondeterministic_impl(registration.sqlite(), || true)
            .expect("exempt the delivery");
    }
    script
        .initialize_managed(&mut connection, pg2sqlite::prelude::FreshFiles::Both)
        .expect("initialize replica");
    connection
        .batch_execute(
            "INSERT INTO runs_rls (id, team_id, status) VALUES (1, 1, 'closed'), (2, 1, 'open')",
        )
        .expect("deliver the team's runs");
    member_write_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        || false,
    )
    .expect("end the exemption");
    let bind = |connection: &mut crate::helpers::Replica, id: i32, run: i32| {
        diesel::insert_into(bindings::table)
            .values((
                bindings::id.eq(id),
                bindings::team_id.eq(1),
                bindings::run_id.eq(run),
                bindings::other_run_id.eq(run),
            ))
            .execute(connection)
    };

    let refused = bind(&mut connection, 1, 1).expect_err("run 1 is closed");
    assert!(refused.to_string().contains("is closed"), "{refused}");
    bind(&mut connection, 2, 2).expect("run 2 is open");
}

#[test]
fn a_definer_read_no_fact_confines_is_left_to_the_server_naming_the_policy() {
    for pg in [
        bindings("NEW.other_run_id", COMPOSITE_RUN_KEY, TEAM, TEAM, ""),
        bindings("NEW.run_id", "FOREIGN KEY (run_id) REFERENCES runs (id)", TEAM, TEAM, ""),
    ] {
        let left = triggers_left(&pg, &team_options());

        let [(name, reason)] = left.as_slice() else { panic!("one trigger left: {left:?}\n{pg}") };
        assert_eq!(name, "bindings_guard");
        assert!(reason.contains("implies runs_select"), "{reason}");
    }
}

#[test]
fn the_report_names_why_the_proof_stopped_at_the_read() {
    let other_run = bindings("NEW.other_run_id", COMPOSITE_RUN_KEY, TEAM, TEAM, "");
    let own_run = bindings("NEW.run_id", COMPOSITE_RUN_KEY, TEAM, TEAM, "");
    let runs_select =
        format!("CREATE POLICY runs_select ON runs FOR SELECT TO app USING ({TEAM});");
    for (pg, why) in [
        (
            other_run.replace(
                &runs_select,
                &format!(
                    "CREATE POLICY runs_select ON runs FOR SELECT TO app USING (true);
                     CREATE POLICY runs_team ON runs AS RESTRICTIVE FOR SELECT TO app
                       USING ({TEAM});"
                ),
            ),
            "implies the restrictive policy runs_team",
        ),
        (other_run.replace(&runs_select, ""), "no policy lets the acting role read a row of runs"),
        (
            own_run.replace(
                "GRANT SELECT ON runs TO app, guard_owner;",
                "GRANT SELECT (id, team_id) ON runs TO app; GRANT SELECT ON runs TO guard_owner;",
            ),
            "may not select its column status",
        ),
        (
            own_run.replace("public.runs run WHERE", "public.runs AS run (id) WHERE"),
            "cannot be followed",
        ),
    ] {
        let left = triggers_left(&pg, &team_options());

        let [(_, reason)] = left.as_slice() else { panic!("one trigger left: {left:?}\n{pg}") };
        assert!(reason.contains(why), "{why}: {reason}");
    }
}

#[test]
fn a_write_policy_on_a_narrower_declared_set_confines_the_definer_read() {
    let helper = "CREATE SCHEMA authz;
         CREATE TABLE team_grants (team_id INT NOT NULL, principal_id TEXT NOT NULL,
           role TEXT NOT NULL, PRIMARY KEY (team_id, principal_id));
         ALTER TABLE team_grants ENABLE ROW LEVEL SECURITY;
         CREATE POLICY team_grants_own ON team_grants FOR SELECT TO app
           USING (principal_id = current_setting('app.user_id', true));
         GRANT SELECT ON team_grants TO app, guard_owner;
         CREATE FUNCTION authz.teams_with(min_role TEXT) RETURNS SETOF INT
         LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
           SELECT g.team_id FROM public.team_grants g
           WHERE g.principal_id = current_setting('app.user_id', true) AND g.role = min_role
         $$;
         ALTER FUNCTION authz.teams_with(TEXT) OWNER TO guard_owner;";
    let pg = bindings(
        "NEW.run_id",
        COMPOSITE_RUN_KEY,
        "team_id = ANY (ARRAY(SELECT authz.teams_with('reader')))",
        "team_id = ANY (ARRAY(SELECT authz.teams_with('writer')))",
        helper,
    );
    let deployment = || {
        helpers::deployment::deployment(
            ["app"],
            [helpers::deployment::caller("app.user_id", "text")],
        )
    };
    let options = |deployment: Deployment| {
        acting_options(deployment, SessionVariableMapping::new("app.user_id", "app_user"))
    };

    let undeclared = triggers_left(&pg, &options(deployment()));
    let [(_, reason)] = undeclared.as_slice() else { panic!("one trigger left: {undeclared:?}") };
    assert!(reason.contains("implies runs_select"), "{reason}");

    let declared = deployment()
        .with_set_within("authz.teams_with('writer')", "authz.teams_with('reader')")
        .expect("valid set expressions");
    assert_eq!(triggers_left(&pg, &options(declared)), []);
}
