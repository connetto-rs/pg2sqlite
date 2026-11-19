//! A definer helper inlined as the set it returns, reading a table on which
//! the acting role holds no privilege.
//!
//! The translation leaves such a table out of the replica, so a helper
//! reading it is refused, naming the table and the grant that keeps it.

use diesel::prelude::*;
use pg2sqlite::prelude::Pg2SqliteOptions;
use sql_traits::structs::Deployment;

use crate::helpers;

diesel::table! {
    /// The table the policy guards, as the acting role reads it.
    t (id) {
        /// Primary key.
        id -> Integer,
        /// The row's owner, matched against what the helper returns.
        owner -> Integer,
    }
}

diesel::table! {
    /// The storage behind `t`.
    t_rls (id) {
        /// Primary key.
        id -> Integer,
        /// The row's owner.
        owner -> Integer,
    }
}

diesel::table! {
    /// The storage behind `secret`, which the helper reads.
    secret_rls (id) {
        /// Primary key.
        id -> Integer,
        /// Which rows the helper returns.
        kind -> Text,
    }
}

/// `secret`, which `visible_ids()` reads as `owner_role`, `extra`, and `t`,
/// whose policy for `app` compares a column with what the helper returns.
fn script(extra: &str, policy: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE ROLE owner_role;
         CREATE TABLE secret (id INT PRIMARY KEY, kind TEXT NOT NULL);
         CREATE FUNCTION visible_ids() RETURNS SETOF INT LANGUAGE sql STABLE SECURITY DEFINER
           SET search_path = pg_catalog, pg_temp AS $$ SELECT id FROM public.secret WHERE kind = 'x' $$;
         ALTER FUNCTION visible_ids() OWNER TO owner_role;
         GRANT SELECT ON secret TO owner_role;
         {extra}
         CREATE TABLE t (id INT PRIMARY KEY, owner INT NOT NULL);
         ALTER TABLE t ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON t {policy};
         GRANT SELECT, INSERT ON t TO app;"
    )
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(Deployment::new("app", ["app"], []).expect("a valid deployment"))
        .with_rls_audit_table_name("rls_audit")
        .with_acting_role("app")
}

const REFUSAL: &str = "Unsupported SQLite feature: visible_ids() cannot be translated as the set \
                       it returns: it reads secret, on which app holds no privilege, so the \
                       translation leaves it out of the replica. Grant app SELECT on it, under a \
                       policy hiding its rows if they are not the role's to read.";

fn refusal(pg: &str) -> String {
    match helpers::translate_pg(pg, &options()) {
        Ok(script) => panic!("translated:\n{}", script.join("\n")),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_read_policy_calling_a_helper_over_a_hidden_table_is_refused() {
    let pg = script("", "FOR SELECT TO app USING (owner IN (SELECT visible_ids()))");
    assert_eq!(refusal(&pg), REFUSAL);
}

#[test]
fn a_write_check_calling_a_helper_over_a_hidden_table_is_refused() {
    let pg = script(
        "",
        "FOR INSERT TO app WITH CHECK (owner IN (SELECT id FROM visible_ids() AS visible (id)))",
    );
    assert_eq!(refusal(&pg), REFUSAL);
}

/// The grant the refusal names keeps `secret` on the replica, its rows hidden
/// from `app` and read by the helper.
#[test]
fn granting_the_table_under_a_hiding_policy_lets_the_helper_read_it() {
    let pg = script(
        "ALTER TABLE secret OWNER TO owner_role;
         ALTER TABLE secret ENABLE ROW LEVEL SECURITY;
         CREATE POLICY hidden ON secret FOR SELECT TO app USING (false);
         GRANT SELECT ON secret TO app;",
        "FOR SELECT TO app USING (owner IN (SELECT visible_ids()))",
    );
    let script = helpers::translate_pg(&pg, &options()).unwrap_or_else(|error| panic!("{error}"));
    let mut connection = helpers::apply_statements(&script);
    // The rows arrive as the sync delivers them, past the guards on the
    // backing tables.
    connection.set_triggers_enabled(false).expect("disable triggers for the seed");
    diesel::insert_into(secret_rls::table)
        .values(&[
            (secret_rls::id.eq(1), secret_rls::kind.eq("x")),
            (secret_rls::id.eq(2), secret_rls::kind.eq("y")),
        ])
        .execute(&mut connection)
        .expect("the helper's rows are stored");
    diesel::insert_into(t_rls::table)
        .values(&[(t_rls::id.eq(10), t_rls::owner.eq(1)), (t_rls::id.eq(20), t_rls::owner.eq(2))])
        .execute(&mut connection)
        .expect("the guarded rows are stored");
    connection.set_triggers_enabled(true).expect("re-enable triggers");
    let visible = t::table.select(t::id).load::<i32>(&mut connection).expect("the view reads");
    assert_eq!(visible, [10]);
}
