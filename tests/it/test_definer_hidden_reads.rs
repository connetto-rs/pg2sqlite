//! Triggers whose `SECURITY DEFINER` bodies read rows the acting role's
//! row-level security hides.
//!
//! A definer body reads as its owner, past the acting role's policies, while
//! the replica holds only the rows the acting role reads. Such a trigger
//! would judge a write on a partial copy, so the replica leaves it to the
//! server and reports it.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationWarning};

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
fn replica(pg: &str) -> (SqliteConnection, Vec<String>) {
    let script = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&options())
        .unwrap_or_else(|error| panic!("{error}\n{pg}"));
    let before_commit = script.before_commit.clone();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    script.register_functions(&mut connection).expect("register helper functions");
    member_write_exempt_utils::register_nondeterministic_impl(&mut connection, || true)
        .expect("exempt the delivery");
    for statement in script.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
        .batch_execute("INSERT INTO members_rls (org, member, admin) VALUES (1, 1, false)")
        .expect("deliver member 1");
    member_write_exempt_utils::register_nondeterministic_impl(&mut connection, || false)
        .expect("end the exemption");
    (connection, before_commit)
}

/// Member 1 leaves organization 1 in one transaction.
fn leave(connection: &mut SqliteConnection, before_commit: &[String]) -> QueryResult<()> {
    connection.transaction(|connection| {
        diesel::delete(members::table.filter(members::member.eq(1))).execute(connection)?;
        for statement in before_commit {
            connection.batch_execute(statement)?;
        }
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
        let (mut connection, before_commit) = replica(&script(body, trigger, ""));

        leave(&mut connection, &before_commit)
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
    let (mut connection, before_commit) = replica(&pg);

    let refused = leave(&mut connection, &before_commit)
        .expect_err("the replica holds every member and no admin");
    assert!(refused.to_string().contains("would lose its last admin"), "{refused}");
    assert_eq!(left_to_server(&pg), []);
}

#[test]
fn an_invoker_trigger_reads_as_the_acting_role_and_stays() {
    let pg = script(DIRECT, AFTER_DELETE, "")
        .replace("SECURITY DEFINER SET search_path", "SET search_path")
        .replace("ALTER FUNCTION keep_an_admin() OWNER TO checker;", "");
    let (mut connection, before_commit) = replica(&pg);

    let refused = leave(&mut connection, &before_commit)
        .expect_err("PostgreSQL shows the acting role no admin either");
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
        let (mut connection, before_commit) = replica(&script(DIRECT, AFTER_DELETE, extra));

        leave(&mut connection, &before_commit).unwrap_or_else(|error| panic!("{error}\n{extra}"));
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
    let (mut connection, _) = replica(&pg);

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
        let (mut connection, before_commit) = replica(&pg);

        leave(&mut connection, &before_commit).unwrap_or_else(|error| panic!("{error}\n{pg}"));
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
    let (mut connection, before_commit) = replica(&pg);

    leave(&mut connection, &before_commit).expect("the server holds the admin");
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
