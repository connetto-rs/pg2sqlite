//! `ONLY t` and `t *` in a `FROM` clause.
//!
//! Both concern table inheritance, which `CREATE TABLE ... INHERITS` and
//! `PARTITION OF` already refuse, so no descendant exists in the replica and
//! each spelling reads the one table, as PostgreSQL does when it has no
//! children.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::Pg2SqliteOptions;
use run_translated_helper::run_translated_with;

#[test]
fn an_inheritance_marker_reads_the_one_table() {
    for from in ["ONLY p", "ONLY (p)", "p *", "p"] {
        let rows = run_translated_with(
            &format!(
                "CREATE TABLE p (id int primary key);
                 INSERT INTO p (id) VALUES (1), (2);
                 SELECT count(*) FROM {from};"
            ),
            &Pg2SqliteOptions::default(),
        );
        assert_eq!(rows, vec![Some("2".to_string())], "FROM {from}");
    }
}
