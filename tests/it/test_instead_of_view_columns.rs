use crate::helpers::run_translated::run_translated_default;

#[test]
fn instead_of_insert_reads_view_columns() {
    let sql = "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT DEFAULT 'anon', score INTEGER);
CREATE VIEW v AS SELECT id AS key, name, score * 2 AS doubled, score FROM base WHERE score > 0;
CREATE FUNCTION redirect() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
INSERT INTO base (id, name, score) VALUES (NEW.key + 100, 'trigger', NEW.score);
RETURN NEW;
END $$;
CREATE TRIGGER redirect INSTEAD OF INSERT ON v FOR EACH ROW EXECUTE FUNCTION redirect();
INSERT INTO v (key, score) VALUES (1, 5);
SELECT id || ' ' || name || ' ' || score FROM base;";
    assert_eq!(run_translated_default(sql), vec![Some("101 trigger 5".to_owned())]);
}

#[test]
fn instead_of_events_read_old_and_new_view_columns() {
    let sql = "CREATE TABLE base (id INTEGER PRIMARY KEY, score INTEGER);
CREATE TABLE seen (event TEXT, before_key INTEGER, after_key INTEGER, before_doubled INTEGER, after_doubled INTEGER);
CREATE VIEW v AS SELECT id AS key, score * 2 AS doubled, score FROM base;
CREATE FUNCTION record_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
INSERT INTO seen VALUES ('insert', OLD.key, NEW.key, OLD.doubled, NEW.doubled);
INSERT INTO base VALUES (NEW.key, NEW.score);
RETURN NEW;
END $$;
CREATE FUNCTION record_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
INSERT INTO seen VALUES ('update', OLD.key, NEW.key, OLD.doubled, NEW.doubled);
UPDATE base SET id = NEW.key, score = NEW.score WHERE id = OLD.key;
RETURN NEW;
END $$;
CREATE FUNCTION record_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
INSERT INTO seen VALUES ('delete', OLD.key, NEW.key, OLD.doubled, NEW.doubled);
DELETE FROM base WHERE id = OLD.key;
RETURN OLD;
END $$;
CREATE TRIGGER record_insert INSTEAD OF INSERT ON v FOR EACH ROW EXECUTE FUNCTION record_insert();
CREATE TRIGGER record_update INSTEAD OF UPDATE ON v FOR EACH ROW EXECUTE FUNCTION record_update();
CREATE TRIGGER record_delete INSTEAD OF DELETE ON v FOR EACH ROW EXECUTE FUNCTION record_delete();
INSERT INTO v (key, score, doubled) VALUES (1, 5, 90);
UPDATE v SET key = 2, score = 7, doubled = 91 WHERE key = 1;
DELETE FROM v WHERE key = 2;
SELECT event || ' ' || COALESCE(before_key::text, 'null') || ' ' || COALESCE(after_key::text, 'null') || ' ' || COALESCE(before_doubled::text, 'null') || ' ' || COALESCE(after_doubled::text, 'null') FROM seen ORDER BY event;";
    assert_eq!(
        run_translated_default(sql),
        vec![
            Some("delete 2 null 14 null".to_owned()),
            Some("insert null 1 null 90".to_owned()),
            Some("update 1 2 10 91".to_owned()),
        ]
    );
}
