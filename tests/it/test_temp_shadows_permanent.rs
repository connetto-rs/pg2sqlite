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

#[test]
fn temp_shadows_statement_targets_preserve_their_database() {
    for temporary_first in [false, true] {
        let permanent = "CREATE TABLE public.scratch (id INTEGER PRIMARY KEY, tag INTEGER);";
        let temporary = "CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag INTEGER);";
        let tables = if temporary_first {
            format!("{temporary} {permanent}")
        } else {
            format!("{permanent} {temporary}")
        };
        for relation in ["public.scratch", "pg_temp.scratch", "scratch"] {
            let permanent = relation == "public.scratch";
            let other = if permanent { "pg_temp.scratch" } else { "public.scratch" };
            let renamed = if permanent { "public.renamed" } else { "pg_temp.renamed" };
            for (statement, expected) in [
                (format!("INSERT INTO {relation} VALUES (3,30) RETURNING tag;"), "30"),
                (
                    format!("UPDATE {relation} SET tag = tag + 1 RETURNING tag;"),
                    if permanent { "11" } else { "21" },
                ),
                (
                    format!("DELETE FROM {relation} RETURNING tag;"),
                    if permanent { "10" } else { "20" },
                ),
                (
                    format!("TRUNCATE {relation}; SELECT tag FROM {other};"),
                    if permanent { "20" } else { "10" },
                ),
                (
                    format!("DROP TABLE {relation}; SELECT tag FROM {other};"),
                    if permanent { "20" } else { "10" },
                ),
                (
                    format!(
                        "ALTER TABLE {relation} ADD COLUMN added INTEGER DEFAULT 7;
                         SELECT added FROM {relation};"
                    ),
                    "7",
                ),
                (
                    format!("ALTER TABLE {relation} DROP COLUMN tag; SELECT id FROM {relation};"),
                    if permanent { "1" } else { "2" },
                ),
                (
                    format!(
                        "ALTER TABLE {relation} RENAME COLUMN tag TO renamed;
                         SELECT renamed FROM {relation};"
                    ),
                    if permanent { "10" } else { "20" },
                ),
                (
                    format!(
                        "ALTER TABLE {relation} RENAME TO renamed;
                         SELECT tag FROM {renamed};"
                    ),
                    if permanent { "10" } else { "20" },
                ),
                (
                    format!(
                        "COMMENT ON TABLE {relation} IS 'table';
                         COMMENT ON COLUMN {relation}.tag IS 'column';
                         SELECT tag FROM {relation};"
                    ),
                    if permanent { "10" } else { "20" },
                ),
            ] {
                let rows = run_translated_default(&format!(
                    "{tables}
                     INSERT INTO public.scratch VALUES (1,10);
                     INSERT INTO pg_temp.scratch VALUES (2,20);
                     {statement}"
                ));
                assert_eq!(rows, vec![Some(expected.to_string())], "{statement}");
            }
            let rows = run_translated_default(&format!(
                "{tables}
                 INSERT INTO public.scratch VALUES (1,10);
                 INSERT INTO pg_temp.scratch VALUES (2,20);
                 INSERT INTO {relation} VALUES (1,100),(2,200)
                 ON CONFLICT (id) DO UPDATE SET tag = excluded.tag;
                 SELECT tag FROM public.scratch UNION ALL SELECT tag FROM pg_temp.scratch;"
            ));
            let expected = if permanent { ["100", "200", "20"] } else { ["10", "100", "200"] };
            assert_eq!(rows, expected.map(|value| Some(value.to_string())));
        }
    }
}

#[test]
fn temp_shadows_generated_identifiers_keep_separate_counters() {
    for temporary_first in [false, true] {
        for column in [
            "SERIAL PRIMARY KEY",
            "INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY",
            "INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY",
        ] {
            let permanent = format!("CREATE TABLE public.scratch (id {column}, tag INTEGER);");
            let temporary = format!("CREATE TEMP TABLE scratch (id {column}, tag INTEGER);");
            let tables = if temporary_first {
                format!("{temporary} {permanent}")
            } else {
                format!("{permanent} {temporary}")
            };
            let rows = run_translated_default(&format!(
                "{tables}
                 INSERT INTO public.scratch (tag) VALUES (10),(11);
                 INSERT INTO pg_temp.scratch (tag) VALUES (20);
                 SELECT sum(id) FROM public.scratch UNION ALL SELECT sum(id) FROM pg_temp.scratch;"
            ));
            assert_eq!(rows, vec![Some("3".to_string()), Some("1".to_string())]);
        }
    }
}

#[test]
fn temp_shadows_named_constraints_keep_separate_owners() {
    for temporary_first in [false, true] {
        for constraint in [
            "CONSTRAINT same_check CHECK (tag > 0)",
            "CONSTRAINT same_unique UNIQUE (tag)",
            "CONSTRAINT same_primary PRIMARY KEY (id)",
        ] {
            let permanent =
                format!("CREATE TABLE public.scratch (id INTEGER, tag INTEGER, {constraint});");
            let temporary =
                format!("CREATE TEMP TABLE scratch (id INTEGER, tag INTEGER, {constraint});");
            let tables = if temporary_first {
                format!("{temporary} {permanent}")
            } else {
                format!("{permanent} {temporary}")
            };
            let rows = run_translated_default(&format!(
                "{tables}
                 INSERT INTO public.scratch VALUES (1,10);
                 INSERT INTO pg_temp.scratch VALUES (1,10);
                 SELECT tag FROM public.scratch UNION ALL SELECT tag FROM pg_temp.scratch;"
            ));
            assert_eq!(rows, vec![Some("10".to_string()), Some("10".to_string())]);
        }
    }
}

#[test]
fn temp_shadows_foreign_keys_keep_their_qualified_parent() {
    for temporary_first in [false, true] {
        let permanent = "CREATE TABLE public.scratch (id INTEGER PRIMARY KEY);";
        let temporary = "CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY);";
        let tables = if temporary_first {
            format!("{temporary} {permanent}")
        } else {
            format!("{permanent} {temporary}")
        };
        for (storage, parent, identifier) in
            [("", "public.scratch", 10), ("TEMP", "pg_temp.scratch", 20)]
        {
            for columns in [
                format!("id INTEGER REFERENCES {parent}(id)"),
                format!("id INTEGER, FOREIGN KEY (id) REFERENCES {parent}(id)"),
            ] {
                let rows = run_translated_default(&format!(
                    "{tables}
                     INSERT INTO public.scratch VALUES (10);
                     INSERT INTO pg_temp.scratch VALUES (20);
                     CREATE {storage} TABLE child ({columns});
                     INSERT INTO child VALUES ({identifier});
                     SELECT id FROM child;"
                ));
                assert_eq!(rows, vec![Some(identifier.to_string())]);
            }
        }
    }
}

#[test]
fn temp_shadows_views_keep_their_bound_table() {
    for temporary_first in [false, true] {
        for relation in ["public.scratch", "pg_temp.scratch", "scratch"] {
            let permanent = "CREATE TABLE public.scratch (id INTEGER);";
            let temporary = "CREATE TEMP TABLE scratch (id INTEGER);";
            let tables = if temporary_first {
                format!("{temporary} {permanent}")
            } else {
                format!("{permanent} {temporary}")
            };
            let rows = run_translated_default(&format!(
                "{tables}
                 INSERT INTO public.scratch VALUES (10);
                 INSERT INTO pg_temp.scratch VALUES (20);
                 CREATE VIEW seen AS SELECT id FROM {relation};
                 SELECT id FROM seen;"
            ));
            let expected = if relation == "public.scratch" { "10" } else { "20" };
            assert_eq!(rows, vec![Some(expected.to_string())]);
        }
    }
}

/// A view naming a quoted schema keeps reading the permanent rows.
#[test]
fn temp_shadows_views_keep_a_quoted_schema_binding() {
    let rows = run_translated_default(
        r#"CREATE TABLE "public".scratch (id INTEGER);
         INSERT INTO "public".scratch VALUES (10);
         CREATE TEMP TABLE scratch (id INTEGER);
         INSERT INTO pg_temp.scratch VALUES (20);
         CREATE VIEW seen AS SELECT id FROM "public".scratch;
         SELECT id FROM seen;"#,
    );
    assert_eq!(rows, vec![Some("10".into())]);
}

/// Reading a temporary view promotes a view that shadows a permanent view.
#[test]
fn temp_shadows_views_reading_a_temporary_view_become_temporary() {
    let rows = run_translated_default(
        "CREATE VIEW seen AS SELECT 10 AS id;
         CREATE TEMP VIEW scratch AS SELECT 20 AS id;
         CREATE VIEW seen AS SELECT id FROM scratch;
         SELECT id FROM public.seen
         UNION ALL SELECT id FROM pg_temp.seen ORDER BY id;",
    );
    assert_eq!(rows, vec![Some("10".into()), Some("20".into())]);
}

/// A table function leaves a shadowed permanent view in the permanent schema.
#[test]
fn temp_shadows_views_over_a_table_function_stay_permanent() {
    let rows = run_translated_default(
        "CREATE VIEW seen AS SELECT value AS id FROM json_array_elements_text('[10]');
         CREATE TEMP VIEW seen AS SELECT '20' AS id;
         SELECT id FROM public.seen
         UNION ALL SELECT id FROM pg_temp.seen ORDER BY id;",
    );
    assert_eq!(rows, vec![Some("10".into()), Some("20".into())]);
}

#[test]
fn temp_shadows_views_created_before_shadowing_keep_their_bound_table() {
    for relation in ["public.scratch", "scratch"] {
        let rows = run_translated_default(&format!(
            "CREATE TABLE public.scratch (id INTEGER);
             INSERT INTO public.scratch VALUES (10);
             CREATE VIEW seen AS SELECT id FROM {relation};
             CREATE TEMP TABLE scratch (id INTEGER);
             INSERT INTO pg_temp.scratch VALUES (20);
             SELECT id FROM seen;"
        ));
        assert_eq!(rows, vec![Some("10".to_string())]);
    }
}

#[test]
fn temp_shadows_views_created_before_permanent_table_keep_their_bound_table() {
    for relation in ["pg_temp.scratch", "scratch"] {
        let rows = run_translated_default(&format!(
            "CREATE TEMP TABLE scratch (id INTEGER);
             INSERT INTO pg_temp.scratch VALUES (20);
             CREATE VIEW seen AS SELECT id FROM {relation};
             CREATE TABLE public.scratch (id INTEGER);
             INSERT INTO public.scratch VALUES (10);
             SELECT id FROM seen;"
        ));
        assert_eq!(rows, vec![Some("20".to_string())]);
    }
}

#[test]
fn temp_shadows_views_bind_both_databases_and_preserve_ctes() {
    let rows = run_translated_default(
        "CREATE TABLE public.scratch (id INTEGER);
         INSERT INTO public.scratch VALUES (10);
         CREATE TEMP TABLE scratch (id INTEGER);
         INSERT INTO pg_temp.scratch VALUES (20);
         CREATE VIEW seen AS
             WITH scratch AS (SELECT 30 AS id)
             SELECT id FROM public.scratch
             UNION ALL SELECT id FROM pg_temp.scratch
             UNION ALL SELECT id FROM scratch;
         SELECT id FROM seen ORDER BY id;",
    );
    assert_eq!(rows, vec![Some("10".into()), Some("20".into()), Some("30".into())]);
}

#[test]
fn temp_shadows_views_promote_without_main_qualification() {
    let rows = run_translated_default(
        "CREATE TABLE public.scratch (id INTEGER);
         CREATE VIEW seen AS SELECT id FROM public.scratch;
         CREATE TEMP TABLE scratch (id INTEGER);
         INSERT INTO pg_temp.scratch VALUES (20);
         CREATE VIEW seen AS SELECT id FROM scratch;
         SELECT id FROM seen;",
    );
    assert_eq!(rows, vec![Some("20".into())]);
}

#[test]
fn temp_shadows_dropping_trigger_preserves_the_other_owner() {
    for temporary_first in [false, true] {
        let permanent = "CREATE TABLE public.scratch (id INTEGER PRIMARY KEY);";
        let temporary = "CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY);";
        let tables = if temporary_first {
            format!("{temporary} {permanent}")
        } else {
            format!("{permanent} {temporary}")
        };
        for (relation, expected) in
            [("public.scratch", "4"), ("pg_temp.scratch", "3"), ("scratch", "3")]
        {
            let rows = run_translated_default(&format!(
                "{tables}
                 CREATE TABLE audit (id INTEGER);
                 CREATE FUNCTION record_row() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN INSERT INTO audit VALUES (NEW.id); RETURN NEW; END; $$;
                 CREATE TRIGGER record_row AFTER INSERT ON public.scratch
                 FOR EACH ROW EXECUTE FUNCTION record_row();
                 CREATE TRIGGER record_row AFTER INSERT ON pg_temp.scratch
                 FOR EACH ROW EXECUTE FUNCTION record_row();
                 DROP TRIGGER record_row ON {relation};
                 INSERT INTO public.scratch VALUES (3);
                 INSERT INTO pg_temp.scratch VALUES (4);
                 SELECT id FROM audit;"
            ));
            assert_eq!(rows, vec![Some(expected.to_string())]);
        }
    }
}

#[test]
fn temp_shadows_table_events_preserve_the_other_trigger_owner() {
    for temporary_first in [false, true] {
        let permanent = "CREATE TABLE public.scratch (id INTEGER PRIMARY KEY);";
        let temporary = "CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY);";
        let tables = if temporary_first {
            format!("{temporary} {permanent}")
        } else {
            format!("{permanent} {temporary}")
        };
        for (event, expected) in [
            ("DROP TABLE scratch; INSERT INTO public.scratch VALUES (3);", vec!["3"]),
            (
                "ALTER TABLE scratch RENAME TO moved;
                 INSERT INTO public.scratch VALUES (3);
                 INSERT INTO pg_temp.moved VALUES (4);",
                vec!["3", "4"],
            ),
        ] {
            let sql = format!(
                "{tables}
                 CREATE TABLE audit (id INTEGER);
                 CREATE FUNCTION record_row() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN INSERT INTO audit VALUES (NEW.id); RETURN NEW; END; $$;
                 CREATE TRIGGER record_row AFTER INSERT ON public.scratch
                 FOR EACH ROW EXECUTE FUNCTION record_row();
                 CREATE TRIGGER record_row AFTER INSERT ON pg_temp.scratch
                 FOR EACH ROW EXECUTE FUNCTION record_row();
                 {event}
                 SELECT id FROM audit ORDER BY id;"
            );
            let rows = run_translated_default(&sql);
            assert_eq!(
                rows,
                expected.into_iter().map(|value| Some(value.to_string())).collect::<Vec<_>>(),
                "{event}"
            );
        }
    }
}

#[test]
fn temp_shadows_merged_trigger_slots_keep_separate_owners() {
    for temporary_first in [false, true] {
        let permanent = "CREATE TABLE public.scratch (id INTEGER PRIMARY KEY);";
        let temporary = "CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY);";
        let tables = if temporary_first {
            format!("{temporary} {permanent}")
        } else {
            format!("{permanent} {temporary}")
        };
        let sql = format!(
            "{tables}
             CREATE TABLE audit (id INTEGER);
             CREATE FUNCTION record_row() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN INSERT INTO audit VALUES (NEW.id); RETURN NEW; END; $$;
             CREATE TRIGGER record_row AFTER INSERT ON public.scratch
             FOR EACH ROW EXECUTE FUNCTION record_row();
             CREATE TRIGGER record_row AFTER INSERT ON pg_temp.scratch
             FOR EACH ROW EXECUTE FUNCTION record_row();
             CREATE TRIGGER record_second AFTER INSERT ON public.scratch
             FOR EACH ROW EXECUTE FUNCTION record_row();
             CREATE TRIGGER record_second AFTER INSERT ON scratch
             FOR EACH ROW EXECUTE FUNCTION record_row();
             INSERT INTO public.scratch VALUES (3);
             INSERT INTO scratch VALUES (4);
             SELECT id FROM audit ORDER BY id;"
        );
        let emitted = crate::helpers::translate::translate_default(&sql);
        assert!(!emitted.contains("\"pg_temp."));
        let rows = run_translated_default(&sql);
        assert_eq!(rows, ["3", "3", "4", "4"].map(|value| Some(value.to_string())));
    }
}

/// Dropping a seeded column-specific trigger preserves the other owner's frame.
#[test]
fn temp_shadows_dropping_seeded_column_trigger_preserves_other_owner() {
    use diesel::{RunQueryDsl, connection::SimpleConnection};
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

    let source = "
        CREATE TABLE scratch (id INTEGER PRIMARY KEY, tag INTEGER, note INTEGER);
        CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY, tag INTEGER, note INTEGER);
        CREATE TABLE audit (value INTEGER);
        CREATE FUNCTION record_tag() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN INSERT INTO audit VALUES (NEW.tag); RETURN NEW; END; $$;
        CREATE FUNCTION record_note() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN INSERT INTO audit VALUES (NEW.note); RETURN NEW; END; $$;
        CREATE TRIGGER record_tag AFTER UPDATE OF tag ON public.scratch
        FOR EACH ROW EXECUTE FUNCTION record_tag();
        CREATE TRIGGER record_note AFTER UPDATE OF note ON public.scratch
        FOR EACH ROW EXECUTE FUNCTION record_note();
        CREATE TRIGGER record_tag AFTER UPDATE OF tag ON pg_temp.scratch
        FOR EACH ROW EXECUTE FUNCTION record_tag();
        CREATE TRIGGER record_note AFTER UPDATE OF note ON pg_temp.scratch
        FOR EACH ROW EXECUTE FUNCTION record_note();";
    let options = Pg2SqliteOptions::default();
    let parsed = Pg2Sqlite::default().sql(source).unwrap();
    let schema = parsed.build_schema().unwrap();
    let setup = Pg2Sqlite::default().sql(source).unwrap().translate(&options).unwrap();
    let mut connection = crate::helpers::establish_connection();
    for statement in setup.into_vec() {
        connection.batch_execute(&statement.to_string()).unwrap();
    }
    let migration = Pg2Sqlite::default()
        .sql(
            "DROP TRIGGER record_tag ON scratch;
             INSERT INTO public.scratch VALUES (1, 0, 0);
             INSERT INTO pg_temp.scratch VALUES (1, 0, 0);
             UPDATE public.scratch SET tag = 3, note = 4;
             UPDATE pg_temp.scratch SET tag = 5, note = 6;",
        )
        .unwrap()
        .translate_with_schema(&schema, &options)
        .unwrap()
        .into_vec();
    for statement in &migration {
        connection.batch_execute(&statement.to_string()).unwrap();
    }
    let values =
        diesel::dsl::sql::<diesel::sql_types::Integer>("SELECT value FROM audit ORDER BY value")
            .load::<i32>(&mut connection)
            .unwrap();
    assert_eq!(
        values,
        vec![3, 4, 6],
        "{}",
        migration.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    );
}

/// Permanent table events keep a renamed trigger on its owner.
#[test]
fn temp_shadows_permanent_table_events_preserve_trigger_owner() {
    for event in [
        "ALTER TABLE scratch RENAME TO moved;
         INSERT INTO moved VALUES (3);
         DROP TABLE moved;",
        "INSERT INTO scratch VALUES (3);
         DROP TABLE scratch;",
        "ALTER TABLE public.scratch RENAME TO moved;
         INSERT INTO moved VALUES (3);
         DROP TABLE public.moved;",
        "INSERT INTO scratch VALUES (3);
         DROP TABLE public.scratch;",
    ] {
        let rows = run_translated_default(&format!(
            "CREATE TABLE scratch (id INTEGER PRIMARY KEY);
             CREATE TABLE audit (id INTEGER);
             CREATE FUNCTION record_row() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN INSERT INTO audit VALUES (NEW.id); RETURN NEW; END; $$;
             CREATE TRIGGER record_row AFTER INSERT ON scratch
             FOR EACH ROW EXECUTE FUNCTION record_row();
             {event}
             CREATE TEMP TABLE scratch (id INTEGER PRIMARY KEY);
             INSERT INTO scratch VALUES (4);
             SELECT id FROM audit;"
        ));
        assert_eq!(rows, vec![Some("3".to_string())]);
    }
}

/// Inaccessible column-trigger frames leave accessible table writes intact.
#[test]
fn temp_shadows_inaccessible_frames_preserve_accessible_writes() {
    use diesel::{RunQueryDsl, connection::SimpleConnection};
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

    let options = Pg2SqliteOptions::default()
        .with_deployment(crate::helpers::deployment::deployment(["app_user"], []))
        .with_acting_role("app_user");
    let source = "CREATE ROLE app_user;
         CREATE TABLE private_rows (id INTEGER PRIMARY KEY, tag INTEGER, note INTEGER);
         CREATE TABLE visible_rows (id INTEGER PRIMARY KEY);
         GRANT SELECT, INSERT, UPDATE, DELETE ON visible_rows TO app_user;
         CREATE FUNCTION record_row() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN INSERT INTO visible_rows VALUES (NEW.id); RETURN NEW; END; $$;
         CREATE TRIGGER record_tag AFTER UPDATE OF tag ON private_rows
         FOR EACH ROW EXECUTE FUNCTION record_row();
         CREATE TRIGGER record_note AFTER UPDATE OF note ON private_rows
         FOR EACH ROW EXECUTE FUNCTION record_row();
         CREATE TRIGGER record_both AFTER UPDATE ON private_rows
         FOR EACH ROW EXECUTE FUNCTION record_row();";
    let schema = Pg2Sqlite::default().sql(source).unwrap().build_schema().unwrap();
    let setup = Pg2Sqlite::default().sql(source).unwrap().translate(&options).unwrap();
    let mut connection = crate::helpers::establish_connection();
    for statement in setup.into_vec() {
        connection.batch_execute(&statement.to_string()).unwrap();
    }
    let migration = Pg2Sqlite::default()
        .sql(
            "DROP TRIGGER record_both ON private_rows;
             INSERT INTO visible_rows VALUES (7);",
        )
        .unwrap()
        .translate_with_schema(&schema, &options)
        .unwrap();
    for statement in migration.into_vec() {
        connection.batch_execute(&statement.to_string()).unwrap();
    }
    let values = diesel::dsl::sql::<diesel::sql_types::Integer>("SELECT id FROM visible_rows")
        .load::<i32>(&mut connection)
        .unwrap();
    assert_eq!(values, vec![7]);
}

/// A trigger drop naming no table, which PostgreSQL rejects, still drops the
/// emitted trigger by name.
#[test]
fn temp_shadows_tableless_trigger_drop_drops_by_name() {
    let translated = crate::helpers::translate::translate_default(
        "CREATE TABLE scratch (id INTEGER PRIMARY KEY);
         CREATE FUNCTION record_row() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RETURN NEW; END; $$;
         CREATE TRIGGER record_row AFTER INSERT ON scratch
         FOR EACH ROW EXECUTE FUNCTION record_row();
         DROP TRIGGER record_row;",
    );
    assert!(translated.ends_with("DROP TRIGGER IF EXISTS record_row"), "{translated}");
}

/// Same-named column triggers on shadowed tables keep separate recording twins.
#[test]
fn temp_shadows_column_frames_keep_separate_owners() {
    use core::fmt::Write as _;

    for temporary_first in [false, true] {
        let permanent = "CREATE TABLE scratch (note INTEGER);";
        let temporary = "CREATE TEMP TABLE scratch (note INTEGER);";
        let tables = if temporary_first {
            format!("{temporary} {permanent}")
        } else {
            format!("{permanent} {temporary}")
        };
        let owners = if temporary_first {
            ["pg_temp.scratch", "public.scratch"]
        } else {
            ["public.scratch", "pg_temp.scratch"]
        };
        let mut source = format!(
            "{tables}
             CREATE FUNCTION noop() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN RETURN NEW; END; $$;"
        );
        for owner in owners {
            write!(
                source,
                "CREATE TRIGGER all_rows AFTER UPDATE ON {owner}
                 FOR EACH ROW EXECUTE FUNCTION noop();
                 CREATE TRIGGER record_note AFTER UPDATE OF note ON {owner}
                 FOR EACH ROW EXECUTE FUNCTION noop();"
            )
            .unwrap();
        }
        source.push_str(
            "INSERT INTO public.scratch VALUES (0);
             INSERT INTO pg_temp.scratch VALUES (0);
             UPDATE public.scratch SET note = 2;
             UPDATE pg_temp.scratch SET note = 3;",
        );
        for (event, expected) in [
            ("SELECT (SELECT note FROM public.scratch) + (SELECT note FROM pg_temp.scratch);", "5"),
            (
                "DROP TABLE scratch;
                 UPDATE public.scratch SET note = 4;
                 SELECT note FROM public.scratch;",
                "4",
            ),
            (
                "DROP TABLE public.scratch;
                 UPDATE pg_temp.scratch SET note = 6;
                 SELECT note FROM pg_temp.scratch;",
                "6",
            ),
            (
                "ALTER TABLE scratch RENAME TO moved;
                 UPDATE pg_temp.moved SET note = 7;
                 UPDATE public.scratch SET note = 8;
                 SELECT (SELECT note FROM public.scratch) + (SELECT note FROM pg_temp.moved);",
                "15",
            ),
            (
                "ALTER TABLE public.scratch RENAME TO moved;
                 UPDATE public.moved SET note = 9;
                 UPDATE pg_temp.scratch SET note = 10;
                 SELECT (SELECT note FROM public.moved) + (SELECT note FROM pg_temp.scratch);",
                "19",
            ),
        ] {
            let rows = run_translated_default(&format!("{source}{event}"));
            assert_eq!(rows, vec![Some(expected.to_string())], "{event}");
        }
    }
}
