use crate::helpers::run_translated::run_translated_default;

#[test]
fn temporary_table_shadows_permanent_table() {
    let rows = run_translated_default(
        "CREATE TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         INSERT INTO scratch VALUES (1, 't');
         SELECT count(*) FROM public.scratch;",
    );
    assert_eq!(rows, vec![Some("0".to_string())]);
}

#[test]
fn temporary_view_shadows_permanent_view() {
    let rows = run_translated_default(
        "CREATE VIEW seen AS SELECT 1 AS id;
         CREATE TEMP VIEW seen AS SELECT 2 AS id;
         SELECT id FROM public.seen;",
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

#[test]
fn qualified_permanent_writes_bypass_temporary_table() {
    let rows = run_translated_default(
        "CREATE TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         INSERT INTO scratch VALUES (1, 't');
         INSERT INTO public.scratch VALUES (2, 'p');
         UPDATE public.scratch SET tag='permanent' WHERE id=2;
         SELECT tag FROM scratch UNION ALL SELECT tag FROM public.scratch;",
    );
    assert_eq!(rows, vec![Some("t".to_string()), Some("permanent".to_string())]);
}

#[test]
fn dropping_temporary_table_reveals_permanent_table() {
    let rows = run_translated_default(
        "CREATE TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         INSERT INTO scratch VALUES (1, 't');
         INSERT INTO public.scratch VALUES (2, 'p');
         UPDATE public.scratch SET tag='permanent' WHERE id=2;
         DROP TABLE scratch;
         SELECT tag FROM scratch;",
    );
    assert_eq!(rows, vec![Some("permanent".to_string())]);
}

#[test]
fn unqualified_reads_prefer_temporary_view() {
    let rows = run_translated_default(
        "CREATE VIEW seen AS SELECT 1 AS id;
         CREATE TEMP VIEW seen AS SELECT 2 AS id;
         SELECT id FROM seen UNION ALL SELECT id FROM public.seen;",
    );
    assert_eq!(rows, vec![Some("2".to_string()), Some("1".to_string())]);
}

#[test]
fn dropping_temporary_view_reveals_permanent_view() {
    let rows = run_translated_default(
        "CREATE VIEW seen AS SELECT 1 AS id;
         CREATE TEMP VIEW seen AS SELECT 2 AS id;
         DROP VIEW seen;
         SELECT id FROM seen;",
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

#[test]
fn initial_schema_keeps_temporary_and_permanent_relations_apart() {
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

    let schema = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
             CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
             CREATE VIEW seen AS SELECT 1 AS id;
             CREATE TEMP VIEW seen AS SELECT 2 AS id;",
        )
        .unwrap()
        .build_schema()
        .unwrap();
    Pg2Sqlite::default()
        .sql("SELECT (SELECT count(*) FROM public.scratch) + (SELECT id FROM public.seen);")
        .unwrap()
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect("temporary and permanent relations occupy separate databases");
}

#[test]
fn permanent_trigger_writes_bypass_temporary_shadow() {
    let rows = run_translated_default(
        "CREATE TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         CREATE TABLE source_rows (id INTEGER PRIMARY KEY);
         CREATE FUNCTION write_scratch() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             INSERT INTO public.scratch VALUES (NEW.id, 'permanent'), (NEW.id + 1, 'remove');
             UPDATE public.scratch SET tag = 'updated' WHERE id = NEW.id;
             DELETE FROM public.scratch WHERE id = NEW.id + 1;
             RETURN NEW;
         END;
         $$;
         CREATE TRIGGER write_scratch AFTER INSERT ON source_rows
         FOR EACH ROW EXECUTE FUNCTION write_scratch();
         INSERT INTO source_rows VALUES (1);
         SELECT tag FROM public.scratch UNION ALL SELECT tag FROM scratch;",
    );
    assert_eq!(rows, vec![Some("updated".to_string())]);
}

#[test]
fn instead_of_trigger_writes_bypass_temporary_shadow() {
    let rows = run_translated_default(
        "CREATE TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         CREATE VIEW source_rows AS SELECT 1 AS id;
         CREATE FUNCTION write_scratch() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             INSERT INTO public.scratch VALUES (1, 'permanent'), (2, 'remove');
             UPDATE public.scratch SET tag = 'updated' WHERE id = 1;
             DELETE FROM public.scratch WHERE id = 2;
             RETURN NEW;
         END;
         $$;
         CREATE TRIGGER write_scratch INSTEAD OF INSERT ON source_rows
         FOR EACH ROW EXECUTE FUNCTION write_scratch();
         INSERT INTO source_rows VALUES (1);
         SELECT tag FROM public.scratch UNION ALL SELECT tag FROM scratch;",
    );
    assert_eq!(rows, vec![Some("updated".to_string())]);
}

#[test]
fn temporary_trigger_cannot_write_a_shadowed_permanent_table() {
    for action in [
        "INSERT INTO public.scratch VALUES (NEW.id, 'permanent')",
        "UPDATE public.scratch SET tag = 'updated' WHERE id = NEW.id",
        "DELETE FROM public.scratch WHERE id = NEW.id",
    ] {
        crate::helpers::translate::assert_refused(
            &format!(
                "CREATE TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
                 CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
                 CREATE TEMP TABLE source_rows (id INTEGER PRIMARY KEY);
                 CREATE FUNCTION write_scratch() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN {action}; RETURN NEW; END; $$;
                 CREATE TRIGGER write_scratch AFTER INSERT ON source_rows
                 FOR EACH ROW EXECUTE FUNCTION write_scratch();"
            ),
            "cannot write to qualified relation \"main.scratch\" from a temporary trigger",
        );
    }
}

#[test]
fn pg_temp_qualification_preserves_temporary_shadowing() {
    let rows = run_translated_default(
        "CREATE TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         CREATE TABLE pg_temp.scratch (id INTEGER PRIMARY KEY, tag TEXT);
         INSERT INTO public.scratch VALUES (1, 'permanent');
         INSERT INTO pg_temp.scratch VALUES (1, 'temporary'), (2, 'remove');
         UPDATE pg_temp.scratch SET tag = 'updated' WHERE id = 1;
         DELETE FROM pg_temp.scratch WHERE id = 2;
         SELECT tag FROM pg_temp.scratch UNION ALL SELECT tag FROM public.scratch;",
    );
    assert_eq!(rows, vec![Some("updated".to_string()), Some("permanent".to_string())]);
}

#[test]
fn temporary_trigger_writes_qualified_temporary_table() {
    let rows = run_translated_default(
        "CREATE TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag TEXT);
         CREATE TEMP TABLE source_rows (id INTEGER PRIMARY KEY);
         CREATE FUNCTION write_scratch() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             INSERT INTO pg_temp.scratch VALUES (NEW.id, 'temporary'), (NEW.id + 1, 'remove');
             UPDATE pg_temp.scratch SET tag = 'updated' WHERE id = NEW.id;
             DELETE FROM pg_temp.scratch WHERE id = NEW.id + 1;
             RETURN NEW;
         END;
         $$;
         CREATE TRIGGER write_scratch AFTER INSERT ON source_rows
         FOR EACH ROW EXECUTE FUNCTION write_scratch();
         INSERT INTO source_rows VALUES (1);
         SELECT tag FROM pg_temp.scratch UNION ALL SELECT tag FROM public.scratch;",
    );
    assert_eq!(rows, vec![Some("updated".to_string())]);
}

#[test]
fn shadowed_tables_keep_unique_indexes_on_their_owners() {
    for temporary_index in ["scratch_id", "scratch_tag"] {
        let rows = run_translated_default(&format!(
            "CREATE TABLE scratch (id INTEGER, tag INTEGER);
             CREATE TEMP TABLE scratch (id INTEGER, tag INTEGER);
             CREATE UNIQUE INDEX scratch_id ON public.scratch(id);
             CREATE UNIQUE INDEX {temporary_index} ON scratch(tag);
             INSERT INTO public.scratch VALUES (1,10),(1,20),(2,10) ON CONFLICT DO NOTHING;
             INSERT INTO scratch VALUES (1,10),(1,20),(2,10) ON CONFLICT DO NOTHING;
             SELECT sum(id) FROM public.scratch UNION ALL SELECT sum(id) FROM scratch;"
        ));
        assert_eq!(rows, vec![Some("3".to_string()), Some("2".to_string())]);
    }
}

#[test]
fn dropping_shadowing_index_preserves_permanent_unique_index() {
    let rows = run_translated_default(
        "CREATE TABLE scratch (id INTEGER, tag INTEGER);
         CREATE TEMP TABLE scratch (id INTEGER, tag INTEGER);
         CREATE UNIQUE INDEX scratch_id ON public.scratch(id);
         CREATE UNIQUE INDEX scratch_id ON scratch(tag);
         INSERT INTO public.scratch VALUES (1,10),(1,20),(2,10) ON CONFLICT DO NOTHING;
         INSERT INTO scratch VALUES (1,10),(1,20),(2,10) ON CONFLICT DO NOTHING;
         DROP INDEX scratch_id;
         INSERT INTO scratch VALUES (2,10);
         INSERT INTO public.scratch VALUES (1,30) ON CONFLICT DO NOTHING;
         SELECT count(*) FROM public.scratch UNION ALL SELECT count(*) FROM scratch;",
    );
    assert_eq!(rows, vec![Some("2".to_string()), Some("3".to_string())]);
}

#[test]
fn dropping_qualified_permanent_index_preserves_temporary_unique_index() {
    let rows = run_translated_default(
        "CREATE TABLE scratch (id INTEGER, tag INTEGER);
         CREATE TEMP TABLE scratch (id INTEGER, tag INTEGER);
         CREATE UNIQUE INDEX scratch_id ON public.scratch(id);
         CREATE UNIQUE INDEX scratch_id ON scratch(tag);
         INSERT INTO public.scratch VALUES (1,10);
         INSERT INTO scratch VALUES (1,10);
         DROP INDEX public.scratch_id;
         INSERT INTO public.scratch VALUES (1,30);
         INSERT INTO scratch VALUES (2,10) ON CONFLICT DO NOTHING;
         SELECT count(*) FROM public.scratch UNION ALL SELECT count(*) FROM scratch;",
    );
    assert_eq!(rows, vec![Some("2".to_string()), Some("1".to_string())]);
}

#[test]
fn shadowed_tables_allow_same_and_different_index_names() {
    for temporary_index in ["scratch_id", "scratch_tag"] {
        for temporary_first in [false, true] {
            let permanent = "CREATE INDEX scratch_id ON public.scratch(id);";
            let temporary = format!("CREATE INDEX {temporary_index} ON scratch(tag);");
            let indexes = if temporary_first {
                format!("{temporary} {permanent}")
            } else {
                format!("{permanent} {temporary}")
            };
            let rows = run_translated_default(&format!(
                "CREATE TABLE scratch (id INTEGER, tag INTEGER);
                 CREATE TEMP TABLE scratch (id INTEGER, tag INTEGER);
                 {indexes}
                 SELECT count(*) FROM public.scratch UNION ALL SELECT count(*) FROM scratch;"
            ));
            assert_eq!(rows, vec![Some("0".to_string()), Some("0".to_string())]);
        }
    }
}

#[test]
fn dropping_temporary_indexes_reveals_permanent_index() {
    for (temporary_index, dropped_index) in [
        ("scratch_id", "scratch_id"),
        ("scratch_id", "pg_temp.scratch_id"),
        ("scratch_tag", "scratch_tag"),
    ] {
        let rows = run_translated_default(&format!(
            "CREATE TABLE scratch (id INTEGER);
             CREATE TEMP TABLE scratch (id INTEGER);
             CREATE UNIQUE INDEX scratch_id ON public.scratch(id);
             CREATE UNIQUE INDEX {temporary_index} ON scratch(id);
             INSERT INTO public.scratch VALUES (1);
             INSERT INTO scratch VALUES (1);
             DROP INDEX {dropped_index};
             DROP INDEX scratch_id;
             INSERT INTO public.scratch VALUES (1);
             INSERT INTO scratch VALUES (1);
             SELECT count(*) FROM public.scratch UNION ALL SELECT count(*) FROM scratch;"
        ));
        assert_eq!(rows, vec![Some("2".to_string()), Some("2".to_string())]);
    }
}
