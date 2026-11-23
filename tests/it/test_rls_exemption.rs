//! Execution contracts for the caller-scoped exemption from row-level security
//! and generated write guards.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::{helpers, helpers::deployment::deployment};

const EXEMPTION_FUNCTION: &str = "write_is_exempt";

const POLICY_SCHEMA: &str = "
CREATE TABLE shared_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT ''
);
ALTER TABLE shared_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY shared_items_select ON shared_items FOR SELECT USING (true);
CREATE POLICY shared_items_insert ON shared_items
    FOR INSERT WITH CHECK (owner = 'alice');
CREATE POLICY shared_items_update ON shared_items
    FOR UPDATE USING (owner = 'alice') WITH CHECK (owner = 'alice');
CREATE POLICY shared_items_delete ON shared_items
    FOR DELETE USING (owner = 'alice');
";

const READ_ONLY_SCHEMA: &str = "
CREATE ROLE app_user;
CREATE TABLE reference_items (
    id INTEGER PRIMARY KEY,
    body TEXT NOT NULL
);
GRANT SELECT ON reference_items TO app_user;
";

const READ_ONLY_RLS_SCHEMA: &str = "
CREATE ROLE app_user;
CREATE TABLE secure_reference_items (
    id INTEGER PRIMARY KEY,
    body TEXT NOT NULL
);
ALTER TABLE secure_reference_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY secure_reference_items_select ON secure_reference_items
    FOR SELECT USING (true);
GRANT SELECT ON secure_reference_items TO app_user;
";

const TRIGGER_CHAIN_SCHEMA: &str = "
CREATE TABLE parent_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL
);
CREATE TABLE child_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    note TEXT NOT NULL
);
ALTER TABLE parent_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE child_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY parent_items_select ON parent_items FOR SELECT USING (true);
CREATE POLICY parent_items_insert ON parent_items
    FOR INSERT WITH CHECK (owner = 'alice');
CREATE POLICY child_items_select ON child_items
    FOR SELECT USING (owner = 'alice');
CREATE POLICY child_items_insert ON child_items
    FOR INSERT WITH CHECK (owner = 'alice');
CREATE POLICY child_items_update ON child_items
    FOR UPDATE USING (owner = 'alice') WITH CHECK (owner = 'alice');
CREATE OR REPLACE FUNCTION touch_child() RETURNS TRIGGER AS $$
BEGIN
    UPDATE child_items SET note = 'touched' WHERE id = NEW.id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER touch_child_after_parent
AFTER INSERT ON parent_items
FOR EACH ROW EXECUTE FUNCTION touch_child();
";

const FUNCTION_POLICY_SCHEMA: &str = "
CREATE FUNCTION policy_allows(owner TEXT) RETURNS BOOLEAN LANGUAGE sql STABLE AS $$ SELECT false $$;
CREATE TABLE function_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL
);
ALTER TABLE function_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY function_items_select ON function_items FOR SELECT USING (true);
CREATE POLICY function_items_insert ON function_items
    FOR INSERT WITH CHECK (policy_allows(owner));
";

const RESTRICTIVE_SCHEMA: &str = "
CREATE TABLE restrictive_items (
    id INTEGER PRIMARY KEY,
    body TEXT NOT NULL
);
ALTER TABLE restrictive_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY restrictive_insert ON restrictive_items AS RESTRICTIVE
    FOR INSERT WITH CHECK (true);
CREATE POLICY restrictive_update ON restrictive_items AS RESTRICTIVE
    FOR UPDATE USING (true) WITH CHECK (true);
CREATE POLICY restrictive_delete ON restrictive_items AS RESTRICTIVE
    FOR DELETE USING (true);
";

const ONE_SIDED_UPDATE_SCHEMA: &str = "
CREATE TABLE check_only_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    body TEXT NOT NULL
);
CREATE TABLE using_only_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    body TEXT NOT NULL
);
ALTER TABLE check_only_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE using_only_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY check_only_select ON check_only_items FOR SELECT USING (true);
CREATE POLICY using_only_select ON using_only_items FOR SELECT USING (true);
CREATE POLICY check_only_insert ON check_only_items FOR INSERT WITH CHECK (true);
CREATE POLICY using_only_insert ON using_only_items FOR INSERT WITH CHECK (true);
CREATE POLICY check_only_update ON check_only_items FOR UPDATE
    USING (true) WITH CHECK (owner = 'alice');
CREATE POLICY using_only_update ON using_only_items FOR UPDATE
    USING (owner = 'alice') WITH CHECK (true);
";

/// `gated_items` is read in full and only inserted into, so it stays a plain
/// table, and `column_items` is read and updated in part, so it gets a view.
const GRANT_GATED_SCHEMA: &str = "
CREATE ROLE app_user;
CREATE TABLE gated_items (
    id INTEGER PRIMARY KEY,
    body TEXT NOT NULL
);
CREATE TABLE column_items (
    id INTEGER PRIMARY KEY,
    body TEXT NOT NULL,
    note TEXT NOT NULL,
    secret TEXT NOT NULL DEFAULT ''
);
GRANT SELECT, INSERT ON gated_items TO app_user;
GRANT SELECT (id, body, note), UPDATE (body), DELETE ON column_items TO app_user;
";

/// Seeds a plain table and a row-security table the role only reads.
const SEEDED_READ_ONLY_SCHEMA: &str = "
CREATE ROLE app NOLOGIN;
CREATE TABLE statuses (id TEXT PRIMARY KEY);
INSERT INTO statuses (id) VALUES ('draft'), ('published');
GRANT SELECT ON statuses TO app;
CREATE TABLE kinds (id TEXT PRIMARY KEY);
ALTER TABLE kinds ENABLE ROW LEVEL SECURITY;
CREATE POLICY kinds_read ON kinds FOR SELECT TO app USING (true);
INSERT INTO kinds (id) VALUES ('sample'), ('device');
GRANT SELECT ON kinds TO app;
";

/// Seeds a table the role reads without its key.
const HIDDEN_KEY_SCHEMA: &str = "
CREATE ROLE app NOLOGIN;
CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);
INSERT INTO notes (body) VALUES ('first');
GRANT SELECT (body) ON notes TO app;
";

/// Seeds and changes a table whose policy reads a table created after them.
const SEED_BEFORE_POLICY_READ_SCHEMA: &str = "
CREATE ROLE app NOLOGIN;
CREATE TABLE kinds (id TEXT PRIMARY KEY);
ALTER TABLE kinds ENABLE ROW LEVEL SECURITY;
INSERT INTO kinds (id) VALUES ('sample'), ('device'), ('reagent');
UPDATE kinds SET id = 'specimen' WHERE id = 'reagent';
CREATE TABLE labels (name TEXT PRIMARY KEY);
ALTER TABLE labels RENAME COLUMN name TO label;
CREATE TABLE allowed (id TEXT PRIMARY KEY);
INSERT INTO allowed (id) VALUES ('sample'), ('specimen');
GRANT SELECT ON kinds, labels, allowed TO app;
CREATE POLICY kinds_read ON kinds FOR SELECT TO app USING (id IN (SELECT id FROM allowed));
";

/// Seeds a table whose policy reads a later table under a policy reading a
/// table later still.
const SEED_BEFORE_POLICY_READ_CHAIN_SCHEMA: &str = "
CREATE ROLE app NOLOGIN;
CREATE TABLE kinds (id TEXT PRIMARY KEY);
ALTER TABLE kinds ENABLE ROW LEVEL SECURITY;
INSERT INTO kinds (id) VALUES ('sample'), ('device');
CREATE TABLE allowed (id TEXT PRIMARY KEY);
ALTER TABLE allowed ENABLE ROW LEVEL SECURITY;
INSERT INTO allowed (id) VALUES ('sample'), ('device');
CREATE TABLE granted (id TEXT PRIMARY KEY);
INSERT INTO granted (id) VALUES ('sample');
GRANT SELECT ON kinds, allowed, granted TO app;
CREATE POLICY kinds_read ON kinds FOR SELECT TO app USING (id IN (SELECT id FROM allowed));
CREATE POLICY allowed_read ON allowed FOR SELECT TO app USING (id IN (SELECT id FROM granted));
";

/// Holds a table's view back into a `TEMP` table's unit, which every
/// connection replays and a later rename makes it rebind.
const HELD_VIEW_REBOUND_SCHEMA: &str = "
CREATE ROLE app NOLOGIN;
CREATE TABLE things (code TEXT PRIMARY KEY);
CREATE TABLE kinds (id TEXT PRIMARY KEY);
ALTER TABLE kinds ENABLE ROW LEVEL SECURITY;
CREATE TEMP TABLE allowed AS SELECT code AS id FROM things;
GRANT SELECT ON kinds, allowed, things TO app;
CREATE POLICY kinds_read ON kinds FOR SELECT TO app USING (id IN (SELECT id FROM allowed));
ALTER TABLE things RENAME TO stuff;
";

const HIDDEN_ROWS_SCHEMA: &str = "
CREATE ROLE app NOLOGIN;
CREATE TABLE owned_notes (id TEXT PRIMARY KEY, owner TEXT NOT NULL);
ALTER TABLE owned_notes ENABLE ROW LEVEL SECURITY;
CREATE POLICY owned_notes_read ON owned_notes FOR SELECT TO app USING (owner = 'alice');
CREATE POLICY owned_notes_write ON owned_notes FOR INSERT TO app WITH CHECK (owner = 'alice');
CREATE POLICY owned_notes_update ON owned_notes FOR UPDATE TO app USING (owner = 'alice');
GRANT SELECT, INSERT, UPDATE ON owned_notes TO app;
CREATE TABLE sealed_notes (id TEXT PRIMARY KEY);
ALTER TABLE sealed_notes ENABLE ROW LEVEL SECURITY;
CREATE POLICY sealed_notes_write ON sealed_notes FOR INSERT TO app WITH CHECK (true);
GRANT SELECT, INSERT ON sealed_notes TO app;
";

mod schema {
    diesel::table! {
        shared_items (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        shared_items_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        reference_items (id) {
            id -> Integer,
            body -> Text,
        }
    }
    diesel::table! {
        secure_reference_items_rls (id) {
            id -> Integer,
            body -> Text,
        }
    }
    diesel::table! {
        parent_items_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }
    diesel::table! {
        child_items (id) {
            id -> Integer,
            owner -> Text,
            note -> Text,
        }
    }
    diesel::table! {
        child_items_rls (id) {
            id -> Integer,
            owner -> Text,
            note -> Text,
        }
    }
    diesel::table! {
        function_items_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }
    diesel::table! {
        restrictive_items_rls (id) {
            id -> Integer,
            body -> Text,
        }
    }
    diesel::table! {
        check_only_items_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        using_only_items_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        gated_items (id) {
            id -> Integer,
            body -> Text,
        }
    }
    diesel::table! {
        column_items (id) {
            id -> Integer,
            body -> Text,
            note -> Text,
        }
    }
    diesel::table! {
        column_items_rls (id) {
            id -> Integer,
            body -> Text,
            note -> Text,
            secret -> Text,
        }
    }
    diesel::table! {
        rls_audit (id) {
            id -> Integer,
            table_name -> Text,
        }
    }
    diesel::table! {
        statuses (id) {
            id -> Text,
        }
    }
    diesel::table! {
        kinds (id) {
            id -> Text,
        }
    }
    diesel::table! {
        kinds_rls (id) {
            id -> Text,
        }
    }
    diesel::table! {
        notes (body) {
            body -> Text,
        }
    }
    diesel::table! {
        owned_notes (id) {
            id -> Text,
            owner -> Text,
        }
    }
    diesel::table! {
        sealed_notes (id) {
            id -> Text,
        }
    }
}

diesel::define_sql_function! {
    /// Reports whether generated write guards are exempt.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

diesel::define_sql_function! {
    /// Returns NULL to prove exemption fails closed.
    fn write_exemption_is_null() -> diesel::sql_types::Nullable<diesel::sql_types::Bool>;
}

diesel::define_sql_function! {
    /// Panics to prove exemption errors fail closed.
    fn write_exemption_panics() -> diesel::sql_types::Bool;
}

diesel::define_sql_function! {
    /// Reports whether a policy admits an owner.
    fn policy_allows(owner: diesel::sql_types::Text) -> diesel::sql_types::Bool;
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_strict_rls_write_deny()
        .with_rls_exemption_function(EXEMPTION_FUNCTION)
}

fn translated(pg: &str, options: &Pg2SqliteOptions) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse PostgreSQL")
        .translate_to_sql(options)
        .expect("translate SQLite")
        .into_vec()
}

fn apply_with_exemption(
    pg: &str,
    options: &Pg2SqliteOptions,
) -> (SqliteConnection, Arc<AtomicBool>) {
    let mut connection = helpers::apply_with_options(pg, options).expect("apply");
    let exempt = Arc::new(AtomicBool::new(false));
    let function_state = Arc::clone(&exempt);
    write_is_exempt_utils::register_nondeterministic_impl(&mut connection, move || {
        function_state.load(Ordering::Relaxed)
    })
    .expect("register RLS exemption");
    (connection, exempt)
}

#[test]
fn null_and_function_error_fail_closed() {
    let null_options = options().with_rls_exemption_function("write_exemption_is_null");
    let mut null_connection =
        helpers::apply_with_options(POLICY_SCHEMA, &null_options).expect("apply");
    write_exemption_is_null_utils::register_nondeterministic_impl(&mut null_connection, || {
        None::<bool>
    })
    .expect("register NULL exemption");
    assert!(
        diesel::insert_into(schema::shared_items_rls::table)
            .values((
                schema::shared_items_rls::id.eq(1),
                schema::shared_items_rls::owner.eq("bob"),
                schema::shared_items_rls::body.eq("blocked"),
            ))
            .execute(&mut null_connection)
            .is_err(),
        "NULL must enforce the write policy"
    );

    let panic_options = options().with_rls_exemption_function("write_exemption_panics");
    let mut panic_connection =
        helpers::apply_with_options(POLICY_SCHEMA, &panic_options).expect("apply");
    write_exemption_panics_utils::register_nondeterministic_impl(
        &mut panic_connection,
        || -> bool { panic!("exemption failure") },
    )
    .expect("register failing exemption");
    assert!(
        diesel::insert_into(schema::shared_items_rls::table)
            .values((
                schema::shared_items_rls::id.eq(1),
                schema::shared_items_rls::owner.eq("bob"),
                schema::shared_items_rls::body.eq("blocked"),
            ))
            .execute(&mut panic_connection)
            .is_err(),
        "a function error must abort the write"
    );
}

#[test]
fn exemption_skips_registered_policy_evaluation_but_not_function_resolution() {
    let policy_options = options().with_user_defined_functions(["policy_allows"]);
    let (mut connection, exempt) = apply_with_exemption(FUNCTION_POLICY_SCHEMA, &policy_options);
    let calls = Arc::new(AtomicUsize::new(0));
    let function_calls = Arc::clone(&calls);
    policy_allows_utils::register_nondeterministic_impl(&mut connection, move |_owner: String| {
        function_calls.fetch_add(1, Ordering::Relaxed);
        false
    })
    .expect("register policy function");
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::function_items_rls::table)
        .values((schema::function_items_rls::id.eq(1), schema::function_items_rls::owner.eq("bob")))
        .execute(&mut connection)
        .expect("exemption skips policy evaluation");
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    let (mut missing_connection, missing_exempt) =
        apply_with_exemption(FUNCTION_POLICY_SCHEMA, &policy_options);
    missing_exempt.store(true, Ordering::Relaxed);
    let error = diesel::insert_into(schema::function_items_rls::table)
        .values((schema::function_items_rls::id.eq(2), schema::function_items_rls::owner.eq("bob")))
        .execute(&mut missing_connection)
        .expect_err("SQLite must resolve every policy function");
    assert!(error.to_string().contains("policy_allows"));
}

#[test]
fn exemption_covers_insert_update_and_delete_through_the_rls_view() {
    let (mut connection, exempt) = apply_with_exemption(POLICY_SCHEMA, &options());

    assert!(
        diesel::insert_into(schema::shared_items::table)
            .values((
                schema::shared_items::id.eq(1),
                schema::shared_items::owner.eq("bob"),
                schema::shared_items::body.eq("blocked"),
            ))
            .execute(&mut connection)
            .is_err()
    );

    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::shared_items::table)
        .values((
            schema::shared_items::id.eq(1),
            schema::shared_items::owner.eq("bob"),
            schema::shared_items::body.eq("inserted"),
        ))
        .execute(&mut connection)
        .expect("exempt view insert");

    exempt.store(false, Ordering::Relaxed);
    diesel::update(schema::shared_items::table.find(1))
        .set(schema::shared_items::body.eq("blocked"))
        .execute(&mut connection)
        .expect("policy-denied update is a no-op");
    assert_eq!(
        schema::shared_items_rls::table
            .find(1)
            .select(schema::shared_items_rls::body)
            .get_result::<String>(&mut connection)
            .expect("read blocked update result"),
        "inserted"
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::update(schema::shared_items::table.find(1))
        .set(schema::shared_items::body.eq("updated"))
        .execute(&mut connection)
        .expect("exempt view update");

    exempt.store(false, Ordering::Relaxed);
    diesel::delete(schema::shared_items::table.find(1))
        .execute(&mut connection)
        .expect("policy-denied delete is a no-op");
    assert_eq!(
        schema::shared_items_rls::table
            .count()
            .get_result::<i64>(&mut connection)
            .expect("count after blocked delete"),
        1
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::delete(schema::shared_items::table.find(1))
        .execute(&mut connection)
        .expect("exempt view delete");
    assert_eq!(
        schema::shared_items_rls::table
            .count()
            .get_result::<i64>(&mut connection)
            .expect("count backing rows"),
        0
    );
}

#[test]
fn backing_delete_enforces_policy_unless_exempt() {
    let (mut connection, exempt) = apply_with_exemption(POLICY_SCHEMA, &options());
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::shared_items_rls::table)
        .values(&[
            (
                schema::shared_items_rls::id.eq(1),
                schema::shared_items_rls::owner.eq("bob"),
                schema::shared_items_rls::body.eq("first"),
            ),
            (
                schema::shared_items_rls::id.eq(2),
                schema::shared_items_rls::owner.eq("bob"),
                schema::shared_items_rls::body.eq("second"),
            ),
        ])
        .execute(&mut connection)
        .expect("seed backing rows");

    exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::delete(schema::shared_items_rls::table.find(1)).execute(&mut connection).is_err(),
        "normal backing delete must enforce policy"
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::delete(schema::shared_items_rls::table.find(2))
        .execute(&mut connection)
        .expect("exempt backing delete");
}

/// A DELETE policy admitting every row leaves nothing for the backing guard to
/// refuse, so a backing delete lands without the exemption.
#[test]
fn backing_delete_every_row_admits_needs_no_exemption() {
    let schema_sql =
        POLICY_SCHEMA.replace("FOR DELETE USING (owner = 'alice')", "FOR DELETE USING (true)");
    let (mut connection, exempt) = apply_with_exemption(&schema_sql, &options());
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::shared_items_rls::table)
        .values((
            schema::shared_items_rls::id.eq(1),
            schema::shared_items_rls::owner.eq("bob"),
            schema::shared_items_rls::body.eq("first"),
        ))
        .execute(&mut connection)
        .expect("seed backing row");

    exempt.store(false, Ordering::Relaxed);
    let deleted = diesel::delete(schema::shared_items_rls::table.find(1))
        .execute(&mut connection)
        .expect("a delete every row may take needs no exemption");
    assert_eq!(deleted, 1);
}

#[test]
fn read_only_non_rls_guards_honour_exemption() {
    let role_options = Pg2SqliteOptions::default()
        .with_deployment(deployment(["app_user"], []))
        .with_acting_role("app_user")
        .with_rls_exemption_function(EXEMPTION_FUNCTION);
    let (mut connection, exempt) = apply_with_exemption(READ_ONLY_SCHEMA, &role_options);

    assert!(
        diesel::insert_into(schema::reference_items::table)
            .values((
                schema::reference_items::id.eq(1),
                schema::reference_items::body.eq("blocked")
            ))
            .execute(&mut connection)
            .is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::reference_items::table)
        .values((schema::reference_items::id.eq(1), schema::reference_items::body.eq("inserted")))
        .execute(&mut connection)
        .expect("exempt read-only insert");
    exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::update(schema::reference_items::table.find(1))
            .set(schema::reference_items::body.eq("blocked"))
            .execute(&mut connection)
            .is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::update(schema::reference_items::table.find(1))
        .set(schema::reference_items::body.eq("updated"))
        .execute(&mut connection)
        .expect("exempt read-only update");
    exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::delete(schema::reference_items::table.find(1)).execute(&mut connection).is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::delete(schema::reference_items::table.find(1))
        .execute(&mut connection)
        .expect("exempt read-only delete");
}

#[test]
fn grant_gates_honour_exemption() {
    use schema::{column_items, column_items_rls, gated_items};
    let role_options = Pg2SqliteOptions::default()
        .with_deployment(deployment(["app_user"], []))
        .with_acting_role("app_user")
        .with_rls_exemption_function(EXEMPTION_FUNCTION);
    let (mut connection, exempt) = apply_with_exemption(GRANT_GATED_SCHEMA, &role_options);

    diesel::insert_into(gated_items::table)
        .values((gated_items::id.eq(1), gated_items::body.eq("inserted")))
        .execute(&mut connection)
        .expect("the role inserts");
    let rename = || diesel::update(gated_items::table.find(1)).set(gated_items::body.eq("updated"));
    assert!(rename().execute(&mut connection).is_err(), "the role may not update");
    assert!(diesel::delete(gated_items::table.find(1)).execute(&mut connection).is_err());
    exempt.store(true, Ordering::Relaxed);
    rename().execute(&mut connection).expect("exempt update");
    diesel::delete(gated_items::table.find(1)).execute(&mut connection).expect("exempt delete");

    diesel::insert_into(column_items_rls::table)
        .values((
            column_items_rls::id.eq(1),
            column_items_rls::body.eq("body"),
            column_items_rls::note.eq("note"),
            column_items_rls::secret.eq("secret"),
        ))
        .execute(&mut connection)
        .expect("seed the backing row");
    let annotate =
        || diesel::update(column_items::table.find(1)).set(column_items::note.eq("noted"));
    exempt.store(false, Ordering::Relaxed);
    assert!(annotate().execute(&mut connection).is_err(), "the role may not update note");
    let insert = || {
        diesel::insert_into(column_items::table).values((
            column_items::id.eq(2),
            column_items::body.eq("body"),
            column_items::note.eq("note"),
        ))
    };
    assert!(insert().execute(&mut connection).is_err(), "the role may not insert");
    exempt.store(true, Ordering::Relaxed);
    annotate().execute(&mut connection).expect("exempt update of note");
    insert().execute(&mut connection).expect("exempt insert");
    assert_eq!(
        column_items_rls::table
            .select((column_items_rls::id, column_items_rls::note, column_items_rls::secret))
            .order(column_items_rls::id)
            .load::<(i32, String, String)>(&mut connection)
            .expect("read the backing rows"),
        [(1, "noted".to_owned(), "secret".to_owned()), (2, "note".to_owned(), String::new())]
    );
}

#[test]
fn read_only_rls_backing_guards_honour_exemption() {
    let role_options = Pg2SqliteOptions::default()
        .with_deployment(deployment(["app_user"], []))
        .with_acting_role("app_user")
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function(EXEMPTION_FUNCTION);
    let (mut connection, exempt) = apply_with_exemption(READ_ONLY_RLS_SCHEMA, &role_options);

    assert!(
        diesel::insert_into(schema::secure_reference_items_rls::table)
            .values((
                schema::secure_reference_items_rls::id.eq(1),
                schema::secure_reference_items_rls::body.eq("blocked"),
            ))
            .execute(&mut connection)
            .is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::secure_reference_items_rls::table)
        .values((
            schema::secure_reference_items_rls::id.eq(1),
            schema::secure_reference_items_rls::body.eq("inserted"),
        ))
        .execute(&mut connection)
        .expect("exempt read-only RLS insert");
    exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::update(schema::secure_reference_items_rls::table.find(1))
            .set(schema::secure_reference_items_rls::body.eq("blocked"))
            .execute(&mut connection)
            .is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::update(schema::secure_reference_items_rls::table.find(1))
        .set(schema::secure_reference_items_rls::body.eq("updated"))
        .execute(&mut connection)
        .expect("exempt read-only RLS update");
    exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::delete(schema::secure_reference_items_rls::table.find(1))
            .execute(&mut connection)
            .is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::delete(schema::secure_reference_items_rls::table.find(1))
        .execute(&mut connection)
        .expect("exempt read-only RLS delete");
}

/// A translated trigger run by an exempt write reaches a row the policies hide,
/// and the exempt statement reads it through the view as PostgreSQL's table
/// owner does, so the monitor records nothing. The view hides it again once
/// the exemption is off.
#[test]
fn translated_trigger_writes_backing_storage_that_exempt_reads_see() {
    let (mut connection, exempt) = apply_with_exemption(TRIGGER_CHAIN_SCHEMA, &options());
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::child_items_rls::table)
        .values((
            schema::child_items_rls::id.eq(1),
            schema::child_items_rls::owner.eq("bob"),
            schema::child_items_rls::note.eq("original"),
        ))
        .execute(&mut connection)
        .expect("seed hidden child");
    diesel::insert_into(schema::parent_items_rls::table)
        .values((schema::parent_items_rls::id.eq(1), schema::parent_items_rls::owner.eq("bob")))
        .execute(&mut connection)
        .expect("insert parent and run translated trigger");

    let visible_notes = |connection: &mut SqliteConnection| {
        schema::child_items::table
            .select(schema::child_items::note)
            .load::<String>(connection)
            .expect("read children through the view")
    };
    assert_eq!(visible_notes(&mut connection), ["touched"]);
    assert_eq!(
        schema::rls_audit::table
            .count()
            .get_result::<i64>(&mut connection)
            .expect("count monitoring rows"),
        0
    );
    exempt.store(false, Ordering::Relaxed);
    assert_eq!(visible_notes(&mut connection), Vec::<String>::new());
}

#[test]
fn restrictive_and_one_sided_update_guards_honour_exemption() {
    let (mut restrictive_connection, restrictive_exempt) =
        apply_with_exemption(RESTRICTIVE_SCHEMA, &options());
    assert!(
        diesel::insert_into(schema::restrictive_items_rls::table)
            .values((
                schema::restrictive_items_rls::id.eq(1),
                schema::restrictive_items_rls::body.eq("blocked"),
            ))
            .execute(&mut restrictive_connection)
            .is_err()
    );
    restrictive_exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::restrictive_items_rls::table)
        .values((
            schema::restrictive_items_rls::id.eq(1),
            schema::restrictive_items_rls::body.eq("inserted"),
        ))
        .execute(&mut restrictive_connection)
        .expect("exempt restrictive-only insert");
    restrictive_exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::update(schema::restrictive_items_rls::table.find(1))
            .set(schema::restrictive_items_rls::body.eq("blocked"))
            .execute(&mut restrictive_connection)
            .is_err()
    );
    restrictive_exempt.store(true, Ordering::Relaxed);
    diesel::update(schema::restrictive_items_rls::table.find(1))
        .set(schema::restrictive_items_rls::body.eq("updated"))
        .execute(&mut restrictive_connection)
        .expect("exempt restrictive-only update");
    restrictive_exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::delete(schema::restrictive_items_rls::table.find(1))
            .execute(&mut restrictive_connection)
            .is_err()
    );
    restrictive_exempt.store(true, Ordering::Relaxed);
    diesel::delete(schema::restrictive_items_rls::table.find(1))
        .execute(&mut restrictive_connection)
        .expect("exempt restrictive-only delete");

    let (mut one_sided_connection, one_sided_exempt) =
        apply_with_exemption(ONE_SIDED_UPDATE_SCHEMA, &options());
    one_sided_exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::check_only_items_rls::table)
        .values((
            schema::check_only_items_rls::id.eq(1),
            schema::check_only_items_rls::owner.eq("bob"),
            schema::check_only_items_rls::body.eq("old"),
        ))
        .execute(&mut one_sided_connection)
        .expect("seed check-only row");
    diesel::insert_into(schema::using_only_items_rls::table)
        .values((
            schema::using_only_items_rls::id.eq(1),
            schema::using_only_items_rls::owner.eq("bob"),
            schema::using_only_items_rls::body.eq("old"),
        ))
        .execute(&mut one_sided_connection)
        .expect("seed using-only row");
    one_sided_exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::update(schema::check_only_items_rls::table.find(1))
            .set(schema::check_only_items_rls::body.eq("blocked"))
            .execute(&mut one_sided_connection)
            .is_err()
    );
    assert!(
        diesel::update(schema::using_only_items_rls::table.find(1))
            .set(schema::using_only_items_rls::body.eq("blocked"))
            .execute(&mut one_sided_connection)
            .is_err()
    );
    one_sided_exempt.store(true, Ordering::Relaxed);
    diesel::update(schema::check_only_items_rls::table.find(1))
        .set(schema::check_only_items_rls::body.eq("updated"))
        .execute(&mut one_sided_connection)
        .expect("exempt check-only update");
    diesel::update(schema::using_only_items_rls::table.find(1))
        .set(schema::using_only_items_rls::body.eq("updated"))
        .execute(&mut one_sided_connection)
        .expect("exempt using-only update");
}

#[test]
fn hardened_connection_accepts_only_innocuous_exemption_function() {
    use rusqlite::functions::FunctionFlags;

    let translated = translated(
        POLICY_SCHEMA,
        &Pg2SqliteOptions::default()
            .with_rls_audit_table_name("rls_audit")
            .with_rls_exemption_function(EXEMPTION_FUNCTION),
    );
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS;
    let connection = rusqlite::Connection::open_in_memory().expect("open hardened SQLite");
    connection
        .create_scalar_function(EXEMPTION_FUNCTION, 0, flags, |_| Ok(true))
        .expect("register innocuous exemption");
    connection.execute_batch("PRAGMA trusted_schema = OFF").expect("disable trusted schema");
    for statement in &translated {
        connection.execute_batch(statement).expect("apply hardened DDL");
    }
    // rusqlite is required here because the project's Diesel version cannot set
    // SQLITE_INNOCUOUS.
    connection
        .execute("INSERT INTO shared_items_rls (id, owner, body) VALUES (1, 'bob', 'server')", [])
        .expect("innocuous exemption runs from the trigger");

    let unsafe_connection = rusqlite::Connection::open_in_memory().expect("open unsafe SQLite");
    unsafe_connection
        .create_scalar_function(EXEMPTION_FUNCTION, 0, FunctionFlags::SQLITE_UTF8, |_| Ok(true))
        .expect("register non-innocuous exemption");
    unsafe_connection.execute_batch("PRAGMA trusted_schema = OFF").expect("disable trusted schema");
    for statement in &translated {
        unsafe_connection.execute_batch(statement).expect("apply unsafe DDL");
    }
    let error = unsafe_connection
        .execute("INSERT INTO shared_items_rls (id, owner, body) VALUES (1, 'bob', 'server')", [])
        .expect_err("non-innocuous schema function must fail");
    assert!(error.to_string().contains("unsafe use"));
}

#[test]
fn quoted_exemption_function_name_is_safe() {
    const QUOTED_NAME: &str = "select";
    let quoted_options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function(QUOTED_NAME);
    let mut connection =
        helpers::apply_with_options(POLICY_SCHEMA, &quoted_options).expect("apply");
    connection
        .register_noarg_sql_function::<diesel::sql_types::Bool, bool, _>(
            QUOTED_NAME,
            diesel::sqlite::SqliteFunctionBehavior::empty(),
            || true,
        )
        .expect("register quoted function name");
    diesel::insert_into(schema::shared_items_rls::table)
        .values((
            schema::shared_items_rls::id.eq(1),
            schema::shared_items_rls::owner.eq("bob"),
            schema::shared_items_rls::body.eq("server"),
        ))
        .execute(&mut connection)
        .expect("quoted exemption function runs");
}

fn role_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(deployment(["app"], []))
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function(EXEMPTION_FUNCTION)
}

/// The script `sql` translates to under a deployment acting as `app`, loaded
/// with the exemption on, and the switch that turns it off.
fn load_exempt(sql: &str) -> (SqliteConnection, Arc<AtomicBool>) {
    let script = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse PostgreSQL")
        .translate(&role_options())
        .expect("translate SQLite");
    let mut connection =
        <SqliteConnection as diesel::Connection>::establish(":memory:").expect("connect");
    let exempt = Arc::new(AtomicBool::new(true));
    let function_state = Arc::clone(&exempt);
    write_is_exempt_utils::register_nondeterministic_impl(&mut connection, move || {
        function_state.load(Ordering::Relaxed)
    })
    .expect("register RLS exemption");
    for statement in script.iter() {
        // The emitted script is the artifact under test, which no typed DSL
        // can stand in for.
        diesel::sql_query(statement.to_string())
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    (connection, exempt)
}

/// Seed rows load under the exemption into tables the role only reads, and the
/// view forwards exempt writes while refusing the others.
#[test]
fn schema_seeds_load_under_exemption_into_read_only_tables() {
    let (mut connection, exempt) = load_exempt(SEEDED_READ_ONLY_SCHEMA);

    assert_eq!(
        schema::statuses::table
            .select(schema::statuses::id)
            .order(schema::statuses::id)
            .load::<String>(&mut connection)
            .expect("read statuses"),
        ["draft", "published"]
    );
    let kinds = |connection: &mut SqliteConnection| {
        schema::kinds::table
            .select(schema::kinds::id)
            .order(schema::kinds::id)
            .load::<String>(connection)
            .expect("read kinds through the view")
    };
    assert_eq!(kinds(&mut connection), ["device", "sample"]);

    diesel::update(schema::kinds::table.find("sample"))
        .set(schema::kinds::id.eq("specimen"))
        .execute(&mut connection)
        .expect("exempt update through the view");
    diesel::delete(schema::kinds::table.find("device"))
        .execute(&mut connection)
        .expect("exempt delete through the view");
    assert_eq!(kinds(&mut connection), ["specimen"]);

    exempt.store(false, Ordering::Relaxed);
    let error = diesel::insert_into(schema::kinds::table)
        .values(schema::kinds::id.eq("reagent"))
        .execute(&mut connection)
        .expect_err("the role only reads kinds");
    assert!(pg2sqlite::prelude::is_policy_refusal(&error.to_string()), "{error}");
    assert_eq!(kinds(&mut connection), ["specimen"]);
}

/// An exempt update or delete through a view hiding the key is refused, since
/// the trigger cannot name the backing row.
#[test]
fn exempt_writes_through_a_view_hiding_the_key_are_refused() {
    let (mut connection, _exempt) = load_exempt(HIDDEN_KEY_SCHEMA);
    let unfound = "cannot update or delete a row of notes";

    let error = diesel::update(schema::notes::table)
        .set(schema::notes::body.eq("edited"))
        .execute(&mut connection)
        .expect_err("the view hides the key");
    assert!(error.to_string().contains(unfound), "{error}");
    let error = diesel::delete(schema::notes::table)
        .execute(&mut connection)
        .expect_err("the view hides the key");
    assert!(error.to_string().contains(unfound), "{error}");
    assert_eq!(
        schema::notes::table
            .select(schema::notes::body)
            .load::<String>(&mut connection)
            .expect("read notes through the view"),
        ["first"]
    );
}

/// Before the table its policy reads exists, a table's seeds reach every row as
/// its owner's do in PostgreSQL, and nothing names the later table, so a
/// column rename there still loads. The policy applies once the script ends.
#[test]
fn seeds_load_before_the_table_the_policy_reads() {
    let (mut connection, exempt) = load_exempt(SEED_BEFORE_POLICY_READ_SCHEMA);

    assert_eq!(
        schema::kinds_rls::table
            .select(schema::kinds_rls::id)
            .order(schema::kinds_rls::id)
            .load::<String>(&mut connection)
            .expect("read the backing table"),
        ["device", "sample", "specimen"]
    );
    exempt.store(false, Ordering::Relaxed);
    assert_eq!(
        schema::kinds::table
            .select(schema::kinds::id)
            .order(schema::kinds::id)
            .load::<String>(&mut connection)
            .expect("read kinds through the view"),
        ["sample", "specimen"]
    );
}

/// A policy reading a table whose own policy reads a later table applies both
/// once the script ends.
#[test]
fn seeds_load_before_a_chain_of_policy_reads() {
    let (mut connection, exempt) = load_exempt(SEED_BEFORE_POLICY_READ_CHAIN_SCHEMA);
    exempt.store(false, Ordering::Relaxed);

    assert_eq!(
        schema::kinds::table
            .select(schema::kinds::id)
            .load::<String>(&mut connection)
            .expect("read kinds through the view"),
        ["sample"]
    );
}

#[derive(diesel::QueryableByName)]
struct NoteId {
    #[diesel(sql_type = diesel::sql_types::Text)]
    id: String,
}

/// An exempt statement reads every row through a row-secured view, the row it
/// just wrote included, as PostgreSQL's table owner does, under policies that
/// hide some rows, under none admitting any row, and in a locking read. The
/// views filter again once the exemption is off.
#[test]
fn exempt_reads_see_every_row_through_the_view() {
    let (mut connection, exempt) = load_exempt(HIDDEN_ROWS_SCHEMA);
    diesel::insert_into(schema::owned_notes::table)
        .values((schema::owned_notes::id.eq("n1"), schema::owned_notes::owner.eq("bob")))
        .execute(&mut connection)
        .expect("exempt insert of a hidden row");
    diesel::insert_into(schema::sealed_notes::table)
        .values(schema::sealed_notes::id.eq("s1"))
        .execute(&mut connection)
        .expect("exempt insert into a sealed table");
    let locking_read = Pg2Sqlite::default()
        .sql(&format!("{HIDDEN_ROWS_SCHEMA}SELECT id FROM owned_notes FOR UPDATE;"))
        .expect("parse PostgreSQL")
        .translate(&role_options())
        .expect("translate SQLite")
        .iter()
        .last()
        .expect("the locking read translates")
        .to_string();

    let reads = |connection: &mut SqliteConnection| {
        let owned = schema::owned_notes::table
            .filter(schema::owned_notes::id.eq("n1"))
            .select(schema::owned_notes::id)
            .load::<String>(connection)
            .expect("read owned notes through the view");
        let sealed = schema::sealed_notes::table
            .select(schema::sealed_notes::id)
            .load::<String>(connection)
            .expect("read sealed notes through the view");
        // The translated locking read is pg2sqlite's output, which no typed
        // query can state.
        let locked = diesel::sql_query(&locking_read)
            .load::<NoteId>(connection)
            .unwrap_or_else(|error| panic!("{locking_read}: {error}"))
            .into_iter()
            .map(|row| row.id)
            .collect::<Vec<_>>();
        (owned, sealed, locked)
    };
    assert_eq!(reads(&mut connection), (vec!["n1".into()], vec!["s1".into()], vec!["n1".into()]));
    exempt.store(false, Ordering::Relaxed);
    assert_eq!(reads(&mut connection), (Vec::new(), Vec::new(), Vec::new()));
}

/// A connection is refused when it would retranslate on its own a unit a
/// held-back view moved into, which would leave the view out.
#[test]
fn connection_refuses_rebinding_a_unit_holding_a_view() {
    let script = Pg2Sqlite::default()
        .sql(HELD_VIEW_REBOUND_SCHEMA)
        .expect("parse PostgreSQL")
        .translate(
            &Pg2SqliteOptions::default()
                .with_deployment(deployment(["app"], []))
                .with_acting_role("app")
                .with_rls_audit_table_name("rls_audit"),
        )
        .expect("translate SQLite");
    let Err(error) = script.connect() else {
        panic!("a held view cannot follow its unit through a retranslation");
    };
    assert!(error.to_string().contains("held back"), "{error}");
}
