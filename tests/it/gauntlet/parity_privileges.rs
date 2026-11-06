//! Parity test: the acting role's grants gate each command and each column on
//! the replica the way PostgreSQL's privilege checks gate them.
//!
//! Every case runs one list of statements against both engines, as the acting
//! role, and compares what each answered and what the storage holds after. A
//! case runs once on a plain table and once with row-level security enabled
//! under a policy admitting every row, since the two translate through
//! different wrappers. The few places where the replica cannot answer as
//! PostgreSQL does are spelled out step by step, with both answers.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::{
    manifest::WrapperKind,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};

use crate::helpers::{deployment::deployment, establish_connection, postgres as postgres_harness};

mod schema {
    diesel::table! {
        t (id) {
            id -> Integer,
            owner -> Nullable<Text>,
            secret -> Nullable<Text>,
        }
    }
    diesel::table! {
        /// The table behind the `t` view on the SQLite side.
        t_rls (id) {
            id -> Integer,
            owner -> Nullable<Text>,
            secret -> Nullable<Text>,
        }
    }
}

use schema::{t, t_rls};

const TABLE: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, owner TEXT, secret TEXT);";

const ROW_SECURITY: &str = "
ALTER TABLE t ENABLE ROW LEVEL SECURITY;
CREATE POLICY t_every_row ON t USING (true) WITH CHECK (true);
";

/// The roles the cases act as, declared for the translation, which reads them
/// from the schema. PostgreSQL has them cluster-wide already.
const ROLES: &str = "
CREATE ROLE postgres SUPERUSER;
CREATE ROLE alice;
CREATE ROLE privilege_group;
CREATE ROLE privilege_member;
GRANT privilege_group TO privilege_member;
";

/// What a statement answered.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    /// The rows a read returned, as `(id, owner)`.
    Rows(Vec<(i32, Option<String>)>),
    /// The write went through.
    Wrote,
    /// The engine refused, with its message.
    Refused(String),
}

fn rows(rows: &[(i32, &str)]) -> Answer {
    Answer::Rows(rows.iter().map(|(id, owner)| (*id, Some((*owner).to_owned()))).collect())
}

fn refused(message: &str) -> Answer {
    Answer::Refused(message.to_owned())
}

const DENIED: &str = "permission denied for table t";

/// One statement a case runs as the acting role.
#[derive(Debug, Clone, Copy)]
enum Statement {
    /// `SELECT id, owner FROM t`.
    SelectReadable,
    /// `SELECT * FROM t`.
    SelectStar,
    /// `UPDATE t SET owner = $2 WHERE id = $1`.
    UpdateOwner(i32, &'static str),
    /// `UPDATE t SET secret = $2 WHERE id = $1`.
    UpdateSecret(i32, &'static str),
    /// `INSERT INTO t (id, owner) VALUES ($1, $2)`.
    InsertReadable(i32, &'static str),
    /// `INSERT INTO t (id, owner, secret) VALUES ($1, $2, $3)`.
    InsertSecret(i32, &'static str, Option<&'static str>),
    /// `DELETE FROM t WHERE id = $1`.
    Delete(i32),
    /// `INSERT INTO t (id, owner) SELECT id, owner FROM t WHERE id = $1`.
    InsertSelected(i32),
    /// `INSERT INTO t (id) VALUES ($1)`.
    InsertId(i32),
}

/// A row as the storage holds it, `(id, owner, secret)`.
type Stored = (i32, Option<String>, Option<String>);

#[derive(diesel::QueryableByName)]
struct Shown {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    owner: Option<String>,
}

fn answer<T>(result: QueryResult<T>, read: impl FnOnce(T) -> Answer) -> Answer {
    match result {
        Ok(value) => read(value),
        Err(diesel::result::Error::DatabaseError(_, information)) => {
            Answer::Refused(information.message().to_owned())
        }
        Err(error) => panic!("not a database answer: {error}"),
    }
}

/// Writes the runner twice, once per connection type. The statements are the
/// same diesel either way, but each connection type needs its own copy, and
/// spelling out the backend bounds costs more than this does.
macro_rules! runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
        }

        impl $name {
            fn run(&mut self, statement: Statement) -> Answer {
                let connection = &mut self.connection;
                let wrote = |_: usize| Answer::Wrote;
                match statement {
                    Statement::SelectReadable => {
                        answer(
                            t::table
                                .select((t::id, t::owner))
                                .order(t::id)
                                .load::<(i32, Option<String>)>(connection),
                            Answer::Rows,
                        )
                    }
                    // The DSL always lists the columns, and the wildcard is
                    // what this step measures.
                    Statement::SelectStar => {
                        answer(
                            diesel::sql_query("SELECT * FROM t ORDER BY id")
                                .load::<Shown>(connection),
                            |shown| {
                                Answer::Rows(
                                    shown.into_iter().map(|row| (row.id, row.owner)).collect(),
                                )
                            },
                        )
                    }
                    Statement::UpdateOwner(id, owner) => {
                        answer(
                            diesel::update(t::table.filter(t::id.eq(id)))
                                .set(t::owner.eq(owner))
                                .execute(connection),
                            wrote,
                        )
                    }
                    Statement::UpdateSecret(id, secret) => {
                        answer(
                            diesel::update(t::table.filter(t::id.eq(id)))
                                .set(t::secret.eq(secret))
                                .execute(connection),
                            wrote,
                        )
                    }
                    Statement::InsertReadable(id, owner) => {
                        answer(
                            diesel::insert_into(t::table)
                                .values((t::id.eq(id), t::owner.eq(owner)))
                                .execute(connection),
                            wrote,
                        )
                    }
                    Statement::InsertSecret(id, owner, secret) => {
                        answer(
                            diesel::insert_into(t::table)
                                .values((t::id.eq(id), t::owner.eq(owner), t::secret.eq(secret)))
                                .execute(connection),
                            wrote,
                        )
                    }
                    Statement::Delete(id) => {
                        answer(
                            diesel::delete(t::table.filter(t::id.eq(id))).execute(connection),
                            wrote,
                        )
                    }
                    Statement::InsertSelected(id) => {
                        answer(
                            diesel::insert_into(t::table)
                                .values(t::table.filter(t::id.eq(id)).select((t::id, t::owner)))
                                .into_columns((t::id, t::owner))
                                .execute(connection),
                            wrote,
                        )
                    }
                    Statement::InsertId(id) => {
                        answer(
                            diesel::insert_into(t::table).values(t::id.eq(id)).execute(connection),
                            wrote,
                        )
                    }
                }
            }
        }
    };
}

runner!(PgRunner, PgConnection);
runner!(SqliteRunner, SqliteConnection);

/// The source, acting as one role.
struct Source {
    runner: PgRunner,
    acting: String,
}

impl Source {
    fn new(grants: &str, row_security: bool, acting: &str) -> Self {
        let mut connection = postgres_harness::fresh_database();
        let security = if row_security { ROW_SECURITY } else { "" };
        postgres_harness::apply(&mut connection, &format!("{TABLE}{security}{grants}"))
            .unwrap_or_else(|error| panic!("PostgreSQL refused the schema: {error}"));
        diesel::insert_into(t::table)
            .values((t::id.eq(1), t::owner.eq("a"), t::secret.eq("s")))
            .execute(&mut connection)
            .expect("seed the row");
        let mut source = Self { runner: PgRunner { connection }, acting: acting.to_owned() };
        source.act();
        source
    }

    fn act(&mut self) {
        // A role switch has no typed DSL form.
        diesel::sql_query(format!("SET ROLE {}", self.acting))
            .execute(&mut self.runner.connection)
            .expect("set the acting role");
    }

    fn run(&mut self, statement: Statement) -> Answer {
        self.runner.run(statement)
    }

    /// The rows as the owner reads them.
    fn stored(&mut self) -> Vec<Stored> {
        // A role switch has no typed DSL form.
        diesel::sql_query("RESET ROLE")
            .execute(&mut self.runner.connection)
            .expect("reset the role");
        let stored =
            t::table.order(t::id).load(&mut self.runner.connection).expect("read the storage");
        self.act();
        stored
    }
}

/// The replica, with the wrapper the translation put around `t`.
struct Replica {
    runner: SqliteRunner,
    wrapper: WrapperKind,
    warnings: Vec<TranslationWarning>,
}

impl Replica {
    fn new(grants: &str, row_security: bool, acting: &str) -> Self {
        let security = if row_security { ROW_SECURITY } else { "" };
        let source = format!("{ROLES}{TABLE}{security}{grants}");
        let options = Pg2SqliteOptions::default()
            .with_rls_audit_table_name("rls_audit")
            .with_deployment(deployment([acting], []))
            .with_acting_role(acting);
        let translator = Pg2Sqlite::default().sql(&source).expect("parse");
        let wrapper = translator
            .translation_manifest(&options)
            .expect("manifest")
            .into_iter()
            .find(|entry| entry.logical == "t")
            .map(|entry| entry.wrapper)
            .expect("t reaches the replica");
        let report = translator
            .translate_with_report(&options)
            .unwrap_or_else(|error| panic!("pg2sqlite refused the schema: {error}"));
        let warnings = report.warnings;
        let translated = report.script.into_vec();
        let mut connection = establish_connection();
        let (triggers, base): (Vec<_>, Vec<_>) = translated
            .iter()
            .map(ToString::to_string)
            .partition(|s| s.starts_with("CREATE TRIGGER"));
        // Translated DDL is runtime text no typed DSL carries. The triggers
        // wait for the seed, which is the replica's counterpart of the owner's
        // load.
        for statement in &base {
            diesel::sql_query(statement)
                .execute(&mut connection)
                .unwrap_or_else(|error| panic!("{error}\n{statement}"));
        }
        let mut replica = Self { runner: SqliteRunner { connection }, wrapper, warnings };
        replica.seed();
        for statement in &triggers {
            diesel::sql_query(statement)
                .execute(&mut replica.runner.connection)
                .unwrap_or_else(|error| panic!("{error}\n{statement}"));
        }
        replica
    }

    const fn is_split(&self) -> bool {
        matches!(self.wrapper, WrapperKind::RlsView | WrapperKind::PrivilegeView)
    }

    fn seed(&mut self) {
        let split = self.is_split();
        let connection = &mut self.runner.connection;
        if split {
            diesel::insert_into(t_rls::table)
                .values((t_rls::id.eq(1), t_rls::owner.eq("a"), t_rls::secret.eq("s")))
                .execute(connection)
        } else {
            diesel::insert_into(t::table)
                .values((t::id.eq(1), t::owner.eq("a"), t::secret.eq("s")))
                .execute(connection)
        }
        .expect("seed the row");
    }

    fn stored(&mut self) -> Vec<Stored> {
        let split = self.is_split();
        let connection = &mut self.runner.connection;
        if split {
            t_rls::table.order(t_rls::id).load(connection)
        } else {
            t::table.order(t::id).load(connection)
        }
        .expect("read the storage")
    }

    /// Writes a row past the view, as the owner's load does.
    fn store(&mut self, (id, owner, secret): (i32, &str, &str)) {
        diesel::insert_into(t_rls::table)
            .values((t_rls::id.eq(id), t_rls::owner.eq(owner), t_rls::secret.eq(secret)))
            .execute(&mut self.runner.connection)
            .expect("store the row");
    }
}

/// One statement with what each engine answers. `replica` is `None` where the
/// replica answers as PostgreSQL does, which is everywhere but the documented
/// divergences.
struct Step {
    statement: Statement,
    postgres: Answer,
    replica: Option<Answer>,
}

const fn agree(statement: Statement, postgres: Answer) -> Step {
    Step { statement, postgres, replica: None }
}

const fn diverge(statement: Statement, postgres: Answer, replica: Answer) -> Step {
    Step { statement, postgres, replica: Some(replica) }
}

/// Runs `steps` as `acting` under `grants` on both engines, with and without
/// row-level security, comparing every answer and, after every step both
/// engines answer alike, the storage.
fn assert_parity(grants: &str, acting: &str, steps: &[Step]) {
    for row_security in [false, true] {
        let mut postgres = Source::new(grants, row_security, acting);
        let mut replica = Replica::new(grants, row_security, acting);
        for step in steps {
            let context =
                format!("{:?} as {acting} under {grants:?}, RLS {row_security}", step.statement);
            assert_eq!(postgres.run(step.statement), step.postgres, "PostgreSQL, {context}");
            let expected = step.replica.as_ref().unwrap_or(&step.postgres);
            assert_eq!(&replica.runner.run(step.statement), expected, "the replica, {context}");
            if step.replica.is_none() {
                assert_eq!(replica.stored(), postgres.stored(), "the storage, {context}");
            }
        }
    }
}

/// PostgreSQL 18.6 applies a policy reading a column the role cannot read,
/// without checking the role's privilege on it. The replica's view filters on
/// the backing table, and its write triggers read such a column from the
/// backing row.
#[test]
fn a_policy_reads_a_column_the_role_cannot() {
    let grants = "CREATE POLICY t_secret ON t AS RESTRICTIVE USING (secret IS DISTINCT FROM \
                  'hidden') WITH CHECK (secret IS DISTINCT FROM 'bad');
                  GRANT SELECT (id, owner), INSERT (id, owner), UPDATE (owner), DELETE ON t TO \
                  alice;";
    let hidden = (2, "b", "hidden");
    let mut postgres = Source::new(grants, true, "alice");
    let mut replica = Replica::new(grants, true, "alice");
    // A role switch has no typed DSL form.
    diesel::sql_query("RESET ROLE").execute(&mut postgres.runner.connection).expect("reset");
    diesel::insert_into(t::table)
        .values((t::id.eq(hidden.0), t::owner.eq(hidden.1), t::secret.eq(hidden.2)))
        .execute(&mut postgres.runner.connection)
        .expect("store the hidden row");
    postgres.act();
    replica.store(hidden);
    for (statement, answer) in [
        (Statement::SelectReadable, rows(&[(1, "a")])),
        (Statement::UpdateOwner(2, "z"), Answer::Wrote),
        (Statement::UpdateOwner(1, "z"), Answer::Wrote),
        (Statement::Delete(2), Answer::Wrote),
        (Statement::InsertReadable(3, "c"), Answer::Wrote),
        (Statement::SelectReadable, rows(&[(1, "z"), (3, "c")])),
        (Statement::Delete(1), Answer::Wrote),
    ] {
        assert_eq!(postgres.run(statement), answer, "PostgreSQL, {statement:?}");
        assert_eq!(replica.runner.run(statement), answer, "the replica, {statement:?}");
        assert_eq!(replica.stored(), postgres.stored(), "the storage, {statement:?}");
    }
}

/// The measured table: PostgreSQL 18.6 with `SELECT (id, owner)`,
/// `UPDATE (owner)` and `INSERT (id, owner)` on `t (id, owner, secret)`
/// answers each statement as listed. `SELECT *` is the one the replica answers
/// differently, with the readable columns, as decided.
#[test]
fn column_grants_gate_each_column() {
    assert_parity(
        "GRANT SELECT (id, owner), INSERT (id, owner), UPDATE (owner) ON t TO alice;",
        "alice",
        &[
            agree(Statement::SelectReadable, rows(&[(1, "a")])),
            diverge(Statement::SelectStar, refused(DENIED), rows(&[(1, "a")])),
            agree(Statement::UpdateOwner(1, "b"), Answer::Wrote),
            diverge(
                Statement::UpdateSecret(1, "x"),
                refused(DENIED),
                refused("no such column: secret"),
            ),
            agree(Statement::InsertReadable(2, "c"), Answer::Wrote),
            diverge(
                Statement::InsertSecret(3, "c", Some("z")),
                refused(DENIED),
                refused("table t has no column named secret"),
            ),
            agree(Statement::Delete(1), refused(DENIED)),
            agree(Statement::SelectReadable, rows(&[(1, "b"), (2, "c")])),
        ],
    );
}

#[test]
fn a_table_grant_of_insert_alone_refuses_update_and_delete() {
    assert_parity(
        "GRANT SELECT, INSERT ON t TO alice;",
        "alice",
        &[
            agree(Statement::InsertReadable(2, "c"), Answer::Wrote),
            agree(Statement::UpdateOwner(1, "b"), refused(DENIED)),
            agree(Statement::Delete(1), refused(DENIED)),
            agree(Statement::SelectStar, rows(&[(1, "a"), (2, "c")])),
        ],
    );
}

#[test]
fn a_table_grant_of_update_without_delete_refuses_delete_and_insert() {
    assert_parity(
        "GRANT SELECT, UPDATE ON t TO alice;",
        "alice",
        &[
            agree(Statement::UpdateOwner(1, "b"), Answer::Wrote),
            agree(Statement::UpdateSecret(1, "x"), Answer::Wrote),
            agree(Statement::Delete(1), refused(DENIED)),
            agree(Statement::InsertReadable(2, "c"), refused(DENIED)),
        ],
    );
}

#[test]
fn a_table_grant_of_delete_alone_refuses_insert_and_update() {
    assert_parity(
        "GRANT SELECT, DELETE ON t TO alice;",
        "alice",
        &[
            agree(Statement::InsertReadable(2, "c"), refused(DENIED)),
            agree(Statement::UpdateOwner(1, "b"), refused(DENIED)),
            agree(Statement::Delete(1), Answer::Wrote),
            agree(Statement::SelectReadable, rows(&[])),
        ],
    );
}

#[test]
fn a_column_update_beside_a_table_select_gates_the_other_columns() {
    assert_parity(
        "GRANT SELECT ON t TO alice; GRANT UPDATE (owner) ON t TO alice;",
        "alice",
        &[
            agree(Statement::SelectStar, rows(&[(1, "a")])),
            agree(Statement::UpdateOwner(1, "b"), Answer::Wrote),
            agree(Statement::UpdateSecret(1, "x"), refused(DENIED)),
            agree(Statement::InsertReadable(2, "c"), refused(DENIED)),
            agree(Statement::Delete(1), refused(DENIED)),
        ],
    );
}

/// A readable column the role may not insert arrives `NULL` from an omitted
/// list entry and from an explicit `NULL` alike, so the replica refuses only a
/// value and stores the default for the explicit `NULL` PostgreSQL refuses.
#[test]
fn a_column_insert_beside_a_table_select_refuses_a_value_for_the_other_columns() {
    assert_parity(
        "GRANT SELECT ON t TO alice; GRANT INSERT (id, owner) ON t TO alice;",
        "alice",
        &[
            agree(Statement::InsertReadable(2, "c"), Answer::Wrote),
            agree(Statement::InsertSecret(3, "c", Some("z")), refused(DENIED)),
            diverge(Statement::InsertSecret(4, "d", None), refused(DENIED), Answer::Wrote),
        ],
    );
}

#[test]
fn a_column_insert_of_the_key_alone_refuses_a_value_for_every_other_column() {
    assert_parity(
        "GRANT SELECT ON t TO alice; GRANT INSERT (id) ON t TO alice;",
        "alice",
        &[
            agree(Statement::InsertId(2), Answer::Wrote),
            agree(Statement::InsertReadable(3, "c"), refused(DENIED)),
            agree(Statement::InsertSecret(4, "d", Some("z")), refused(DENIED)),
            agree(Statement::SelectStar, Answer::Rows(vec![(1, Some("a".to_owned())), (2, None)])),
        ],
    );
}

#[test]
fn a_grant_reaches_the_acting_role_through_an_inherited_role() {
    let mut postgres = postgres_harness::fresh_database();
    // Role membership has no typed DSL form, and is idempotent cluster-wide.
    postgres_harness::apply(&mut postgres, "GRANT privilege_group TO privilege_member;")
        .expect("make privilege_member a member of privilege_group");
    assert_parity(
        "GRANT SELECT (id, owner), UPDATE (owner) ON t TO privilege_group;",
        "privilege_member",
        &[
            agree(Statement::SelectReadable, rows(&[(1, "a")])),
            agree(Statement::UpdateOwner(1, "b"), Answer::Wrote),
            agree(Statement::InsertReadable(2, "c"), refused(DENIED)),
            agree(Statement::Delete(1), refused(DENIED)),
        ],
    );
}

#[test]
fn a_grant_to_public_reaches_every_role() {
    assert_parity(
        "GRANT SELECT (id, owner), DELETE ON t TO PUBLIC;",
        "alice",
        &[
            agree(Statement::SelectReadable, rows(&[(1, "a")])),
            agree(Statement::UpdateOwner(1, "b"), refused(DENIED)),
            agree(Statement::Delete(1), Answer::Wrote),
        ],
    );
}

#[test]
fn a_superuser_holds_every_privilege() {
    assert_parity(
        "",
        "postgres",
        &[
            agree(Statement::SelectStar, rows(&[(1, "a")])),
            agree(Statement::UpdateSecret(1, "x"), Answer::Wrote),
            agree(Statement::InsertSecret(2, "c", Some("z")), Answer::Wrote),
            agree(Statement::Delete(1), Answer::Wrote),
        ],
    );
}

/// PostgreSQL 18.6 lets a role write a column it may not read: with
/// `SELECT (id, owner)`, `INSERT (id, owner, secret)` and
/// `UPDATE (owner, secret)`, `INSERT INTO t (id, owner, secret) VALUES
/// (2, 'b', 'w')` inserts and `UPDATE t SET secret = 'x' WHERE id = 1`
/// updates. The replica's view holds only the readable columns, so it refuses
/// both.
#[test]
fn a_column_the_role_may_write_but_not_read_is_refused() {
    let grants = "GRANT SELECT (id, owner), INSERT (id, owner, secret), UPDATE (owner, secret) ON t \
                  TO alice;";
    for row_security in [false, true] {
        let mut postgres = Source::new(grants, row_security, "alice");
        let mut replica = Replica::new(grants, row_security, "alice");
        let insert = Statement::InsertSecret(2, "b", Some("w"));
        assert_eq!(postgres.run(insert), Answer::Wrote);
        assert_eq!(replica.runner.run(insert), refused("table t has no column named secret"));
        let update = Statement::UpdateSecret(1, "x");
        assert_eq!(postgres.run(update), Answer::Wrote);
        assert_eq!(replica.runner.run(update), refused("no such column: secret"));
        assert_eq!(
            replica.stored(),
            vec![(1, Some("a".to_owned()), Some("s".to_owned()))],
            "the replica wrote nothing, RLS {row_security}"
        );
    }
}

/// PostgreSQL checks privileges before it reads a row, so a refused command
/// that matches nothing is still refused. The replica's gates fire per row, so
/// there the same command changes nothing and succeeds.
#[test]
fn a_refused_write_matching_no_row_succeeds_with_zero_rows() {
    let grants = "GRANT SELECT ON t TO alice; GRANT UPDATE (owner) ON t TO alice;";
    for row_security in [false, true] {
        let mut postgres = Source::new(grants, row_security, "alice");
        let mut replica = Replica::new(grants, row_security, "alice");
        for statement in [
            Statement::Delete(999),
            Statement::UpdateSecret(999, "x"),
            Statement::InsertSelected(999),
        ] {
            assert_eq!(postgres.run(statement), refused(DENIED), "PostgreSQL, {statement:?}");
            assert_eq!(replica.runner.run(statement), Answer::Wrote, "the replica, {statement:?}");
            assert_eq!(replica.stored(), postgres.stored(), "nothing changed, {statement:?}");
        }
    }
}

/// The replica finds the row an `UPDATE` or `DELETE` names by its primary key,
/// read through the view. A role that cannot read the key reads as PostgreSQL
/// lets it, and the replica refuses its updates and deletes by name, with a
/// warning when it translates.
#[test]
fn a_role_that_cannot_read_the_key_cannot_update_or_delete_on_the_replica() {
    let grants = "GRANT SELECT (owner), UPDATE (owner), DELETE ON t TO alice;";
    let refusal = "cannot update or delete a row of t on the replica, since the acting role \
                   cannot read its key column id";
    for row_security in [false, true] {
        let mut postgres = Source::new(grants, row_security, "alice");
        let mut replica = Replica::new(grants, row_security, "alice");
        let warning = replica
            .warnings
            .iter()
            .find(|warning| matches!(warning, TranslationWarning::UnreadableRowKey { .. }))
            .unwrap_or_else(|| panic!("the translation warns, RLS {row_security}"));
        assert_eq!(
            warning,
            &TranslationWarning::UnreadableRowKey {
                table: "t".to_owned(),
                column: "id".to_owned()
            }
        );
        assert_eq!(
            warning.to_string(),
            "the acting role cannot read key column id of t, so the replica refuses its updates \
             and deletes"
        );
        let owners = t::table.select(t::owner).order(t::owner);
        assert_eq!(
            owners.load::<Option<String>>(&mut replica.runner.connection).expect("read owners"),
            owners.load::<Option<String>>(&mut postgres.runner.connection).expect("read owners"),
        );
        let rename = || diesel::update(t::table.filter(t::owner.eq("a"))).set(t::owner.eq("b"));
        let remove = diesel::delete(t::table.filter(t::owner.eq("a")));
        assert_eq!(
            answer(remove.execute(&mut replica.runner.connection), |_| Answer::Wrote),
            refused(refusal)
        );
        assert_eq!(
            answer(rename().execute(&mut replica.runner.connection), |_| Answer::Wrote),
            refused(refusal)
        );
        assert_eq!(
            answer(rename().execute(&mut postgres.runner.connection), |_| Answer::Wrote),
            Answer::Wrote
        );
        let remove = diesel::delete(t::table.filter(t::owner.eq("b")));
        assert_eq!(
            answer(remove.execute(&mut postgres.runner.connection), |_| Answer::Wrote),
            Answer::Wrote
        );
        assert_eq!(
            replica.stored(),
            vec![(1, Some("a".to_owned()), Some("s".to_owned()))],
            "the replica wrote nothing, RLS {row_security}"
        );
    }
}
