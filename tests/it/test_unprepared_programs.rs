//! Every trigger, view and helper function body a loaded connection holds that
//! SQLite refuses to prepare is listed, and nothing SQLite prepares.
//!
//! SQLite refuses to expand a `WITH` query in a program already holding
//! `SQLITE_LIMIT_EXPR_DEPTH` table cursors. A helper PostgreSQL inlines is
//! written at each call site as its own copy of the helper's set, so a schema
//! whose inlined helpers read one another repeatedly loads and then fails on
//! the first write. A helper PostgreSQL calls is a table-valued function,
//! costing its call site one cursor, with its body a program of its own.

use core::fmt::Write as _;

use diesel::{RunQueryDsl, connection::SimpleConnection};
use pg2sqlite::{
    pg2sqlite::Pg2Sqlite,
    prelude::{Error, HelperFunction, Pg2SqliteOptions, Script},
    programs::{ProgramsError, UnpreparedProgram, unprepared_programs},
};
use sql_traits::structs::{Deployment, SessionSetting, SettingValue};
use sqlparser::ast::DataType;

use crate::helpers;

/// `SECURITY DEFINER` helpers `ids_0` to `ids_<levels>`, the first walking
/// the caller's items down the hierarchy and each next one calling the
/// previous one twice, in arms keeping rows on different conditions, so its
/// set is read once per arm, with the update policy calling the last `calls`
/// times.
fn walk_chain(levels: usize, calls: usize) -> String {
    helper_chain(levels, calls, walk)
}

/// The items below the items `ids_<previous>` answers, calling it twice in
/// arms keeping rows on different conditions.
fn walk(previous: usize) -> String {
    format!(
        "SELECT item.id FROM public.items item WHERE item.id IN (SELECT public.ids_{previous}()) AND item.value = 0
  UNION
  SELECT item.id FROM public.items item WHERE item.parent_id IN (SELECT public.ids_{previous}())"
    )
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

/// Invoker helpers `ids_0` to `ids_<levels>` that PostgreSQL inlines, the
/// first walking the caller's items down the hierarchy and each next one
/// reading the previous one twice, with the update policy reading the last
/// `calls` times.
fn inlined_walk_chain(levels: usize, calls: usize) -> String {
    let using =
        vec![format!("id IN (SELECT x.id FROM ids_{levels}() AS x(id))"); calls].join(" OR ");
    chain_schema("ids", "SETOF UUID", WALK, levels, inlined_walk, "", &using)
}

/// The items below the items `ids_<previous>` answers, read twice through
/// `FROM`, where PostgreSQL inlines an invoker helper.
fn inlined_walk(previous: usize) -> String {
    format!(
        "SELECT item.id FROM public.items item JOIN public.ids_{previous}() AS x(id) ON item.id = x.id
  UNION
  SELECT item.id FROM public.items item JOIN public.ids_{previous}() AS x(id) ON item.parent_id = x.id"
    )
}

/// The caller's items and every item below them.
const WALK: &str = "WITH RECURSIVE reached (id) AS (
    SELECT item.id FROM public.items item WHERE item.owner_id = NULLIF(current_setting('app.user_id', true), '')::uuid
    UNION
    SELECT item.id FROM public.items item JOIN reached ON item.parent_id = reached.id
  )
  SELECT reached.id FROM reached";

/// The attributes of a `SECURITY DEFINER` helper pinning its search path,
/// which PostgreSQL calls.
const DEFINER: &str = "SECURITY DEFINER SET search_path = pg_catalog, pg_temp";

/// `SECURITY DEFINER` helpers `ids_0` to `ids_<levels>`, the first walking
/// the caller's items down the hierarchy and each next one answering
/// `body(previous)`, with the update policy calling the last `calls` times.
fn helper_chain(levels: usize, calls: usize, body: impl Fn(usize) -> String) -> String {
    let using = vec![format!("id IN (SELECT ids_{levels}())"); calls].join(" OR ");
    chain_schema("ids", "SETOF UUID", WALK, levels, body, DEFINER, &using)
}

/// The items schema with `STABLE` helpers `<name>_0` to `<name>_<levels>`
/// declared with `attributes` and returning `returns`, the first answering
/// `first` and each next one answering `body(previous)`, and an update policy
/// using `using`.
fn chain_schema(
    name: &str,
    returns: &str,
    first: &str,
    levels: usize,
    body: impl Fn(usize) -> String,
    attributes: &str,
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
            "CREATE FUNCTION {name}_{level}() RETURNS {returns} LANGUAGE sql STABLE {attributes} AS $$
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
        let translated = helpers::translate_script(&script(count), &options())
            .unwrap_or_else(|error| panic!("{count} fails to translate: {error}"));
        let mut connection = loaded(None, &translated);
        let unprepared =
            unprepared_programs(&mut connection, &translated.functions).expect("the schema reads");
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

/// A connection registering the helper functions of `translated` and
/// holding `held` then `translated`.
fn loaded(held: Option<&Script<String>>, translated: &Script<String>) -> diesel::SqliteConnection {
    let mut connection = helpers::establish_connection();
    translated.register_functions(&mut connection).expect("register helper functions");
    for script in held.into_iter().chain([translated]) {
        connection
            .batch_execute(&script.iter().map(String::as_str).collect::<Vec<_>>().join(";\n"))
            .expect("the script loads");
    }
    connection
}

/// Whether `unprepared` names `program` among the programs it compiles.
fn lists(unprepared: &UnpreparedProgram, program: &str) -> bool {
    unprepared.programs.iter().any(|name| name == program)
}

#[test]
fn a_policy_trigger_is_listed_exactly_when_sqlite_refuses_it() {
    sweep(
        |calls| inlined_walk_chain(1, calls),
        [1, 8, 16, 24, 32, 48, 64],
        "UPDATE items SET value = 1",
        "items_update_trigger",
    );
}

/// `inlined_walk_chain(levels, 1)` and a view reading `ids_<levels>` once.
fn reaching_view(levels: usize) -> String {
    format!(
        "{}CREATE VIEW reached WITH (security_invoker = true) AS SELECT id FROM items WHERE id IN (SELECT x.id FROM ids_{levels}() AS x(id));
GRANT SELECT ON reached TO app;
",
        inlined_walk_chain(levels, 1)
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
    let unprepared = unprepared_programs::<String>(&mut connection, &[]).expect("the schema reads");
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

/// Each call of a helper PostgreSQL calls costs its program one cursor,
/// however many sets the helper's body reads.
#[test]
fn a_called_helper_chain_prepares_however_deep_or_often_called() {
    for sql in [walk_chain(7, 1), walk_chain(1, 64)] {
        let script = helpers::translate_script(&sql, &options())
            .unwrap_or_else(|error| panic!("the chain translates: {error}"));
        let mut connection = loaded(None, &script);
        let unprepared =
            unprepared_programs(&mut connection, &script.functions).expect("the schema reads");
        assert!(unprepared.is_empty(), "SQLite refuses a program: {unprepared:?}");
        diesel::sql_query("UPDATE items SET value = 1")
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("SQLite refuses the update: {error}"));
    }
}

/// The translation of a database holding `tallies` and `reached`, a view
/// calling the helper function of `ids_2`, which no policy calls, and of
/// `migration` over it.
fn migrated(migration: &str) -> (Script<String>, Script<String>) {
    let starting = format!(
        "{}CREATE TABLE tallies (id INTEGER PRIMARY KEY);
GRANT SELECT ON tallies TO app;
CREATE VIEW reached WITH (security_invoker = true) AS SELECT id FROM items WHERE id IN (SELECT ids_2());
GRANT SELECT ON reached TO app;
",
        chain_schema("ids", "SETOF UUID", WALK, 2, walk, DEFINER, "true")
    );
    let source = Pg2Sqlite::default().sql(&starting).expect("the starting schema parses");
    let schema = source.build_schema().expect("the starting schema builds");
    let held = source.translate_to_sql(&options()).expect("the starting schema translates");
    let migration = Pg2Sqlite::default()
        .sql(migration)
        .expect("the migration parses")
        .starting_functions(&held.functions)
        .expect("the starting functions parse as SQLite")
        .translate_to_sql_with_schema(&schema, &options())
        .expect("the migration translates");
    (held, migration)
}

/// A view the database holds keeps reading the helper function it calls on
/// a connection registering the functions of the last migration alone.
#[test]
fn a_migration_keeps_the_helper_functions_its_held_views_call() {
    let (held, migration) = migrated("ALTER TABLE tallies ADD COLUMN v INTEGER;");
    let mut connection = loaded(Some(&held), &migration);
    diesel::sql_query("SELECT count(*) FROM reached")
        .execute(&mut connection)
        .unwrap_or_else(|error| panic!("SQLite refuses the held view: {error}"));
}

/// SQLite reparses every view on a table rename, so a view the migration
/// drops afterwards still needs its helper function then.
#[test]
fn a_migration_keeps_the_helper_functions_of_a_view_it_drops_after_a_rename() {
    let (held, migration) = migrated(
        "ALTER TABLE tallies RENAME TO counts;
DROP VIEW reached;",
    );
    let mut connection = loaded(Some(&held), &migration);
    diesel::sql_query("SELECT count(*) FROM counts")
        .execute(&mut connection)
        .unwrap_or_else(|error| panic!("SQLite refuses the migration: {error}"));
}

/// A malformed starting helper reports the body that failed to parse.
#[test]
fn a_starting_helper_with_invalid_sql_is_refused() {
    let body = "SELECT (";
    let function = HelperFunction {
        name: "held_helper".to_owned(),
        parameters: Vec::new(),
        columns: vec!["value".to_owned()],
        body,
    };
    let Err(Error::SqlParse(error)) = Pg2Sqlite::default().starting_functions(&[function]) else {
        panic!("invalid helper SQL must produce a parse error");
    };
    assert_eq!(error.input(), body);
}

/// A starting helper must contain exactly one statement.
#[test]
fn a_starting_helper_with_multiple_statements_is_refused() {
    let body = "SELECT 1; SELECT 2;";
    let function = HelperFunction {
        name: "held_helper".to_owned(),
        parameters: Vec::new(),
        columns: vec!["value".to_owned()],
        body,
    };
    let Err(Error::SqlParse(error)) = Pg2Sqlite::default().starting_functions(&[function]) else {
        panic!("multiple helper statements must produce a parse error");
    };
    assert_eq!(error.input(), body);
    assert!(error.to_string().contains("a helper function body is one statement"));
}

/// A helper emitted by the translation is retained only once when also supplied
/// as a starting helper.
#[test]
fn a_starting_helper_already_emitted_is_retained_once() {
    let source = Pg2Sqlite::default().sql(&walk_chain(0, 1)).expect("the source parses");
    let held = source.translate_to_sql(&options()).expect("the source translates");
    assert_eq!(held.functions.len(), 1);
    let translated = source
        .starting_functions(&held.functions)
        .expect("the starting helpers parse")
        .translate_to_sql(&options())
        .expect("the source translates with starting helpers");
    assert_eq!(translated.functions, held.functions);
}

/// A table cannot shadow a helper function registered by the translation.
#[test]
fn a_table_colliding_with_an_emitted_helper_is_refused_by_name() {
    let source = walk_chain(0, 1);
    let translated = helpers::translate_script(&source, &options()).expect("the source translates");
    let function = translated.functions.first().expect("the policy calls a helper");
    let colliding = format!(
        "{source}\nCREATE TABLE \"{0}\" (value INTEGER);\nGRANT SELECT ON \"{0}\" TO app;",
        function.name
    );
    let error = helpers::translate_script(&colliding, &options())
        .expect_err("a table shadowing a helper must produce a name collision");
    let Error::EmittedNameCollision { name, first, second, .. } = error else {
        panic!("expected a helper name collision, got {error}");
    };
    assert_eq!(name, function.name);
    assert_eq!(first, format!("helper function {}", function.name));
    assert_eq!(second, format!("\"{}\"", function.name));
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
    let error =
        unprepared_programs::<String>(&mut connection, &[]).expect_err("the trigger is not read");
    assert!(
        matches!(&error, ProgramsError::UnreadTrigger { schema, name } if schema == "main" && name == "written"),
        "{error:?}"
    );
}

/// A helper function whose body a migration breaks is listed by name, while
/// the view calling it still prepares and fails only once read.
#[test]
fn a_helper_function_whose_body_sqlite_refuses_is_listed_by_name() {
    let function = |name: &str, relation: &str| {
        HelperFunction {
            name: name.to_owned(),
            parameters: vec!["wanted".to_owned()],
            columns: vec!["x".to_owned()],
            body: format!("SELECT x FROM {relation} WHERE x = ?1"),
        }
    };
    let functions = [function("kept", "t"), function("broken", "gone")];
    let mut connection = helpers::establish_connection();
    for function in &functions {
        function.register(&mut connection).expect("register the helper function");
    }
    connection
        .batch_execute(
            "CREATE TABLE t (x);
CREATE TABLE gone (x);
CREATE VIEW reads_kept AS SELECT x FROM kept(1);
CREATE VIEW reads_broken AS SELECT x FROM broken(1);
DROP TABLE gone;",
        )
        .expect("the schema loads");
    let unprepared = unprepared_programs(&mut connection, &functions).expect("the schema reads");
    let listed: Vec<(&str, &[String])> = unprepared
        .iter()
        .map(|unprepared| (unprepared.statement.as_str(), unprepared.programs.as_slice()))
        .collect();
    assert_eq!(listed, [("SELECT x FROM gone WHERE x = ?1", &["broken".to_owned()][..])]);
    assert!(unprepared[0].message.contains("no such table: gone"), "{unprepared:?}");
    let error = diesel::sql_query("SELECT x FROM reads_broken")
        .execute(&mut connection)
        .expect_err("the broken body fails once called");
    assert!(error.to_string().contains("no such table: gone"), "{error}");
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
        DEFINER,
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
        DEFINER,
        &format!("id IN (SELECT ids_{levels}())"),
    )
}

#[test]
fn a_helper_reading_another_twice_grows_the_script_linearly() {
    let bytes = |levels| {
        let script = helpers::translate_script(&doubled_chain(levels), &options())
            .unwrap_or_else(|error| panic!("{levels} levels translate: {error}"));
        script.iter().map(String::len).sum::<usize>()
            + script.functions.iter().map(|function| function.body.len()).sum::<usize>()
    };
    let (half, full) = (bytes(4), bytes(8));
    assert!(full < 3 * half, "8 levels write {full} bytes against {half} for 4");
}

#[test]
fn a_helper_reading_another_twice_keeps_the_rows_postgresql_keeps() {
    const LEVELS: u8 = 8;
    let script = helpers::translate_script(&doubled_chain(LEVELS.into()), &options())
        .unwrap_or_else(|error| panic!("the chain translates: {error}"));
    let blob = |byte: u8| format!("X'{}'", format!("{byte:02x}").repeat(16));
    let mut connection = helpers::establish_connection();
    script.register_functions(&mut connection).expect("register helper functions");
    for statement in script.iter() {
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
