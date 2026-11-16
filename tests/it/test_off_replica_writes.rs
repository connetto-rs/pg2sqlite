//! Statements writing a table the acting role holds no privilege on.
//!
//! Such a table is left off the replica, and every statement whose target it
//! is leaves with it, since the replica holds only what the role reads. A
//! statement whose effect could still reach a table the role reads, through a
//! trigger on the table or a foreign key acting on its referencing rows, is
//! refused instead.

use diesel::prelude::*;
use pg2sqlite::prelude::Pg2SqliteOptions;

use crate::helpers;

diesel::table! {
    /// The table the acting role reads and writes.
    kinds (id) {
        /// Primary key.
        id -> Integer,
        /// Label.
        label -> Text,
    }
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_acting_role("app")
}

/// `pg` led by the acting role, a table it holds no privilege on, and one it
/// writes.
fn script(pg: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE TABLE grantees (id UUID PRIMARY KEY, kind TEXT NOT NULL);
         CREATE TABLE kinds (id INTEGER PRIMARY KEY, label TEXT NOT NULL);
         GRANT ALL ON kinds TO app;
         {pg}"
    )
}

fn refusal(pg: &str) -> String {
    helpers::translate_pg(&script(pg), &options())
        .expect_err("the statement should be refused")
        .to_string()
}

#[test]
fn a_seed_of_a_table_left_off_the_replica_leaves_with_it() {
    let mut connection = helpers::apply_with_options(
        &script(
            "INSERT INTO grantees VALUES (gen_random_uuid(), 'everyone');
             INSERT INTO kinds VALUES (1, 'everyone');",
        ),
        &options(),
    )
    .expect("the script runs on SQLite");

    let labels: Vec<String> =
        kinds::table.select(kinds::label).load(&mut connection).expect("kinds is on the replica");
    assert_eq!(labels, ["everyone"]);
}

#[test]
fn every_write_to_a_table_left_off_the_replica_leaves_with_it() {
    let mut connection = helpers::apply_with_options(
        &script(
            "INSERT INTO grantees SELECT gen_random_uuid(), label FROM kinds;
             INSERT INTO grantees VALUES (gen_random_uuid(), 'a')
                 ON CONFLICT (id) DO UPDATE SET kind = excluded.kind;
             UPDATE grantees SET kind = 'b';
             DELETE FROM grantees;
             TRUNCATE grantees;
             COPY grantees (id, kind) FROM stdin;
00000000-0000-0000-0000-000000000001\teveryone
\\.
             ALTER TABLE grantees ADD COLUMN note TEXT;
             CREATE INDEX grantees_kind ON grantees (kind);
             DROP INDEX grantees_kind;
             ALTER TABLE grantees RENAME TO principals;
             INSERT INTO principals VALUES (gen_random_uuid(), 'renamed', NULL);
             INSERT INTO kinds VALUES (1, 'kept');
             DROP TABLE principals, kinds;
             CREATE TABLE kinds (id INTEGER PRIMARY KEY, label TEXT NOT NULL);
             GRANT ALL ON kinds TO app;
             INSERT INTO kinds VALUES (2, 'recreated');",
        ),
        &options(),
    )
    .expect("the script runs on SQLite");

    let labels: Vec<String> =
        kinds::table.select(kinds::label).load(&mut connection).expect("kinds is on the replica");
    assert_eq!(labels, ["recreated"]);
}

#[test]
fn a_write_firing_a_trigger_on_a_table_left_off_the_replica_is_refused() {
    let triggered = "CREATE FUNCTION note_grantee() RETURNS trigger AS $$
         BEGIN
             INSERT INTO kinds VALUES (3, NEW.kind);
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER grantees_noted AFTER INSERT ON grantees
             FOR EACH ROW EXECUTE FUNCTION note_grantee();";

    let message = refusal(&format!(
        "{triggered} INSERT INTO grantees VALUES (gen_random_uuid(), 'everyone');"
    ));
    assert!(message.contains("grantees_noted"), "{message}");

    // The trigger follows its table through a rename.
    let message = refusal(&format!(
        "{triggered} ALTER TABLE grantees RENAME TO principals;
         INSERT INTO principals VALUES (gen_random_uuid(), 'everyone');"
    ));
    assert!(message.contains("grantees_noted"), "{message}");

    // A trigger created after the write, dropped before it, or gone with its
    // table never fires for it.
    helpers::apply_with_options(
        &script(&format!(
            "INSERT INTO grantees VALUES (gen_random_uuid(), 'before');
             {triggered}
             DROP TRIGGER grantees_noted ON grantees;
             INSERT INTO grantees VALUES (gen_random_uuid(), 'after');
             UPDATE grantees SET kind = 'unwatched';
             CREATE TRIGGER grantees_noted AFTER INSERT ON grantees
                 FOR EACH ROW EXECUTE FUNCTION note_grantee();
             DROP TABLE grantees;
             CREATE TABLE grantees (id UUID PRIMARY KEY, kind TEXT NOT NULL);
             INSERT INTO grantees VALUES (gen_random_uuid(), 'recreated');"
        )),
        &options(),
    )
    .expect("the script runs on SQLite");
}

#[test]
fn an_update_trigger_on_other_columns_does_not_fire() {
    helpers::apply_with_options(
        &script(
            "CREATE FUNCTION touch() RETURNS trigger AS $$
             BEGIN
                 RETURN NEW;
             END;
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER grantees_kind BEFORE UPDATE OF kind ON grantees
                 FOR EACH ROW EXECUTE FUNCTION touch();
             UPDATE grantees SET id = gen_random_uuid();",
        ),
        &options(),
    )
    .expect("the script runs on SQLite");

    let message = refusal(
        "CREATE FUNCTION touch() RETURNS trigger AS $$
         BEGIN
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER grantees_kind BEFORE UPDATE OF kind ON grantees
             FOR EACH ROW EXECUTE FUNCTION touch();
         UPDATE grantees SET kind = 'b';",
    );
    assert!(message.contains("grantees_kind"), "{message}");
}

#[test]
fn a_write_reaching_a_visible_table_through_a_foreign_key_is_refused() {
    let referenced = "CREATE TABLE memberships (
             id INTEGER PRIMARY KEY,
             grantee UUID REFERENCES grantees (id) ON DELETE CASCADE ON UPDATE SET NULL,
             kind TEXT
         );
         GRANT ALL ON memberships TO app;";
    let options = options().with_uuid_representation(pg2sqlite::prelude::UuidRepresentation::Text);
    let refused = |pg: &str| {
        helpers::translate_pg(&script(&format!("{referenced} {pg}")), &options)
            .expect_err("the statement should be refused")
            .to_string()
    };

    let message = refused("DELETE FROM grantees;");
    assert!(message.contains("memberships"), "{message}");
    assert!(message.contains("CASCADE"), "{message}");

    let message = refused("UPDATE grantees SET id = gen_random_uuid();");
    assert!(message.contains("memberships"), "{message}");
    assert!(message.contains("SET NULL"), "{message}");

    // Neither the key's action nor a referencing row is touched.
    helpers::translate_pg(
        &script(&format!(
            "{referenced}
             UPDATE grantees SET kind = 'b';
             INSERT INTO grantees VALUES (gen_random_uuid(), 'everyone');"
        )),
        &options,
    )
    .expect("the writes leave with grantees");
}
