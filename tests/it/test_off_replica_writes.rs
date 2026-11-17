//! Statements writing a table the acting role holds no privilege on.
//!
//! Such a table is left off the replica, and every statement whose target it
//! is leaves with it, since the replica holds only what the role reads, as
//! does every foreign key into it. A statement whose effect could still reach
//! a table the role reads, through a trigger on the table or a foreign key
//! acting on its referencing rows, is refused instead.

use diesel::prelude::*;
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    warnings::TranslationWarning,
};

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

diesel::table! {
    /// A table the acting role writes, keyed into tables it cannot read.
    users (id) {
        /// Primary key, half of the composite key into `principals`.
        id -> Text,
        /// The other half of the composite key into `principals`.
        kind -> Nullable<Text>,
        /// A key into `kinds`, which the acting role reads.
        label -> Nullable<Integer>,
    }
}

diesel::table! {
    /// A table the acting role writes, with column-level keys into `grantees`.
    notes (id) {
        /// Primary key.
        id -> Integer,
        /// A key into `grantees`, declared with the table.
        author -> Nullable<Text>,
        /// A key into `grantees`, added by `ALTER TABLE`.
        editor -> Nullable<Text>,
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

    let message = refusal(
        "CREATE FUNCTION touch() RETURNS trigger AS $$
         BEGIN
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER grantees_kind BEFORE UPDATE OF kind ON grantees
             FOR EACH ROW EXECUTE FUNCTION touch();
         UPDATE grantees SET (id, kind) = (gen_random_uuid(), 'b');",
    );
    assert!(message.contains("grantees_kind"), "{message}");
}

#[test]
fn only_a_trigger_on_the_written_event_refuses_the_write() {
    let emptied = "CREATE FUNCTION forget_kinds() RETURNS trigger AS $$
         BEGIN
             DELETE FROM kinds;
             RETURN NULL;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER grantees_emptied AFTER TRUNCATE ON grantees
             FOR EACH STATEMENT EXECUTE FUNCTION forget_kinds();
         CREATE TABLE tags (kind INTEGER REFERENCES kinds ON DELETE CASCADE);
         GRANT ALL ON tags TO app;";

    // A key cascading into another table never reaches the written one.
    helpers::apply_with_options(
        &script(&format!(
            "{emptied}
             INSERT INTO grantees VALUES (gen_random_uuid(), 'everyone');
             UPDATE grantees SET kind = 'b';
             DELETE FROM grantees;"
        )),
        &options(),
    )
    .expect("the script runs on SQLite");

    let message = refusal(&format!("{emptied} TRUNCATE grantees;"));
    assert!(message.contains("TRUNCATE on grantees"), "{message}");
    assert!(message.contains("grantees_emptied"), "{message}");
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

#[test]
fn a_replica_leaving_derived_writes_to_the_server_leaves_their_writes_off() {
    let options = options()
        .with_uuid_representation(pg2sqlite::prelude::UuidRepresentation::Text)
        .with_server_derived_writes_omitted();
    helpers::apply_with_options(
        &script(
            "CREATE FUNCTION note_grantee() RETURNS trigger AS $$
             BEGIN
                 INSERT INTO kinds VALUES (3, NEW.kind);
                 RETURN NEW;
             END;
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER grantees_noted AFTER INSERT ON grantees
                 FOR EACH ROW EXECUTE FUNCTION note_grantee();
             CREATE TABLE memberships (
                 id INTEGER PRIMARY KEY,
                 grantee UUID REFERENCES grantees (id) ON DELETE CASCADE
             );
             GRANT ALL ON memberships TO app;
             INSERT INTO grantees VALUES (gen_random_uuid(), 'everyone');
             DELETE FROM grantees;
             INSERT INTO memberships VALUES (1, NULL);",
        ),
        &options,
    )
    .expect("the script runs on SQLite");
}

#[test]
fn a_foreign_key_into_a_table_left_off_the_replica_leaves_with_it() {
    const ID: &str = "00000000-0000-0000-0000-000000000001";
    let options = options().with_uuid_representation(pg2sqlite::prelude::UuidRepresentation::Text);
    let mut connection = helpers::apply_with_options(
        &script(
            "CREATE TABLE principals (id UUID, kind TEXT, PRIMARY KEY (id, kind));
             CREATE TABLE users (
                 id UUID PRIMARY KEY,
                 kind TEXT DEFAULT 'user',
                 label INTEGER REFERENCES kinds (id),
                 FOREIGN KEY (id, kind) REFERENCES principals MATCH FULL
                     DEFERRABLE INITIALLY DEFERRED
             );
             GRANT ALL ON users TO app;
             CREATE TABLE notes (
                 id INTEGER PRIMARY KEY,
                 author UUID REFERENCES grantees (id) ON DELETE CASCADE
             );
             GRANT ALL ON notes TO app;
             ALTER TABLE notes ADD COLUMN editor UUID REFERENCES grantees;
             INSERT INTO kinds VALUES (1, 'user');",
        ),
        &options,
    )
    .expect("the script runs on SQLite");

    diesel::insert_into(users::table)
        .values((users::id.eq(ID), users::label.eq(1)))
        .execute(&mut connection)
        .expect("a user is written without principals on the replica");
    diesel::insert_into(notes::table)
        .values((notes::id.eq(1), notes::author.eq(ID), notes::editor.eq(ID)))
        .execute(&mut connection)
        .expect("a note is written without grantees on the replica");
    diesel::update(notes::table)
        .set(notes::author.eq(None::<String>))
        .execute(&mut connection)
        .expect("a note is updated");
    diesel::delete(notes::table).execute(&mut connection).expect("a note is deleted");

    // The constraints that stay on the replica still hold: the MATCH FULL
    // guard refuses a half-null key, and the key into `kinds` a missing kind.
    diesel::insert_into(users::table)
        .values((
            users::id.eq("00000000-0000-0000-0000-000000000002"),
            users::kind.eq(None::<String>),
        ))
        .execute(&mut connection)
        .expect_err("MATCH FULL refuses a half-null key");
    diesel::insert_into(users::table)
        .values((users::id.eq("00000000-0000-0000-0000-000000000003"), users::label.eq(2)))
        .execute(&mut connection)
        .expect_err("the key into kinds refuses a missing kind");
}

#[test]
fn a_foreign_key_over_intervals_into_a_table_left_off_the_replica_leaves_with_it() {
    helpers::apply_with_options(
        &script(
            "CREATE TABLE windows (span INTERVAL PRIMARY KEY);
             CREATE TABLE plans (
                 id INTEGER PRIMARY KEY,
                 span INTERVAL,
                 FOREIGN KEY (span) REFERENCES windows (span)
             );
             GRANT ALL ON plans TO app;
             INSERT INTO plans VALUES (1, '1 day');",
        ),
        &options(),
    )
    .expect("the script runs on SQLite");
}

/// `notes` keyed into `grantees` by `ALTER TABLE ADD CONSTRAINT`, one key
/// plain and one cascading.
const ADDED_KEYS: &str = "CREATE TABLE notes (id INTEGER PRIMARY KEY, author UUID, editor UUID);
     GRANT ALL ON notes TO app;
     ALTER TABLE notes ADD CONSTRAINT notes_author FOREIGN KEY (author) REFERENCES grantees (id);
     ALTER TABLE notes ADD CONSTRAINT notes_editor FOREIGN KEY (editor) REFERENCES grantees (id)
         ON DELETE CASCADE;";

#[test]
fn an_added_foreign_key_into_a_table_left_off_the_replica_leaves_with_it() {
    const ID: &str = "00000000-0000-0000-0000-000000000001";
    let options = options().with_uuid_representation(UuidRepresentation::Text);
    for options in [options.clone(), options.with_server_derived_writes_omitted()] {
        let mut connection = helpers::apply_with_options(&script(ADDED_KEYS), &options)
            .expect("the script runs on SQLite");
        diesel::insert_into(notes::table)
            .values((notes::id.eq(1), notes::author.eq(ID), notes::editor.eq(ID)))
            .execute(&mut connection)
            .expect("a note is written without grantees on the replica");
    }
}

#[test]
fn a_foreign_key_into_a_table_left_off_the_replica_is_not_reported_as_left_to_the_server() {
    let options = options()
        .with_uuid_representation(UuidRepresentation::Text)
        .with_server_derived_writes_omitted();
    let report = Pg2Sqlite::default()
        .sql(&script(&format!(
            "{ADDED_KEYS}
             CREATE TABLE memberships (
                 id INTEGER PRIMARY KEY,
                 grantee UUID REFERENCES grantees (id) ON DELETE CASCADE,
                 sponsor UUID,
                 FOREIGN KEY (sponsor) REFERENCES grantees (id) ON UPDATE SET NULL
             );
             GRANT ALL ON memberships TO app;"
        )))
        .expect("the fixture parses")
        .translate_with_report(&options)
        .expect("the fixture translates");
    let reported: Vec<_> = report
        .warnings
        .iter()
        .filter(|warning| matches!(warning, TranslationWarning::LeftToServer { .. }))
        .collect();
    assert!(reported.is_empty(), "{reported:?}");
}
