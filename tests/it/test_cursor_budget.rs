//! A trigger or a view whose program SQLite refuses to prepare is refused at
//! translation, naming the helpers it repeats.
//!
//! SQLite refuses to expand a `WITH` query in a program already holding
//! `SQLITE_LIMIT_EXPR_DEPTH` table cursors, and every call site of a helper
//! is written as its own copy of the helper's set, so a schema whose helpers
//! call one another repeatedly loads and then fails on the first write.

use core::fmt::Write as _;

use diesel::RunQueryDsl;
use pg2sqlite::{errors::Error, pg2sqlite::Pg2Sqlite, prelude::Pg2SqliteOptions};
use sql_traits::structs::{Deployment, SessionSetting, SettingValue};
use sqlparser::ast::DataType;

use crate::helpers;

/// Helpers `ids_0` to `ids_<levels>`, the first walking the caller's items
/// down the hierarchy and each next one calling the previous one twice, with
/// the update policy calling the last `calls` times.
fn walk_chain(levels: usize, calls: usize) -> String {
    helper_chain(levels, calls, |previous| {
        format!(
            "SELECT item.id FROM public.items item WHERE item.id IN (SELECT public.ids_{previous}())
  UNION
  SELECT item.id FROM public.items item WHERE item.parent_id IN (SELECT public.ids_{previous}())"
        )
    })
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

/// A trigger on `events` running `writes` copies of `write`, then a write
/// whose `WITH` query SQLite expands once the copies hold their cursors.
fn writing_trigger(target: &str, write: &str, writes: usize) -> String {
    format!(
        "CREATE TABLE events (id INTEGER PRIMARY KEY, v INTEGER NOT NULL);
CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
{}
  UPDATE {target} SET v = NEW.v WHERE id IN (WITH last (id) AS (SELECT NEW.id) SELECT id FROM last);
  RETURN NEW;
END
$$;
CREATE TRIGGER touch AFTER UPDATE ON events FOR EACH ROW EXECUTE FUNCTION touch();
",
        vec![format!("  {write};"); writes].join("\n")
    )
}

/// Translates `script(count)` for each of `counts`, over the translation of
/// `starting` when given, which the translation also reads as the SQLite
/// statements the database holds, and runs `firing` on each script
/// translated, which prepares every program it fires.
///
/// Asserts that SQLite prepares every program of a script translated, and
/// that the translation refuses the larger counts.
fn sweep(
    starting: Option<&str>,
    script: impl Fn(usize) -> String,
    counts: impl IntoIterator<Item = usize>,
    firing: &str,
    options: &Pg2SqliteOptions,
) {
    let starting = starting.map(|sql| {
        let translator = Pg2Sqlite::default().sql(sql).expect("the starting schema parses");
        let schema = translator.build_schema().expect("the starting schema builds");
        let script = translator
            .translate_to_sql(options)
            .unwrap_or_else(|error| panic!("the starting schema is refused: {error}"))
            .into_vec();
        (schema, script)
    });
    let mut refused = None;
    let mut accepted = None;
    for count in counts {
        let translator = Pg2Sqlite::default().sql(&script(count)).expect("the script parses");
        let translated = match &starting {
            Some((schema, held)) => {
                translator
                    .starting_sqlite(&held.join(";\n"))
                    .expect("the starting script parses as SQLite")
                    .translate_to_sql_with_schema(schema, options)
            }
            None => translator.translate_to_sql(options),
        };
        match translated {
            Ok(translated) => {
                assert!(refused.is_none(), "{count} translates past a refusal at {refused:?}");
                let mut connection = helpers::establish_connection();
                let base = starting.iter().flat_map(|(_, script)| script);
                for statement in base.chain(&translated.into_vec()) {
                    diesel::sql_query(statement).execute(&mut connection).unwrap_or_else(|error| {
                        panic!("emitted statement failed: {error}\n{statement}")
                    });
                }
                diesel::sql_query(firing)
                    .execute(&mut connection)
                    .unwrap_or_else(|error| panic!("SQLite refuses {firing} at {count}: {error}"));
                accepted = Some(count);
            }
            Err(Error::TranslationRefusal(_)) => refused = refused.or(Some(count)),
            Err(error) => panic!("{count} fails to translate: {error}"),
        }
    }
    assert!(
        accepted.is_some() && refused.is_some(),
        "the sweep crosses the limit, accepted up to {accepted:?}, refused from {refused:?}"
    );
}

#[test]
fn every_policy_translated_prepares() {
    sweep(None, |calls| walk_chain(1, calls), 1..=16, "UPDATE items SET value = 1", &options());
}

#[test]
fn a_table_renamed_keeps_its_indexes_and_foreign_keys() {
    let script = |writes| {
        format!(
            "CREATE TABLE parents (id INTEGER PRIMARY KEY);
CREATE TABLE rows_old (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents (id), a INTEGER, b INTEGER, c INTEGER, d INTEGER, v INTEGER);
CREATE INDEX rows_a ON rows_old (a);
CREATE INDEX rows_b ON rows_old (b);
CREATE INDEX rows_c ON rows_old (c);
CREATE INDEX rows_d ON rows_old (d);
ALTER TABLE rows_old RENAME TO rows;
{}",
            writing_trigger("rows", "UPDATE rows SET v = NEW.v, parent_id = NULL WHERE a = NEW.id", writes)
        )
    };
    sweep(
        None,
        script,
        (10..=200).step_by(10),
        "UPDATE events SET v = 1",
        &Pg2SqliteOptions::default(),
    );
}

#[test]
fn a_foreign_key_added_to_a_table_is_checked_by_its_writes() {
    let script = |writes| {
        format!(
            "CREATE TABLE parents (id INTEGER PRIMARY KEY);
CREATE TABLE rows (id INTEGER PRIMARY KEY, v INTEGER);
ALTER TABLE rows ADD COLUMN p1 INTEGER REFERENCES parents (id);
ALTER TABLE rows ADD COLUMN p2 INTEGER REFERENCES parents (id);
ALTER TABLE rows ADD COLUMN p3 INTEGER REFERENCES parents (id);
ALTER TABLE rows ADD COLUMN p4 INTEGER REFERENCES parents (id);
{}",
            writing_trigger(
                "rows",
                "UPDATE rows SET v = NEW.v, p1 = NEW.id, p2 = NEW.id, p3 = NEW.id, p4 = NEW.id WHERE id = NEW.id",
                writes
            )
        )
    };
    sweep(
        None,
        script,
        (10..=200).step_by(10),
        "UPDATE events SET v = 1",
        &Pg2SqliteOptions::default(),
    );
}

#[test]
fn a_table_referenced_by_several_columns_checks_each_on_its_writes() {
    let script = |writes| {
        format!(
            "CREATE TABLE parents (id INTEGER PRIMARY KEY, v INTEGER);
CREATE TABLE rows (id INTEGER PRIMARY KEY, p1 INTEGER REFERENCES parents (id), p2 INTEGER REFERENCES parents (id), p3 INTEGER REFERENCES parents (id), p4 INTEGER REFERENCES parents (id));
{}",
            writing_trigger(
                "parents",
                "UPDATE parents SET id = NEW.id WHERE id = NEW.id",
                writes
            )
        )
    };
    sweep(
        None,
        script,
        (10..=200).step_by(10),
        "UPDATE events SET v = 1",
        &Pg2SqliteOptions::default(),
    );
}

#[test]
fn a_write_reading_other_relations_counts_them() {
    let script = |writes| {
        format!(
            "CREATE TABLE parents (id INTEGER PRIMARY KEY, v INTEGER);
CREATE TABLE rows (id INTEGER PRIMARY KEY, v INTEGER);
{}",
            writing_trigger(
                "rows",
                "UPDATE rows SET v = parents.v FROM parents, (SELECT v FROM parents JOIN events USING (v)) AS shared WHERE parents.id = rows.id AND shared.v = parents.v AND rows.id = NEW.id",
                writes
            )
        )
    };
    sweep(
        None,
        script,
        (10..=200).step_by(10),
        "UPDATE events SET v = 1",
        &Pg2SqliteOptions::default(),
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
fn a_view_past_the_limit_is_refused_as_the_statement_reading_it() {
    sweep(None, reaching_view, 1..=6, "SELECT count(*) FROM reached", &options());
    let message = refusal(&reaching_view(6));
    for named in ["view `reached`", "ids_6() once"] {
        assert!(message.contains(named), "the refusal names {named}: {message}");
    }
}

#[test]
fn a_view_of_the_starting_schema_counts_in_the_programs_reading_it() {
    let starting = format!(
        "{}CREATE VIEW reached WITH (security_invoker = true) AS SELECT id FROM items WHERE id IN (SELECT ids_2());
GRANT SELECT ON reached TO app;
",
        walk_chain(2, 1)
    );
    let script = |reads| {
        format!(
            "CREATE TABLE tallies (id INTEGER PRIMARY KEY, v INTEGER NOT NULL);
{}GRANT SELECT, UPDATE ON tallies TO app;
GRANT SELECT, UPDATE ON events TO app;
",
            writing_trigger(
                "tallies",
                "UPDATE tallies SET v = (SELECT count(*) FROM reached) WHERE id = NEW.id",
                reads
            )
        )
    };
    sweep(
        Some(&starting),
        script,
        (1..=20).map(|reads| reads * 2),
        "UPDATE events SET v = 1",
        &options(),
    );
}

/// The refusal translating `sql` raises.
fn refusal(sql: &str) -> String {
    let error = helpers::translate_pg(sql, &options())
        .map(|script| script.join("\n"))
        .expect_err("SQLite refuses to prepare a program of the script");
    let Error::TranslationRefusal(refusal) = &error else { panic!("not a refusal: {error}") };
    refusal.to_string()
}

#[test]
fn a_program_past_sqlites_cursor_limit_is_refused_naming_the_repeated_helpers() {
    let message = refusal(&walk_chain(1, 16));
    for named in ["items_update_trigger", "cursors", "ids_0() 96 times", "ids_1() 48 times"] {
        assert!(message.contains(named), "the refusal names {named}: {message}");
    }
}

#[test]
fn a_deep_helper_chain_is_refused_on_a_default_thread() {
    let message = refusal(&walk_chain(7, 1));
    assert!(message.contains("ids_7()"), "the refusal names the policy's helper: {message}");
}

/// Past the 60 helpers whose inlined sets a debug build copied into an abort
/// on a default thread.
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
