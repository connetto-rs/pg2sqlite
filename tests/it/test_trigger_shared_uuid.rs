//! Generated assignments share one stored UUID across multiple `INSERT`s.

use diesel::prelude::*;
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};

use crate::helpers;

// Schema definitions for test tables
diesel::table! {
    /// Entities table with UUID primary key.
    entities (id) {
        /// Entity ID (UUID stored as BLOB).
        id -> Binary,
        /// Entity name.
        name -> Text,
    }
}

diesel::table! {
    /// Ownables table with UUID primary key referencing entities.
    ownables (id) {
        /// Ownable ID (UUID stored as BLOB), references entities(id).
        id -> Binary,
        /// Owner name.
        owner -> Text,
    }
}

diesel::table! {
    /// Parent relations table tracking entity parent relationships.
    parent_relations (id) {
        /// Parent relation ID (UUID stored as BLOB).
        id -> Binary,
        /// Entity ID (UUID stored as BLOB), references entities(id).
        entity_id -> Binary,
        /// Parent name.
        parent_name -> Text,
    }
}

diesel::table! {
    /// Staging table for triggering entity creation.
    entity_staging (rowid) {
        /// SQLite rowid.
        rowid -> Integer,
        /// Entity name.
        name -> Text,
        /// Owner name.
        owner -> Text,
    }
}

diesel::table! {
    /// Input table for triggering full entity creation.
    entity_input (rowid) {
        /// SQLite rowid.
        rowid -> Integer,
        /// Entity name.
        name -> Text,
    }
}

diesel::table! {
    /// Metadata table for entity metadata.
    metadata (entity_id) {
        /// Entity ID (UUID stored as BLOB), references entities(id).
        entity_id -> Binary,
        /// Creation timestamp.
        created_at -> Text,
    }
}

diesel::allow_tables_to_appear_in_same_query!(
    entities,
    ownables,
    parent_relations,
    entity_staging,
    entity_input,
    metadata
);

#[derive(Queryable, Selectable)]
#[diesel(table_name = entities)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Entity {
    /// Entity ID (UUID stored as BLOB).
    id: Vec<u8>,
    /// Entity name.
    #[allow(dead_code)]
    name: String,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = ownables)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Ownable {
    /// Ownable ID (UUID stored as BLOB), references entities(id).
    id: Vec<u8>,
    /// Owner name.
    #[allow(dead_code)]
    owner: String,
}

#[allow(dead_code)]
#[derive(Queryable, Selectable)]
#[diesel(table_name = parent_relations)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct ParentRelation {
    /// Parent relation ID (UUID stored as BLOB).
    id: Vec<u8>,
    /// Entity ID (UUID stored as BLOB), references entities(id).
    entity_id: Vec<u8>,
    /// Parent name.
    parent_name: String,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = metadata)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Metadata {
    /// Entity ID (UUID stored as BLOB), references entities(id).
    entity_id: Vec<u8>,
    /// Creation timestamp.
    #[allow(dead_code)]
    created_at: String,
}

/// A staging table insert.
#[derive(Insertable)]
#[diesel(table_name = entity_staging)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct EntityStagingInsert {
    /// Entity name.
    name: String,
    /// Owner name.
    owner: String,
}

/// An input table insert.
#[derive(Insertable)]
#[diesel(table_name = entity_input)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct EntityInputInsert {
    /// Entity name.
    name: String,
}

/// Generated assignments share one stored value across inserts.
#[test]
fn test_trigger_translation_uses_stored_value() {
    let pg_sql = "
        CREATE TABLE entities (
            id UUID PRIMARY KEY DEFAULT uuidv7(),
            name TEXT NOT NULL
        );

        CREATE TABLE ownables (
            id UUID PRIMARY KEY REFERENCES entities(id),
            owner TEXT NOT NULL
        );

        CREATE TABLE parent_relations (
            id UUID PRIMARY KEY DEFAULT uuidv7(),
            entity_id UUID NOT NULL REFERENCES entities(id),
            parent_name TEXT NOT NULL
        );

        CREATE OR REPLACE FUNCTION create_entity_trigger()
        RETURNS TRIGGER AS $$
        DECLARE
            v_new_id UUID;
        BEGIN
            v_new_id := uuidv7();

            INSERT INTO entities (id, name) VALUES (v_new_id, NEW.name);
            INSERT INTO ownables (id, owner) VALUES (v_new_id, 'system');
            INSERT INTO parent_relations (entity_id, parent_name) VALUES (v_new_id, 'root');

            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER trigger_create_entity
            BEFORE INSERT ON entities
            FOR EACH ROW
            EXECUTE FUNCTION create_entity_trigger();
    ";

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7");

    let translated =
        Pg2Sqlite::default().sql(pg_sql).unwrap().translate(&options).unwrap().into_vec();

    // Join all statements into one string for inspection
    let sqlite_sql: String =
        translated.iter().map(ToString::to_string).collect::<Vec<_>>().join(";\n");

    // Snapshot the translated SQL for consistency checking
    insta::assert_snapshot!("trigger_shared_value_pattern", &sqlite_sql);

    assert!(sqlite_sql.contains("pg2sqlite_body_values"));
    assert!(!sqlite_sql.contains("last_insert_rowid()"));
}

/// Test that the translated trigger actually works in SQLite.
/// This validates that:
/// 1. The same UUID is used in both tables (entities and ownables)
/// 2. Foreign key constraints are satisfied
#[test]
fn test_trigger_shared_uuid_works_in_sqlite() {
    let pg_sql = "
        CREATE TABLE entities (
            id UUID PRIMARY KEY DEFAULT uuidv7(),
            name TEXT NOT NULL
        );

        CREATE TABLE ownables (
            id UUID PRIMARY KEY REFERENCES entities(id),
            owner TEXT NOT NULL
        );

        CREATE OR REPLACE FUNCTION create_entity_with_ownable()
        RETURNS TRIGGER AS $$
        DECLARE
            v_new_id UUID;
        BEGIN
            v_new_id := uuidv7();

            INSERT INTO entities (id, name) VALUES (v_new_id, NEW.name);
            INSERT INTO ownables (id, owner) VALUES (v_new_id, NEW.owner);

            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        -- This trigger runs on a staging table to create both entity and ownable
        CREATE TABLE entity_staging (
            name TEXT NOT NULL,
            owner TEXT NOT NULL
        );

        CREATE TRIGGER trigger_create_entity_ownable
            AFTER INSERT ON entity_staging
            FOR EACH ROW
            EXECUTE FUNCTION create_entity_with_ownable();
    ";

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7");

    let translated =
        Pg2Sqlite::default().sql(pg_sql).unwrap().translate(&options).unwrap().into_vec();

    // Snapshot the translated SQL for consistency checking
    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("trigger_shared_uuid_works_in_sqlite", translated_sql);

    // Execute the translated schema
    let mut conn = establish_connection();
    for stmt in &translated {
        diesel::sql_query(stmt.to_string())
            .execute(&mut conn)
            .expect("Failed to execute statement");
    }

    // Insert via the staging table, which should trigger both INSERTs
    diesel::insert_into(entity_staging::table)
        .values(&EntityStagingInsert {
            name: "test_entity".to_string(),
            owner: "alice".to_string(),
        })
        .execute(&mut conn)
        .expect("Failed to insert into staging table");

    // Verify both tables have a row
    let entity_count: i64 = entities::table.count().get_result(&mut conn).unwrap();
    assert_eq!(entity_count, 1, "Expected 1 entity");

    let ownable_count: i64 = ownables::table.count().get_result(&mut conn).unwrap();
    assert_eq!(ownable_count, 1, "Expected 1 ownable");

    // Most importantly: verify they have the SAME UUID
    let entity: Entity = entities::table.select(Entity::as_select()).first(&mut conn).unwrap();
    let ownable: Ownable = ownables::table.select(Ownable::as_select()).first(&mut conn).unwrap();

    assert_eq!(entity.id, ownable.id, "Entity and ownable should have the same UUID");
}

/// Test the three-table case: entities → ownables → metadata
/// where all three need to share the same UUID.
#[test]
fn test_trigger_three_tables_shared_uuid() {
    let pg_sql = "
        CREATE TABLE entities (
            id UUID PRIMARY KEY DEFAULT uuidv7(),
            name TEXT NOT NULL
        );

        CREATE TABLE ownables (
            id UUID PRIMARY KEY REFERENCES entities(id),
            owner TEXT NOT NULL
        );

        CREATE TABLE metadata (
            entity_id UUID PRIMARY KEY REFERENCES entities(id),
            created_at TEXT NOT NULL DEFAULT 'now'
        );

        CREATE OR REPLACE FUNCTION create_full_entity()
        RETURNS TRIGGER AS $$
        DECLARE
            v_new_id UUID;
        BEGIN
            v_new_id := uuidv7();

            INSERT INTO entities (id, name) VALUES (v_new_id, NEW.name);
            INSERT INTO ownables (id, owner) VALUES (v_new_id, 'system');
            INSERT INTO metadata (entity_id) VALUES (v_new_id);

            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TABLE entity_input (
            name TEXT NOT NULL
        );

        CREATE TRIGGER trigger_create_full_entity
            AFTER INSERT ON entity_input
            FOR EACH ROW
            EXECUTE FUNCTION create_full_entity();
    ";

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7");

    let translated =
        Pg2Sqlite::default().sql(pg_sql).unwrap().translate(&options).unwrap().into_vec();

    // Snapshot the translated SQL for consistency checking
    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("trigger_three_tables_shared_uuid", translated_sql);

    // Execute the translated schema
    let mut conn = establish_connection();
    for stmt in &translated {
        diesel::sql_query(stmt.to_string())
            .execute(&mut conn)
            .expect("Failed to execute statement");
    }

    // Insert via the input table
    diesel::insert_into(entity_input::table)
        .values(&EntityInputInsert { name: "my_entity".to_string() })
        .execute(&mut conn)
        .expect("Failed to insert into input table");

    // Verify all three tables have a row
    let entity_count: i64 = entities::table.count().get_result(&mut conn).unwrap();
    let ownable_count: i64 = ownables::table.count().get_result(&mut conn).unwrap();
    let metadata_count: i64 = metadata::table.count().get_result(&mut conn).unwrap();

    assert_eq!(entity_count, 1);
    assert_eq!(ownable_count, 1);
    assert_eq!(metadata_count, 1);

    let entity: Entity = entities::table.select(Entity::as_select()).first(&mut conn).unwrap();
    let ownable: Ownable = ownables::table.select(Ownable::as_select()).first(&mut conn).unwrap();
    let metadata_record: Metadata =
        metadata::table.select(Metadata::as_select()).first(&mut conn).unwrap();

    assert_eq!(entity.id, ownable.id, "Entity and ownable should have the same UUID");
    assert_eq!(
        entity.id, metadata_record.entity_id,
        "Entity and metadata should reference the same UUID"
    );
}

fn assert_generated_value_survives(columns: bool, between: &str) {
    let entity_columns = if columns { "(id, name)" } else { "" };
    let ownable_columns = if columns { "(id, owner)" } else { "" };
    let postgres = format!(
        "
        CREATE TABLE entities (id UUID PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE ownables (id UUID PRIMARY KEY REFERENCES entities(id), owner TEXT NOT NULL);
        CREATE TABLE entity_staging (name TEXT NOT NULL, owner TEXT NOT NULL);
        CREATE FUNCTION insert_pair() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE generated_id UUID; other_value INTEGER;
        BEGIN
            generated_id := uuidv7();
            INSERT INTO entities {entity_columns} VALUES (generated_id, NEW.name);
            {between}
            INSERT INTO ownables {ownable_columns} VALUES (generated_id, NEW.owner);
            RETURN NEW;
        END $$;
        CREATE TRIGGER insert_pair BEFORE INSERT ON entity_staging
        FOR EACH ROW EXECUTE FUNCTION insert_pair();
        "
    );
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7");
    let translated = Pg2Sqlite::default().sql(&postgres).unwrap().translate(&options).unwrap();
    let mut connection = establish_connection();
    for statement in translated.into_vec() {
        diesel::sql_query(statement.to_string()).execute(&mut connection).unwrap();
    }
    diesel::insert_into(entity_staging::table)
        .values(EntityStagingInsert { name: "entity".into(), owner: "owner".into() })
        .execute(&mut connection)
        .unwrap();
    let entity_ids = entities::table.select(entities::id).load::<Vec<u8>>(&mut connection).unwrap();
    let ownable_ids =
        ownables::table.select(ownables::id).load::<Vec<u8>>(&mut connection).unwrap();
    assert_eq!(entity_ids.len(), 1);
    assert_eq!(entity_ids, ownable_ids);
}

#[diesel::declare_sql_function]
extern "SQL" {
    fn counted_rowid_uuid() -> diesel::sql_types::Binary;
}

#[test]
fn invocation_frame_preserves_last_insert_rowid_before_generated_value_capture() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use diesel::connection::SimpleConnection;

    diesel::table! {
        rowid_seed (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        rowid_observations (observed) {
            observed -> BigInt,
        }
    }

    let postgres = "
        CREATE TABLE rowid_seed (id INTEGER PRIMARY KEY);
        CREATE TABLE rowid_observations (observed BIGINT PRIMARY KEY);
        CREATE TABLE entities (id UUID PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE ownables (id UUID PRIMARY KEY, owner TEXT NOT NULL);
        CREATE TABLE entity_staging (name TEXT NOT NULL, owner TEXT NOT NULL);
        CREATE FUNCTION insert_pair() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE generated_id UUID;
        BEGIN
            INSERT INTO rowid_observations VALUES (last_insert_rowid());
            generated_id := uuidv7();
            INSERT INTO entities (id, name) VALUES (generated_id, NEW.name);
            INSERT INTO ownables (id, owner) VALUES (generated_id, NEW.owner);
            RETURN NEW;
        END $$;
        CREATE TRIGGER insert_pair BEFORE INSERT ON entity_staging
        FOR EACH ROW EXECUTE FUNCTION insert_pair();
    ";
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("counted_rowid_uuid");
    let script = Pg2Sqlite::default().sql(postgres).unwrap().translate_to_sql(&options).unwrap();
    let mut connection = establish_connection();
    script.register_functions(&mut connection).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let calls = Arc::clone(&calls);
        counted_rowid_uuid_utils::register_nondeterministic_impl(&mut connection, move || {
            u128::try_from(calls.fetch_add(1, Ordering::Relaxed) + 1)
                .unwrap()
                .to_be_bytes()
                .to_vec()
        })
        .unwrap();
    }
    let statements = script.into_vec();
    assert!(statements.iter().any(|statement| statement.contains("pg2sqlite_body_values")));
    for statement in statements {
        connection.batch_execute(&statement).unwrap();
    }
    diesel::insert_into(rowid_seed::table)
        .values(rowid_seed::id.eq(73))
        .execute(&mut connection)
        .unwrap();
    diesel::insert_into(entity_staging::table)
        .values(EntityStagingInsert { name: "entity".into(), owner: "owner".into() })
        .execute(&mut connection)
        .unwrap();
    assert_eq!(
        rowid_observations::table
            .select(rowid_observations::observed)
            .load::<i64>(&mut connection)
            .unwrap(),
        [73]
    );
    let entity_ids = entities::table.select(entities::id).load::<Vec<u8>>(&mut connection).unwrap();
    let ownable_ids =
        ownables::table.select(ownables::id).load::<Vec<u8>>(&mut connection).unwrap();
    assert_eq!(entity_ids, [1_u128.to_be_bytes().to_vec()]);
    assert_eq!(entity_ids, ownable_ids);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn generated_value_without_insert_columns() {
    assert_generated_value_survives(false, "");
}

#[test]
fn generated_value_after_untaken_return() {
    assert_generated_value_survives(true, "IF false THEN RETURN NULL; END IF;");
}

#[test]
fn generated_value_after_perform() {
    assert_generated_value_survives(true, "PERFORM 1;");
}

#[test]
fn generated_value_after_select_into() {
    assert_generated_value_survives(true, "SELECT 1 INTO other_value;");
}

#[test]
fn generated_value_after_notice() {
    assert_generated_value_survives(true, "RAISE NOTICE 'notice';");
}

#[test]
fn generated_value_after_nested_if() {
    assert_generated_value_survives(
        true,
        "IF true THEN IF false THEN RETURN NULL; END IF; END IF;",
    );
}
