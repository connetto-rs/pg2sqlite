//! Tables placed in an attached database, with every view and trigger that
//! reads them emitted `TEMP` so it can read across databases.

use std::{cell::Cell, path::PathBuf};

use diesel::{connection::SimpleConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{
    Error, FreshFiles, Pg2Sqlite, Pg2SqliteOptions, Placement, Script, SessionVariableMapping,
};

use crate::helpers::{
    deployment::{caller, deployment},
    replay::run,
};

diesel::define_sql_function! {
    /// The caller the connection serves.
    fn current_app_user() -> diesel::sql_types::Text;
}

diesel::define_sql_function! {
    /// Reports whether generated write guards are exempt.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

thread_local! {
    static WRITE_IS_EXEMPT: Cell<bool> = const { Cell::new(false) };
}

mod schema {
    diesel::table! {
        papers (id) {
            id -> Integer,
            title -> Text,
        }
    }

    diesel::table! {
        shared.papers_rls (id) {
            id -> Integer,
            title -> Text,
        }
    }

    diesel::table! {
        paper_shares (id) {
            id -> Integer,
            paper_id -> Integer,
            grantee -> Text,
        }
    }

    diesel::table! {
        paper_shares_rls (id) {
            id -> Integer,
            paper_id -> Integer,
            grantee -> Text,
        }
    }

    diesel::table! {
        notes (id) {
            id -> Integer,
            owner -> Text,
        }
    }

    diesel::table! {
        shared.sqlite_master (name) {
            name -> Text,
        }
    }
}

mod main_catalogue {
    diesel::table! {
        main.sqlite_master (name) {
            name -> Text,
        }
    }

    diesel::table! {
        main.papers_rls (id) {
            id -> Integer,
            title -> Text,
        }
    }
}

use schema::{notes, paper_shares, paper_shares_rls, papers, papers_rls, sqlite_master};

const PAPERS: &str = "
    CREATE TABLE paper_shares (id INT PRIMARY KEY, paper_id INT NOT NULL, grantee TEXT NOT NULL);
    CREATE TABLE papers (id INT PRIMARY KEY, title TEXT NOT NULL);
    ALTER TABLE papers ENABLE ROW LEVEL SECURITY;
    CREATE POLICY papers_read ON papers FOR SELECT USING (
        EXISTS (SELECT 1 FROM paper_shares s WHERE s.paper_id = papers.id
                AND s.grantee = current_setting('app.user_id', true)));
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(deployment([], [caller("app.user_id", "text")]))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "current_app_user"))
        .with_rls_audit_table_name("_rls_audit")
        .with_rls_exemption_function("write_is_exempt")
}

fn placed_options() -> Pg2SqliteOptions {
    options().with_attached_tables("shared", ["papers"])
}

fn translate(pg: &str, options: &Pg2SqliteOptions) -> Result<Script<String>, Error> {
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(options)
}

/// Two main database files sharing one attached file, removed when dropped.
struct Files {
    main: PathBuf,
    second_main: PathBuf,
    shared: PathBuf,
}

impl Files {
    fn new(name: &str) -> Self {
        let directory = std::env::temp_dir();
        let stem = format!("pg2sqlite-attached-{}-{name}", std::process::id());
        let files = Self {
            main: directory.join(format!("{stem}-main.db")),
            second_main: directory.join(format!("{stem}-second-main.db")),
            shared: directory.join(format!("{stem}-shared.db")),
        };
        files.remove();
        files
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.main);
        let _ = std::fs::remove_file(&self.second_main);
        let _ = std::fs::remove_file(&self.shared);
    }

    /// A connection serving `caller`, with the shared file attached.
    fn connect(&self, caller: &str) -> SqliteConnection {
        self.open(&self.main, caller)
    }

    /// A connection serving `caller` from the second main file, with the
    /// shared file attached.
    fn connect_second(&self, caller: &str) -> SqliteConnection {
        self.open(&self.second_main, caller)
    }

    fn open(&self, main: &std::path::Path, caller: &str) -> SqliteConnection {
        let mut connection =
            SqliteConnection::establish(main.to_str().expect("utf-8 path")).expect("open");
        let caller = caller.to_owned();
        current_app_user_utils::register_impl(&mut connection, move || caller.clone())
            .expect("register the caller function");
        write_is_exempt_utils::register_nondeterministic_impl(&mut connection, || {
            WRITE_IS_EXEMPT.with(Cell::get)
        })
        .expect("register the RLS exemption");
        connection
            .batch_execute(&format!("ATTACH '{}' AS shared", self.shared.display()))
            .expect("attach");
        connection
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        self.remove();
    }
}

fn set_write_exempt(exempt: bool) {
    WRITE_IS_EXEMPT.with(|value| value.set(exempt));
}

/// Seeds the backing storage with the write guards exempt, the way a trusted
/// sync bypasses the generated enforcement.
fn seed_papers(connection: &mut SqliteConnection) {
    set_write_exempt(true);
    diesel::insert_into(paper_shares::table)
        .values(&[
            (
                paper_shares::id.eq(1),
                paper_shares::paper_id.eq(10),
                paper_shares::grantee.eq("alice"),
            ),
            (
                paper_shares::id.eq(2),
                paper_shares::paper_id.eq(11),
                paper_shares::grantee.eq("bob"),
            ),
        ])
        .execute(connection)
        .expect("seed shares");
    diesel::insert_into(papers_rls::table)
        .values(&[
            (papers_rls::id.eq(10), papers_rls::title.eq("shared to alice")),
            (papers_rls::id.eq(11), papers_rls::title.eq("shared to bob")),
        ])
        .execute(connection)
        .expect("seed papers");
    set_write_exempt(false);
}

/// Seeds the `notes` backing table directly, the way a server sync would.
fn seed_notes(connection: &mut SqliteConnection) {
    set_write_exempt(true);
    diesel::insert_into(notes::table)
        .values(&[
            (notes::id.eq(1), notes::owner.eq("alice")),
            (notes::id.eq(2), notes::owner.eq("bob")),
        ])
        .execute(connection)
        .expect("seed notes");
    set_write_exempt(false);
}

/// Creates the store set `label`, initializes both files with `script`, and
/// seeds the `notes` backing table as user alice.
fn seed_in_fresh_store(label: &str, script: &Script<String>) -> Files {
    let files = Files::new(label);
    {
        let mut alice = files.connect("alice");
        run(&mut alice, script.initialize(FreshFiles::Both).expect("initialize"));
        seed_notes(&mut alice);
    }
    files
}

fn visible_papers(connection: &mut SqliteConnection) -> Vec<(i32, String)> {
    papers::table.order(papers::id).load(connection).expect("read papers")
}

#[test]
fn listed_table_storage_moves_and_its_policy_objects_go_temp() {
    let script = translate(PAPERS, &placed_options()).expect("translate");
    let files = Files::new("reproduction");
    let mut alice = files.connect("alice");
    run(&mut alice, script.initialize(FreshFiles::Both).expect("initialize"));
    seed_papers(&mut alice);

    assert_eq!(visible_papers(&mut alice), [(10, "shared to alice".to_owned())]);
    let refused = diesel::insert_into(papers::table)
        .values((papers::id.eq(12), papers::title.eq("new")))
        .execute(&mut alice)
        .expect_err("no INSERT policy, so the write is refused");
    assert!(
        refused.to_string().contains("permission denied: no INSERT policy on papers"),
        "{refused}"
    );
    let shared_objects: Vec<String> =
        sqlite_master::table.select(sqlite_master::name).load(&mut alice).expect("catalogue");
    assert_eq!(shared_objects, ["papers_rls"]);
    let main_objects: Vec<String> = main_catalogue::sqlite_master::table
        .select(main_catalogue::sqlite_master::name)
        .load(&mut alice)
        .expect("main catalogue");
    assert!(
        !main_objects.contains(&"papers".to_owned())
            && !main_objects.contains(&"papers_rls".to_owned()),
        "{main_objects:?}"
    );
}

#[test]
fn a_second_connection_running_the_connection_program_sees_its_own_caller_rows() {
    let script = translate(PAPERS, &placed_options()).expect("translate");
    let files = Files::new("second-connection");
    {
        let mut alice = files.connect("alice");
        run(&mut alice, script.initialize(FreshFiles::Both).expect("initialize"));
        seed_papers(&mut alice);
    }

    let mut bob = files.connect("bob");
    run(&mut bob, script.connect().expect("connection program"));
    assert_eq!(visible_papers(&mut bob), [(11, "shared to bob".to_owned())]);
}

#[test]
fn a_second_main_file_reads_an_existing_shared_file_through_its_policy() {
    const NOTES: &str = "
        CREATE TABLE notes (id INT PRIMARY KEY, owner TEXT NOT NULL);
        ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
        CREATE POLICY notes_read ON notes FOR SELECT
            USING (owner = current_setting('app.user_id', true));
    ";
    let script =
        translate(NOTES, &options().with_attached_tables("shared", ["notes"])).expect("translate");
    let files = seed_in_fresh_store("second-main", &script);

    let mut bob = files.connect_second("bob");
    run(&mut bob, script.initialize(FreshFiles::Main).expect("main-only initialization"));
    let visible: Vec<(i32, String)> =
        notes::table.order(notes::id).load(&mut bob).expect("read notes");
    assert_eq!(visible, [(2, "bob".to_owned())]);
}

#[test]
fn a_fresh_attached_store_keeps_a_main_policy_that_reads_it() {
    let script = translate(PAPERS, &options().with_attached_tables("shared", ["paper_shares"]))
        .expect("translate");
    let files = Files::new("fresh-attached-policy");
    {
        let mut alice = files.connect("alice");
        run(&mut alice, script.initialize(FreshFiles::Both).expect("initialize"));
        set_write_exempt(true);
        diesel::insert_into(main_catalogue::papers_rls::table)
            .values((
                main_catalogue::papers_rls::id.eq(10),
                main_catalogue::papers_rls::title.eq("kept"),
            ))
            .execute(&mut alice)
            .expect("seed papers");
        set_write_exempt(false);
    }
    std::fs::remove_file(&files.shared).expect("remove the closed store");

    let mut alice = files.connect("alice");
    run(&mut alice, script.initialize(FreshFiles::Attached).expect("attached-only initialization"));
    assert_eq!(visible_papers(&mut alice), []);
    diesel::insert_into(paper_shares::table)
        .values((
            paper_shares::id.eq(1),
            paper_shares::paper_id.eq(10),
            paper_shares::grantee.eq("alice"),
        ))
        .execute(&mut alice)
        .expect("share a paper in the new store");
    assert_eq!(visible_papers(&mut alice), [(10, "kept".to_owned())]);
}

#[test]
fn a_fresh_attached_store_keeps_the_policy_of_an_unrelated_main_table() {
    const NOTES: &str = "
        CREATE TABLE notes (id INT PRIMARY KEY, owner TEXT NOT NULL);
        ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
        CREATE POLICY notes_read ON notes FOR SELECT
            USING (owner = current_setting('app.user_id', true));
        CREATE TABLE papers (id INT PRIMARY KEY, title TEXT NOT NULL);
    ";
    let script = translate(NOTES, &placed_options()).expect("translate");
    let files = seed_in_fresh_store("fresh-attached-unrelated", &script);
    std::fs::remove_file(&files.shared).expect("remove the closed store");

    let mut bob = files.connect("bob");
    run(&mut bob, script.initialize(FreshFiles::Attached).expect("attached-only initialization"));
    let visible: Vec<(i32, String)> =
        notes::table.order(notes::id).load(&mut bob).expect("read notes");
    assert_eq!(visible, [(2, "bob".to_owned())]);
}

#[test]
fn a_policy_reading_its_own_row_sees_the_rows_the_unplaced_translation_sees() {
    const NOTES: &str = "
        CREATE TABLE notes (id INT PRIMARY KEY, owner TEXT NOT NULL);
        ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
        CREATE POLICY notes_read ON notes FOR SELECT
            USING (owner = current_setting('app.user_id', true));
        CREATE POLICY notes_write ON notes FOR INSERT
            WITH CHECK (owner = current_setting('app.user_id', true));
    ";
    let read_as_alice = |options: &Pg2SqliteOptions, name: &str| {
        let script = translate(NOTES, options).expect("translate");
        let files = Files::new(name);
        let mut connection = files.connect("alice");
        run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
        diesel::insert_into(notes::table)
            .values((notes::id.eq(1), notes::owner.eq("alice")))
            .execute(&mut connection)
            .expect("alice writes her own note");
        diesel::insert_into(notes::table)
            .values((notes::id.eq(2), notes::owner.eq("bob")))
            .execute(&mut connection)
            .expect_err("alice cannot write bob's note");
        notes::table.order(notes::id).load::<(i32, String)>(&mut connection).expect("read notes")
    };

    let placed = read_as_alice(&options().with_attached_tables("shared", ["notes"]), "own-placed");
    let unplaced = read_as_alice(&options(), "own-unplaced");
    assert_eq!(placed, [(1, "alice".to_owned())]);
    assert_eq!(placed, unplaced);
}

#[test]
fn an_unlisted_policy_reading_a_listed_table_goes_temp_too() {
    let schema = format!(
        "{PAPERS}
        ALTER TABLE paper_shares ENABLE ROW LEVEL SECURITY;
        CREATE POLICY shares_read ON paper_shares FOR SELECT USING (true);
        CREATE POLICY shares_insert ON paper_shares FOR INSERT
            WITH CHECK (EXISTS (SELECT 1 FROM papers p WHERE p.id = paper_id));"
    );
    let script = translate(&schema, &placed_options()).expect("translate");
    let files = Files::new("closure");
    let mut alice = files.connect("alice");
    run(&mut alice, script.initialize(FreshFiles::Both).expect("initialize"));
    set_write_exempt(true);
    diesel::insert_into(paper_shares_rls::table)
        .values((
            paper_shares_rls::id.eq(1),
            paper_shares_rls::paper_id.eq(10),
            paper_shares_rls::grantee.eq("alice"),
        ))
        .execute(&mut alice)
        .expect("seed shares");
    diesel::insert_into(papers_rls::table)
        .values(&[
            (papers_rls::id.eq(10), papers_rls::title.eq("shared to alice")),
            (papers_rls::id.eq(11), papers_rls::title.eq("hidden")),
        ])
        .execute(&mut alice)
        .expect("seed papers");
    set_write_exempt(false);

    diesel::insert_into(paper_shares::table)
        .values((
            paper_shares::id.eq(2),
            paper_shares::paper_id.eq(10),
            paper_shares::grantee.eq("bob"),
        ))
        .execute(&mut alice)
        .expect("paper 10 is visible to alice, so she may share it");
    let refused = diesel::insert_into(paper_shares::table)
        .values((
            paper_shares::id.eq(3),
            paper_shares::paper_id.eq(11),
            paper_shares::grantee.eq("bob"),
        ))
        .execute(&mut alice)
        .expect_err("paper 11 is not visible to alice");
    assert!(!refused.to_string().contains("no such table"), "{refused}");
}

#[test]
fn an_index_on_a_listed_secured_table_moves_with_its_backing_table() {
    let pg = format!("{PAPERS} CREATE INDEX papers_title ON papers (title);");
    let script = translate(&pg, &placed_options()).expect("translate");
    let files = Files::new("index");
    let mut connection = files.connect("alice");
    run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
    let indexes: Vec<String> = sqlite_master::table
        .filter(sqlite_master::name.eq("papers_title"))
        .select(sqlite_master::name)
        .load(&mut connection)
        .expect("shared catalogue");
    assert_eq!(indexes, ["papers_title"]);
}

#[test]
fn the_manifest_says_where_each_object_lives() {
    let manifest = Pg2Sqlite::default()
        .sql(PAPERS)
        .expect("parse")
        .translation_manifest(&placed_options())
        .expect("manifest");
    let entry = |name: &str| {
        let entry = manifest.iter().find(|entry| entry.logical == name).expect("an entry");
        (entry.logical_placement, entry.physical_placement)
    };
    assert_eq!(entry("papers"), (Placement::Temp, Placement::Attached));
    assert_eq!(entry("paper_shares"), (Placement::Main, Placement::Main));
}

fn refusal(pg: &str, options: &Pg2SqliteOptions) -> String {
    match translate(pg, options) {
        Err(Error::UnusableAttachedTables { reason }) => reason,
        other => panic!("expected an attached table refusal, got {other:?}"),
    }
}

#[test]
fn a_foreign_key_across_databases_is_refused_in_either_direction() {
    let into_attached = "
        CREATE TABLE owners (id INT PRIMARY KEY);
        CREATE TABLE notes (id INT PRIMARY KEY, owner INT REFERENCES owners (id));
    ";
    let options = Pg2SqliteOptions::default().with_attached_tables("shared", ["notes"]);
    assert!(refusal(into_attached, &options).contains("foreign key"));

    let options = Pg2SqliteOptions::default().with_attached_tables("shared", ["owners"]);
    assert!(refusal(into_attached, &options).contains("foreign key"));

    let options = Pg2SqliteOptions::default().with_attached_tables("shared", ["owners", "notes"]);
    let script = translate(into_attached, &options).expect("both listed");
    let files = Files::new("foreign-key-both");
    let mut connection = files.connect("alice");
    run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
    let shared_objects: Vec<String> =
        sqlite_master::table.select(sqlite_master::name).load(&mut connection).expect("catalogue");
    assert!(shared_objects.contains(&"owners".to_owned()), "{shared_objects:?}");
    assert!(shared_objects.contains(&"notes".to_owned()), "{shared_objects:?}");
}

#[test]
fn filling_a_table_from_another_database_part_is_initialized() {
    let pg = "
        CREATE TABLE notes (id INT PRIMARY KEY, body TEXT);
        CREATE TABLE archive AS SELECT id, body FROM notes;
    ";
    let options = Pg2SqliteOptions::default().with_attached_tables("shared", ["notes"]);
    let script = translate(pg, &options).expect("a cross-part fill is initialized");
    let files = Files::new("cross-part-fill");
    let mut connection = files.connect("alice");
    run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
    let shared_objects: Vec<String> =
        sqlite_master::table.select(sqlite_master::name).load(&mut connection).expect("catalogue");
    assert!(shared_objects.contains(&"notes".to_owned()), "{shared_objects:?}");
    let main_objects: Vec<String> = main_catalogue::sqlite_master::table
        .select(main_catalogue::sqlite_master::name)
        .load(&mut connection)
        .expect("main catalogue");
    assert!(main_objects.contains(&"archive".to_owned()), "{main_objects:?}");

    let options = Pg2SqliteOptions::default().with_attached_tables("shared", ["notes", "archive"]);
    let script = translate(pg, &options).expect("both in the attached database");
    let files = Files::new("attached-fill");
    let mut connection = files.connect("alice");
    run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
    let shared_objects: Vec<String> =
        sqlite_master::table.select(sqlite_master::name).load(&mut connection).expect("catalogue");
    assert!(shared_objects.contains(&"notes".to_owned()), "{shared_objects:?}");
    assert!(shared_objects.contains(&"archive".to_owned()), "{shared_objects:?}");
}

#[test]
fn a_reserved_schema_or_an_undeclared_table_is_refused() {
    let pg = "CREATE TABLE notes (id INT PRIMARY KEY);";
    let main = Pg2SqliteOptions::default().with_attached_tables("MAIN", ["notes"]);
    assert!(refusal(pg, &main).contains("`MAIN`"));
    let temp = Pg2SqliteOptions::default().with_attached_tables("temp", ["notes"]);
    assert!(refusal(pg, &temp).contains("`temp`"));
    let missing = Pg2SqliteOptions::default().with_attached_tables("shared", ["nots"]);
    assert!(refusal(pg, &missing).contains("`nots`"));
}

#[test]
fn a_serial_key_counts_in_the_attached_database() {
    let pg = "CREATE TABLE notes (id SERIAL PRIMARY KEY, owner TEXT NOT NULL);";
    let script =
        translate(pg, &Pg2SqliteOptions::default().with_attached_tables("shared", ["notes"]))
            .expect("translate");
    let files = Files::new("serial");
    let mut connection = files.connect("alice");
    run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
    diesel::insert_into(notes::table)
        .values(notes::owner.eq("alice"))
        .execute(&mut connection)
        .expect("the key comes from the attached sqlite_sequence");
    let ids: Vec<i32> = notes::table.select(notes::id).load(&mut connection).expect("read");
    assert_eq!(ids, [1]);
}

#[test]
fn renaming_a_listed_table_is_refused() {
    let pg = "CREATE TABLE drafts (id INT PRIMARY KEY); ALTER TABLE drafts RENAME TO notes;";
    let options = Pg2SqliteOptions::default().with_attached_tables("shared", ["notes"]);
    assert!(refusal(pg, &options).contains("renamed"));
}

#[test]
fn translating_against_a_starting_schema_keeps_the_placement() {
    let base = Pg2Sqlite::default().sql(PAPERS).expect("parse");
    let schema = base.build_schema().expect("schema");
    let script = Pg2Sqlite::default()
        .sql("CREATE TABLE extra (id INT PRIMARY KEY);")
        .expect("parse")
        .translate_to_sql_with_schema(&schema, &placed_options())
        .expect("placement against the starting schema");
    let files = Files::new("starting-schema");
    let mut connection = files.connect("alice");
    run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
    let main_objects: Vec<String> = main_catalogue::sqlite_master::table
        .select(main_catalogue::sqlite_master::name)
        .load(&mut connection)
        .expect("main catalogue");
    assert!(main_objects.contains(&"extra".to_owned()), "{main_objects:?}");
}

/// A view a later batch creates over a view an earlier batch placed in `temp`
/// is placed there too.
#[test]
fn a_later_view_over_an_earlier_temp_view_is_temp() {
    diesel::table! {
        titles (title) {
            title -> Text,
        }
    }
    let schema = Pg2Sqlite::default().sql(PAPERS).expect("parse").build_schema().expect("schema");
    let migration = Pg2Sqlite::default()
        .sql("CREATE VIEW titles WITH (security_invoker = true) AS SELECT title FROM papers;")
        .expect("parse")
        .translate_to_sql_with_schema(&schema, &placed_options())
        .expect("translate the migration");
    let script = translate(PAPERS, &placed_options()).expect("translate");
    let files = Files::new("later-temp-view");
    let mut alice = files.connect("alice");
    run(&mut alice, script.initialize(FreshFiles::Both).expect("initialize"));
    run(&mut alice, migration.initialize(FreshFiles::Both).expect("initialize the migration"));
    seed_papers(&mut alice);
    let titles: Vec<String> =
        titles::table.select(titles::title).load(&mut alice).expect("read the later view");
    assert_eq!(titles, ["shared to alice"]);
}

/// An earlier table whose key is a serial, plain or with row-level security,
/// is read by a later batch.
#[test]
fn a_later_batch_reads_an_earlier_serial_table() {
    let earlier = format!(
        "{PAPERS}
         CREATE TABLE counters (id SERIAL PRIMARY KEY, label TEXT);
         CREATE TABLE stamps (id BIGSERIAL PRIMARY KEY, label TEXT);
         ALTER TABLE stamps ENABLE ROW LEVEL SECURITY;
         CREATE POLICY stamps_read ON stamps FOR SELECT USING (true);"
    );
    let schema = Pg2Sqlite::default().sql(&earlier).expect("parse").build_schema().expect("schema");
    Pg2Sqlite::default()
        .sql("SELECT label FROM counters; SELECT label FROM stamps;")
        .expect("parse")
        .translate_to_sql_with_schema(&schema, &placed_options())
        .expect("translate the reads");
}

const ITEMS: &str = "
    CREATE ROLE app;
    CREATE TABLE items (id INT PRIMARY KEY, parent_id INT);
    GRANT SELECT, UPDATE (parent_id) ON items TO app;
    CREATE VIEW item_view AS SELECT id, parent_id FROM items;
    GRANT SELECT, UPDATE ON item_view TO app;
";

fn items_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(deployment(["app"], []))
        .with_acting_role("app")
        .with_rls_audit_table_name("_rls_audit")
        .with_rls_exemption_function("write_is_exempt")
        .with_attached_tables("shared", ["items"])
}

/// Creates the files `label` from `earlier`, applies `migration`, translated
/// against the schema `earlier` leaves, to them, and returns them with the
/// migration's script.
fn migrate(
    label: &str,
    earlier: &str,
    migration: &str,
    options: &Pg2SqliteOptions,
) -> (Files, Script<String>) {
    let schema = Pg2Sqlite::default().sql(earlier).expect("parse").build_schema().expect("schema");
    let migration = Pg2Sqlite::default()
        .sql(migration)
        .expect("parse")
        .translate_to_sql_with_schema(&schema, options)
        .expect("translate the migration");
    let script = translate(earlier, options).expect("translate");
    let files = Files::new(label);
    let mut alice = files.connect("alice");
    run(&mut alice, script.initialize(FreshFiles::Both).expect("initialize"));
    run(&mut alice, migration.initialize(FreshFiles::Both).expect("initialize the migration"));
    (files, migration)
}

/// The names of the views a connection's `temp` schema holds.
fn temp_views(connection: &mut SqliteConnection) -> Vec<String> {
    diesel::table! {
        temp.sqlite_master (name) {
            name -> Text,
            #[sql_name = "type"]
            kind -> Text,
        }
    }
    sqlite_master::table
        .filter(sqlite_master::kind.eq("view"))
        .select(sqlite_master::name)
        .order(sqlite_master::name)
        .load(connection)
        .expect("read the temp catalogue")
}

/// A trigger a later batch puts on a view an earlier batch placed in `temp`
/// replaces the view's write on a new connection running the later batch's
/// connection program alone.
#[test]
fn a_later_instead_of_trigger_on_an_earlier_temp_view_reaches_a_new_connection() {
    diesel::table! {
        items (id) {
            id -> Integer,
            parent_id -> Nullable<Integer>,
        }
    }
    let (files, migration) = migrate(
        "later-instead-of-trigger",
        ITEMS,
        "CREATE FUNCTION pin_parent() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN UPDATE items SET parent_id = 42 WHERE id = OLD.id; RETURN NEW; END $$;
         CREATE TRIGGER pinned INSTEAD OF UPDATE ON item_view
             FOR EACH ROW EXECUTE FUNCTION pin_parent();",
        &items_options(),
    );
    {
        let mut alice = files.connect("alice");
        set_write_exempt(true);
        diesel::insert_into(items::table)
            .values(items::id.eq(1))
            .execute(&mut alice)
            .expect("seed");
        set_write_exempt(false);
    }
    let mut bob = files.connect("bob");
    run(&mut bob, migration.connect().expect("the migration's connection program"));
    diesel::sql_query("UPDATE item_view SET parent_id = 5 WHERE id = 1")
        .execute(&mut bob)
        .expect("update through the view");
    let parents: Vec<Option<i32>> =
        items::table.select(items::parent_id).load(&mut bob).expect("read items");
    assert_eq!(parents, [Some(42)]);
}

/// A migration that leaves an earlier table's policy alone recreates its
/// `temp` objects on a new connection running its connection program alone.
#[test]
fn a_migration_alone_connects_an_earlier_policy() {
    let (files, migration) =
        migrate("untouched-policy", PAPERS, "CREATE TABLE extra (id INT);", &placed_options());
    seed_papers(&mut files.connect("alice"));
    let mut bob = files.connect("bob");
    run(&mut bob, migration.connect().expect("the migration's connection program"));
    assert_eq!(visible_papers(&mut bob), [(11, "shared to bob".to_owned())]);
}

/// A view a migration drops over an earlier `temp` view is absent from a new
/// connection, while the view it read stays.
#[test]
fn a_dropped_view_is_absent_from_a_new_connection() {
    let earlier = format!(
        "{ITEMS}
         CREATE VIEW outer_view AS SELECT id FROM item_view;
         GRANT SELECT ON outer_view TO app;"
    );
    let (files, migration) =
        migrate("dropped-view", &earlier, "DROP VIEW outer_view;", &items_options());
    let mut bob = files.connect("bob");
    run(&mut bob, migration.connect().expect("the migration's connection program"));
    assert_eq!(temp_views(&mut bob), ["item_view"]);
}

/// A table whose row-level security a migration disables reads its rows on a
/// new connection, with no `temp` view left standing for it.
#[test]
fn disabling_row_security_reaches_a_new_connection() {
    diesel::table! {
        memos (id) {
            id -> Integer,
            body -> Text,
        }
    }
    let (files, migration) = migrate(
        "disabled-row-security",
        &memos(),
        "ALTER TABLE memos DISABLE ROW LEVEL SECURITY;",
        &items_options(),
    );
    set_write_exempt(true);
    diesel::insert_into(memos::table)
        .values((memos::id.eq(1), memos::body.eq("kept")))
        .execute(&mut files.connect("alice"))
        .expect("seed");
    set_write_exempt(false);
    let mut bob = files.connect("bob");
    run(&mut bob, migration.connect().expect("the migration's connection program"));
    assert_eq!(temp_views(&mut bob), ["item_view"]);
    let bodies: Vec<String> = memos::table.select(memos::body).load(&mut bob).expect("read memos");
    assert_eq!(bodies, ["kept"]);
}

/// A migration initializes no fresh file, which would lack every earlier
/// object, and its refusal names the translation that does.
#[test]
fn a_migration_initializes_no_fresh_file() {
    let (_files, migration) =
        migrate("fresh-file-refusal", ITEMS, "CREATE TABLE extra (id INT);", &items_options());
    for files in [FreshFiles::Main, FreshFiles::Attached] {
        let Err(error @ Error::Initialization(_)) = migration.initialize(files) else {
            panic!("{files:?} initializes a fresh file from a migration");
        };
        assert!(error.to_string().contains("whole history"), "{error}");
    }
}

#[test]
fn a_seed_write_through_a_temp_view_is_initialized() {
    let pg = format!("{PAPERS} INSERT INTO papers VALUES (1, 'seed');");
    let script = translate(&pg, &placed_options()).expect("translate");
    let files = Files::new("seed-write");
    let mut connection = files.connect("alice");
    set_write_exempt(true);
    run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
    set_write_exempt(false);
    let seeded: Vec<(i32, String)> = papers_rls::table
        .select((papers_rls::id, papers_rls::title))
        .load(&mut connection)
        .expect("seed rows");
    assert_eq!(seeded, [(1, "seed".to_owned())]);
}

/// The `ITEMS` schema with a table whose policy reads the attached `items`.
fn memos() -> String {
    format!(
        "{ITEMS}
         CREATE TABLE memos (id INT PRIMARY KEY, body TEXT NOT NULL);
         GRANT SELECT ON memos TO app;
         ALTER TABLE memos ENABLE ROW LEVEL SECURITY;
         CREATE POLICY m ON memos USING (EXISTS (SELECT 1 FROM items i WHERE i.id = memos.id));"
    )
}

/// The name and definition of each object a connection's `temp` schema holds.
fn temp_objects(connection: &mut SqliteConnection) -> Vec<(String, Option<String>)> {
    diesel::table! {
        temp.sqlite_master (name) {
            name -> Text,
            sql -> Nullable<Text>,
        }
    }
    sqlite_master::table
        .select((sqlite_master::name, sqlite_master::sql))
        .order(sqlite_master::name)
        .load(connection)
        .expect("read the temp catalogue")
}

/// A migration that leaves an earlier translation's connection objects alone
/// recreates each of them on a new connection running its program alone.
#[test]
fn a_migration_recreates_the_earlier_connection_objects() {
    let pinned = format!(
        "{ITEMS}
         CREATE FUNCTION pin_parent() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN UPDATE items SET parent_id = 42 WHERE id = OLD.id; RETURN NEW; END $$;
         CREATE TRIGGER pinned INSTEAD OF UPDATE ON item_view
             FOR EACH ROW EXECUTE FUNCTION pin_parent();
         CREATE TRIGGER pinned_too INSTEAD OF UPDATE ON item_view
             FOR EACH ROW EXECUTE FUNCTION pin_parent();"
    );
    for (index, (earlier, options)) in [
        (PAPERS.to_owned(), placed_options()),
        (ITEMS.to_owned(), items_options()),
        (memos(), items_options()),
        (pinned, items_options()),
    ]
    .into_iter()
    .enumerate()
    {
        let (files, migration) = migrate(
            &format!("recreated-{index}"),
            &earlier,
            "CREATE TABLE unrelated (id INT);",
            &options,
        );
        let script = translate(&earlier, &options).expect("translate");
        let mut carol = files.connect("carol");
        run(&mut carol, script.connect().expect("connection program"));
        let mut bob = files.connect("bob");
        run(&mut bob, migration.connect().expect("the migration's connection program"));
        assert_eq!(temp_objects(&mut bob), temp_objects(&mut carol), "{earlier}");
    }
}

/// A column a migration adds to a table with policies reaches a new
/// connection, which holds every `temp` object of that table as well.
#[test]
fn an_added_column_reaches_a_new_connection_with_the_earlier_temp_objects() {
    diesel::table! {
        memos (id) {
            id -> Integer,
            extra -> Nullable<Integer>,
        }
    }
    let (files, migration) = migrate(
        "added-column",
        &memos(),
        "ALTER TABLE memos ADD COLUMN extra INT;",
        &items_options(),
    );
    let script = translate(&memos(), &items_options()).expect("translate");
    let mut carol = files.connect("carol");
    run(&mut carol, script.connect().expect("connection program"));
    let mut bob = files.connect("bob");
    run(&mut bob, migration.connect().expect("the migration's connection program"));
    let names = |connection: &mut SqliteConnection| {
        temp_objects(connection).into_iter().map(|(name, _)| name).collect::<Vec<_>>()
    };
    assert_eq!(names(&mut bob), names(&mut carol));
    let extras: Vec<Option<i32>> =
        memos::table.select(memos::extra).load(&mut bob).expect("read the added column");
    assert!(extras.is_empty(), "{extras:?}");
}
