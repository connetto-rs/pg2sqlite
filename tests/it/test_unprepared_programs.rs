//! Every trigger and view a loaded connection holds that SQLite refuses to
//! prepare is listed, and nothing SQLite prepares.
//!
//! SQLite refuses to expand a `WITH` query in a program already holding
//! `SQLITE_LIMIT_EXPR_DEPTH` table cursors, and every call site of a helper
//! is written as its own copy of the helper's set, so a schema whose helpers
//! call one another repeatedly loads and then fails on the first write.

use core::fmt::Write as _;

use diesel::{RunQueryDsl, connection::SimpleConnection};
use pg2sqlite::{
    prelude::Pg2SqliteOptions,
    programs::{ProgramsError, UnpreparedProgram, unprepared_programs},
};
use sql_traits::structs::{Deployment, SessionSetting, SettingValue};
use sqlparser::ast::DataType;

use crate::helpers;

/// Helpers `ids_0` to `ids_<levels>`, the first walking the caller's items
/// down the hierarchy and each next one calling the previous one twice, in
/// arms keeping rows on different conditions, so its set is read once per
/// arm, with the update policy calling the last `calls` times.
fn walk_chain(levels: usize, calls: usize) -> String {
    helper_chain(levels, calls, |previous| {
        format!(
            "SELECT item.id FROM public.items item WHERE item.id IN (SELECT public.ids_{previous}()) AND item.value = 0
  UNION
  SELECT item.id FROM public.items item WHERE item.parent_id IN (SELECT public.ids_{previous}())"
        )
    })
}

/// The answer of a helper reading `ids_<previous>` twice, as its rows and as
/// their parents.
fn doubling(previous: usize) -> String {
    format!(
        "SELECT item.id FROM public.items item WHERE item.id IN (SELECT public.ids_{previous}())
  UNION
  SELECT item.id FROM public.items item WHERE item.parent_id IN (SELECT public.ids_{previous}())"
    )
}

/// Helpers `ids_0` to `ids_<levels>`, the first walking the caller's items
/// down the hierarchy and each next one answering `body(previous)`, with the
/// update policy calling the last `calls` times.
fn helper_chain(levels: usize, calls: usize, body: impl Fn(usize) -> String) -> String {
    let using = vec![format!("id IN (SELECT ids_{levels}())"); calls].join(" OR ");
    chain_schema(
        "ids",
        "SETOF UUID",
        "WITH RECURSIVE reached (id) AS (
    SELECT item.id FROM public.items item WHERE item.owner_id = NULLIF(current_setting('app.user_id', true), '')::uuid
    UNION
    SELECT item.id FROM public.items item JOIN reached ON item.parent_id = reached.id
  )
  SELECT reached.id FROM reached",
        levels,
        body,
        &using,
    )
}

/// The items schema with helpers `<name>_0` to `<name>_<levels>` returning
/// `returns`, the first answering `first` and each next one answering
/// `body(previous)`, and an update policy using `using`.
fn chain_schema(
    name: &str,
    returns: &str,
    first: &str,
    levels: usize,
    body: impl Fn(usize) -> String,
    using: &str,
) -> String {
    let mut sql = String::from(
        "CREATE ROLE app;
CREATE ROLE authenticator LOGIN NOINHERIT;
GRANT app TO authenticator;
CREATE ROLE helper_owner NOLOGIN BYPASSRLS;
CREATE TABLE items (id UUID PRIMARY KEY, owner_id UUID NOT NULL, parent_id UUID REFERENCES items (id), value INTEGER NOT NULL);
ALTER TABLE items ENABLE ROW LEVEL SECURITY;
GRANT SELECT, UPDATE (value) ON items TO app;
GRANT SELECT ON items TO helper_owner;
",
    );
    for level in 0..=levels {
        let answer = if level == 0 { first.to_owned() } else { body(level - 1) };
        write!(
            sql,
            "CREATE FUNCTION {name}_{level}() RETURNS {returns} LANGUAGE sql STABLE SECURITY DEFINER \
             SET search_path = pg_catalog, pg_temp AS $$
  {answer}
$$;
ALTER FUNCTION {name}_{level}() OWNER TO helper_owner;
"
        )
        .expect("writing to a string");
    }
    write!(
        sql,
        "CREATE POLICY read_items ON items FOR SELECT TO app USING (true);
CREATE POLICY update_items ON items FOR UPDATE TO app USING ({using});
"
    )
    .expect("writing to a string");
    sql
}

fn options() -> Pg2SqliteOptions {
    let caller = SessionSetting::value("app.user_id", SettingValue::Caller(DataType::Uuid))
        .expect("a custom setting name");
    let deployment =
        Deployment::new("authenticator", ["app"], [caller]).expect("a valid deployment");
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_uuid_representation(pg2sqlite::prelude::UuidRepresentation::Blob)
        .with_session_variable(pg2sqlite::prelude::SessionVariableMapping::new(
            "app.user_id",
            "current_app_user",
        ))
        .with_deployment(deployment)
        .with_acting_role("app")
}

/// Translates `script(count)` for each of `counts`, loads it, lists the
/// programs SQLite refuses to prepare, and runs `firing`, which prepares
/// `program`.
///
/// Asserts that `program` is listed exactly when SQLite refuses `firing`,
/// and that the sweep reaches both sides of the limit.
fn sweep(
    script: impl Fn(usize) -> String,
    counts: impl IntoIterator<Item = usize>,
    firing: &str,
    program: &str,
) {
    let mut refused = None;
    let mut accepted = None;
    for count in counts {
        let translated = helpers::translate_pg(&script(count), &options())
            .unwrap_or_else(|error| panic!("{count} fails to translate: {error}"));
        let mut connection = helpers::establish_connection();
        connection.batch_execute(&translated.join(";\n")).expect("the script loads");
        let unprepared = unprepared_programs(&mut connection).expect("the schema reads");
        let listed = unprepared.iter().any(|unprepared| lists(unprepared, program));
        // The statement a client sends, written as text the way a client sends
        // it.
        match diesel::sql_query(firing).execute(&mut connection) {
            Ok(_) => {
                assert!(!listed, "{count} lists {program}, which SQLite prepares: {unprepared:?}");
                assert!(refused.is_none(), "{count} prepares past a refusal at {refused:?}");
                accepted = Some(count);
            }
            Err(error) => {
                assert!(
                    listed,
                    "{count} leaves out {program}, which fails with {error}: {unprepared:?}"
                );
                refused = refused.or(Some(count));
            }
        }
    }
    assert!(
        accepted.is_some() && refused.is_some(),
        "the sweep crosses the limit, accepted up to {accepted:?}, refused from {refused:?}"
    );
}

/// Whether `unprepared` names `program` among the programs it compiles.
fn lists(unprepared: &UnpreparedProgram, program: &str) -> bool {
    unprepared.programs.iter().any(|name| name == program)
}

#[test]
fn a_policy_trigger_is_listed_exactly_when_sqlite_refuses_it() {
    sweep(
        |calls| walk_chain(1, calls),
        [1, 8, 16, 24, 32, 48, 64],
        "UPDATE items SET value = 1",
        "items_update_trigger",
    );
}

/// `walk_chain(levels, 1)` and a view reading `ids_<levels>` once.
fn reaching_view(levels: usize) -> String {
    format!(
        "{}CREATE VIEW reached WITH (security_invoker = true) AS SELECT id FROM items WHERE id IN (SELECT ids_{levels}());
GRANT SELECT ON reached TO app;
",
        walk_chain(levels, 1)
    )
}

#[test]
fn a_view_is_listed_exactly_when_sqlite_refuses_to_read_it() {
    sweep(reaching_view, 1..=8, "SELECT count(*) FROM reached", "reached");
}

/// Views `v0` to `v<levels>` over `t`, `v0` reading a `WITH` query and each
/// next one reading the previous one twice, so SQLite expands `2^levels`
/// copies of it.
fn doubling_views(levels: usize) -> String {
    let mut sql = String::from(
        "CREATE TABLE t (x);
CREATE VIEW v0 AS WITH c (x) AS (SELECT x FROM t) SELECT x FROM c;
",
    );
    for level in 1..=levels {
        let previous = level - 1;
        writeln!(
            sql,
            "CREATE VIEW v{level} AS SELECT x FROM v{previous} UNION ALL SELECT x FROM v{previous};"
        )
        .expect("writing to a string");
    }
    sql
}

#[test]
fn every_refused_program_is_listed_with_the_write_compiling_it() {
    let mut connection = helpers::establish_connection();
    connection
        .batch_execute(&format!(
            "{}CREATE TABLE a (x);
CREATE TRIGGER a_deleted AFTER DELETE ON a BEGIN SELECT x FROM v10; END;
CREATE TRIGGER a_inserted AFTER INSERT ON a BEGIN SELECT x FROM v1; END;
CREATE TABLE b (x, y, z AS (x + 1));
CREATE TRIGGER \"update\" AFTER INSERT ON b BEGIN SELECT x FROM v10; END;
CREATE TRIGGER b_updated AFTER UPDATE OF y ON b BEGIN SELECT x FROM v10; END;
CREATE TRIGGER \"q\"\" AFTER INSERT\" -- AFTER INSERT
 BEFORE DELETE ON b BEGIN SELECT x FROM v10; END;
CREATE TRIGGER 's AFTER INSERT' /* INSERT */ DELETE ON b BEGIN SELECT x FROM v10; END;
CREATE TRIGGER `t AFTER INSERT` AFTER DELETE ON b BEGIN SELECT x FROM v10; END;
CREATE TRIGGER [u AFTER INSERT] AFTER DELETE ON b BEGIN SELECT x FROM v10; END;
CREATE VIEW w AS SELECT x FROM t;
CREATE TRIGGER w_inserted INSTEAD OF INSERT ON w BEGIN INSERT INTO t VALUES (NEW.x); END;
CREATE TEMP TRIGGER /* DELETE */ t_updated AFTER UPDATE OF x ON t BEGIN SELECT x FROM v10; END;
CREATE VIEW deep AS SELECT x FROM v10;
CREATE TRIGGER deep_updated INSTEAD OF UPDATE ON deep BEGIN SELECT 1; END;
",
            doubling_views(10)
        ))
        .expect("the script loads");
    let unprepared = unprepared_programs(&mut connection).expect("the schema reads");
    let mut listed: Vec<(&str, Vec<&str>)> = unprepared
        .iter()
        .map(|unprepared| {
            (
                unprepared.statement.as_str(),
                unprepared.programs.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    listed.sort_unstable();
    let view_reads: Vec<(String, String)> = ["v8", "v9", "v10", "deep"]
        .into_iter()
        .map(|view| (format!("SELECT * FROM \"main\".\"{view}\""), view.to_owned()))
        .collect();
    let mut expected: Vec<(&str, Vec<&str>)> = vec![
        ("DELETE FROM \"main\".\"a\"", vec!["a_deleted"]),
        ("INSERT INTO \"main\".\"b\" DEFAULT VALUES", vec!["update"]),
        (
            "DELETE FROM \"main\".\"b\"",
            vec!["q\" AFTER INSERT", "s AFTER INSERT", "t AFTER INSERT", "u AFTER INSERT"],
        ),
        ("UPDATE \"main\".\"b\" SET \"x\" = \"x\", \"y\" = \"y\"", vec!["b_updated"]),
        ("UPDATE \"main\".\"t\" SET \"x\" = \"x\"", vec!["t_updated"]),
        ("UPDATE \"main\".\"deep\"", vec!["deep_updated"]),
    ];
    expected.extend(view_reads.iter().map(|(read, view)| (read.as_str(), vec![view.as_str()])));
    expected.sort_unstable();
    assert_eq!(listed, expected);
    for unprepared in &unprepared {
        let shown = unprepared.to_string();
        assert!(
            shown.contains(&unprepared.statement) && shown.contains("nested too deep"),
            "the statement is shown with SQLite's own refusal: {shown}"
        );
    }
}

#[test]
fn a_trigger_not_stored_as_sqlite_writes_one_is_refused_by_name() {
    let mut connection = helpers::establish_connection();
    connection
        .batch_execute(
            "CREATE TABLE t (x);
PRAGMA writable_schema = ON;
INSERT INTO sqlite_schema VALUES ('trigger', 'written', 't', 0, 'create trigger written after delete on t begin select 1; end');
PRAGMA writable_schema = RESET;",
        )
        .expect("the schema is written");
    let error = unprepared_programs(&mut connection).expect_err("the trigger is not read");
    assert!(
        matches!(&error, ProgramsError::UnreadTrigger { schema, name } if schema == "main" && name == "written"),
        "{error:?}"
    );
}

#[test]
fn a_deep_helper_chain_translates_on_a_default_thread() {
    assert_translates(&walk_chain(7, 1));
}

const CHAIN_DEPTH: usize = 100;

/// Translates `sql`, failing the test when translation errors.
fn assert_translates(sql: &str) {
    helpers::translate_pg(sql, &options())
        .unwrap_or_else(|error| panic!("the chain translates: {error}"));
}

#[test]
fn a_helper_chain_read_through_from_translates_on_a_default_thread() {
    assert_translates(&helper_chain(CHAIN_DEPTH, 1, |previous| {
        format!("SELECT x.id FROM public.ids_{previous}() AS x(id)")
    }));
}

#[test]
fn a_helper_chain_read_through_in_translates_on_a_default_thread() {
    assert_translates(&helper_chain(CHAIN_DEPTH, 1, |previous| {
        format!(
            "SELECT item.id FROM public.items item WHERE item.id IN (SELECT public.ids_{previous}())"
        )
    }));
}

#[test]
fn a_helper_chain_read_through_a_scalar_subquery_translates_on_a_default_thread() {
    assert_translates(&chain_schema(
        "me",
        "UUID",
        "SELECT NULLIF(current_setting('app.user_id', true), '')::uuid",
        CHAIN_DEPTH,
        |previous| format!("SELECT (SELECT public.me_{previous}())"),
        &format!("owner_id = me_{CHAIN_DEPTH}()"),
    ));
}

/// Helpers `ids_0` to `ids_<levels>`, the first answering the root items and
/// each next one calling the previous one twice, so `ids_<level>` holds the
/// items at most `level` levels below a root.
fn doubled_chain(levels: usize) -> String {
    chain_schema(
        "ids",
        "SETOF UUID",
        "SELECT item.id FROM public.items item WHERE item.parent_id IS NULL",
        levels,
        doubling,
        &format!("id IN (SELECT ids_{levels}())"),
    )
}

#[test]
fn a_helper_reading_another_twice_grows_the_script_linearly() {
    let bytes = |levels| {
        helpers::translate_pg(&doubled_chain(levels), &options())
            .unwrap_or_else(|error| panic!("{levels} levels translate: {error}"))
            .iter()
            .map(String::len)
            .sum::<usize>()
    };
    let (half, full) = (bytes(4), bytes(8));
    assert!(full < 3 * half, "8 levels write {full} bytes against {half} for 4");
}

#[test]
fn a_helper_reading_another_twice_keeps_the_rows_postgresql_keeps() {
    const LEVELS: u8 = 8;
    let script = helpers::translate_pg(&doubled_chain(LEVELS.into()), &options())
        .unwrap_or_else(|error| panic!("the chain translates: {error}"));
    let blob = |byte: u8| format!("X'{}'", format!("{byte:02x}").repeat(16));
    let mut connection = helpers::establish_connection();
    for statement in &script {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted statement failed: {error}\n{statement}"));
        if !statement.starts_with("CREATE TABLE items_rls ") {
            continue;
        }
        // Stored before the triggers refusing writes outside a helper exist.
        // Item `depth + 1` sits `depth` levels below the root item 1.
        for depth in 0..LEVELS + 4 {
            let parent = if depth == 0 { "NULL".to_owned() } else { blob(depth) };
            let owned_by = blob(0xaa);
            diesel::sql_query(format!(
                "INSERT INTO items_rls (id, owner_id, parent_id, value) VALUES ({}, {owned_by}, {parent}, 0)",
                blob(depth + 1)
            ))
            .execute(&mut connection)
            .expect("the item is stored");
        }
    }
    helpers::set_session_user_id(&rosetta_uuid::Uuid::from([0xaa; 16]));
    diesel::sql_query("UPDATE items SET value = 1")
        .execute(&mut connection)
        .expect("the caller updates the items it reaches");
    let updated = diesel::sql_query("SELECT count(*) AS count FROM items_rls WHERE value = 1")
        .get_result::<helpers::Count>(&mut connection)
        .expect("the items are counted");
    assert_eq!(updated.count, i64::from(LEVELS) + 1, "ids_{LEVELS} reaches {LEVELS} levels down");
}
