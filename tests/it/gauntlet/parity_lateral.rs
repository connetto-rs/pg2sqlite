//! A `CROSS JOIN LATERAL` over a `UNION ALL` of plain selects reads the same
//! rows on both engines, in a plain query and as the step of a recursive walk,
//! cycles included.

use diesel::{connection::SimpleConnection, prelude::*, sql_types::Text};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::ast::Statement;

use crate::helpers::{establish_connection, postgres as postgres_harness};

const SCHEMA: &str = "
CREATE TABLE nodes (id TEXT PRIMARY KEY);
CREATE TABLE edges (id TEXT NOT NULL, parent_id TEXT NOT NULL);
CREATE TABLE owned (id TEXT NOT NULL, project_id TEXT NOT NULL);
";

/// A table whose column PostgreSQL compares under its own collation, which
/// only the translation reads, since SQLite knows no collation named `C`.
const LABELS: &str = "CREATE TABLE labels (id TEXT NOT NULL, name TEXT COLLATE \"C\" NOT NULL);";

mod schema {
    diesel::table! { nodes (id) { id -> Text, } }
    diesel::table! { edges (id, parent_id) { id -> Text, parent_id -> Text, } }
    diesel::table! { owned (id, project_id) { id -> Text, project_id -> Text, } }
}

const NODES: [&str; 5] = ["a", "b", "c", "d", "e"];
/// `a` and `b` are each other's parent, and `c` hangs under `b`.
const EDGES: [(&str, &str); 3] = [("a", "b"), ("b", "a"), ("c", "b")];
/// `c` is also owned by `b`, and `d` by `c`.
const OWNED: [(&str, &str); 2] = [("c", "b"), ("d", "c")];

macro_rules! seed {
    ($connection:expr) => {{
        let connection = $connection;
        connection.batch_execute(SCHEMA).unwrap();
        diesel::insert_into(schema::nodes::table)
            .values(NODES.map(|id| schema::nodes::id.eq(id)).to_vec())
            .execute(connection)
            .unwrap();
        diesel::insert_into(schema::edges::table)
            .values(
                EDGES
                    .map(|(id, parent)| {
                        (schema::edges::id.eq(id), schema::edges::parent_id.eq(parent))
                    })
                    .to_vec(),
            )
            .execute(connection)
            .unwrap();
        diesel::insert_into(schema::owned::table)
            .values(
                OWNED
                    .map(|(id, project)| {
                        (schema::owned::id.eq(id), schema::owned::project_id.eq(project))
                    })
                    .to_vec(),
            )
            .execute(connection)
            .unwrap();
    }};
}

/// Every node with each parent it has, once per way it has it.
const PARENTS: &str = "
SELECT n.id AS node, step.parent AS reached FROM nodes n CROSS JOIN LATERAL (
  SELECT e.parent_id AS parent FROM edges e WHERE e.id = n.id
  UNION ALL
  SELECT o.project_id FROM owned o WHERE o.id = n.id
) step";

/// Every node with every node its parents reach, itself included.
const WALK: &str = "
WITH RECURSIVE reached (node, id) AS (
  SELECT n.id, n.id FROM nodes n
  UNION
  SELECT reached.node, step.id FROM reached CROSS JOIN LATERAL (
    SELECT e.parent_id AS id FROM edges e WHERE e.id = reached.id
    UNION ALL
    SELECT o.project_id FROM owned o WHERE o.id = reached.id
  ) step
)
SELECT r.node, r.id AS reached FROM reached r";

#[derive(QueryableByName, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Pair {
    #[diesel(sql_type = Text)]
    node: String,
    #[diesel(sql_type = Text)]
    reached: String,
}

fn translated(query: &str) -> String {
    let schema = Pg2Sqlite::default().sql(SCHEMA).unwrap().build_schema().unwrap();
    Pg2Sqlite::default()
        .sql(query)
        .unwrap()
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .unwrap_or_else(|error| panic!("refused: {error}"))
        .into_vec()
        .into_iter()
        .find_map(|statement| {
            match statement {
                Statement::Query(query) => Some(query.to_string()),
                _ => None,
            }
        })
        .expect("translated query")
}

/// The pairs `query` reads, sorted, so that both engines' orders compare.
/// The DSL has no `LATERAL` and no recursive `WITH`, so the query is raw.
fn pairs<C: Connection>(query: &str, connection: &mut C) -> Vec<Pair>
where
    C::Backend: diesel::backend::Backend,
    diesel::query_builder::SqlQuery: diesel::query_dsl::LoadQuery<'static, C, Pair>,
{
    let mut pairs = diesel::sql_query(query).load::<Pair>(connection).unwrap();
    pairs.sort_unstable();
    pairs
}

fn pair(node: &str, reached: &str) -> Pair {
    Pair { node: node.to_string(), reached: reached.to_string() }
}

#[test]
fn a_lateral_union_reads_the_same_rows_on_both_engines() {
    let mut postgres = postgres_harness::fresh_database();
    let mut sqlite = establish_connection();
    seed!(&mut postgres);
    seed!(&mut sqlite);

    let expected = pairs(PARENTS, &mut postgres);
    assert_eq!(
        expected,
        [pair("a", "b"), pair("b", "a"), pair("c", "b"), pair("c", "b"), pair("d", "c")]
    );
    assert_eq!(pairs(&translated(PARENTS), &mut sqlite), expected);
}

/// A lateral select in parentheses on the right of a set operation keeps its
/// grouping once it is a union of its arms.
#[test]
fn a_parenthesized_lateral_union_operand_reads_the_same_rows_on_both_engines() {
    let mut postgres = postgres_harness::fresh_database();
    let mut sqlite = establish_connection();
    seed!(&mut postgres);
    seed!(&mut sqlite);

    let query =
        format!("SELECT o.id AS node, o.project_id AS reached FROM owned o INTERSECT ({PARENTS})");
    let expected = pairs(&query, &mut postgres);
    assert_eq!(expected, [pair("c", "b"), pair("d", "c")]);
    assert_eq!(pairs(&translated(&query), &mut sqlite), expected);
}

/// A recursive step in parentheses stays at the level SQLite requires of
/// the recursive reference once it is a union of its arms.
#[test]
fn a_parenthesized_recursive_step_reads_the_same_rows_on_both_engines() {
    let mut postgres = postgres_harness::fresh_database();
    let mut sqlite = establish_connection();
    seed!(&mut postgres);
    seed!(&mut sqlite);

    let walk = WALK
        .replace("UNION\n  SELECT reached.node", "UNION\n  (SELECT reached.node")
        .replace(") step\n)", ") step)\n)");
    assert_ne!(walk, WALK);
    let expected = pairs(WALK, &mut postgres);
    assert_eq!(pairs(&walk, &mut postgres), expected);
    assert_eq!(pairs(&translated(&walk), &mut sqlite), expected);
}

/// A column of the lateral read in parentheses keeps the name PostgreSQL
/// gives it, which a query over the select reads it by.
#[test]
fn a_parenthesized_lateral_column_keeps_its_name_on_both_engines() {
    let mut postgres = postgres_harness::fresh_database();
    let mut sqlite = establish_connection();
    seed!(&mut postgres);
    seed!(&mut sqlite);

    let query = "SELECT d.id AS node, d.v AS reached FROM (
        SELECT (step.v), o.id FROM owned o CROSS JOIN LATERAL (
          SELECT e.id AS v FROM edges e WHERE e.parent_id = o.project_id
        ) step
      ) d";
    let expected = pairs(query, &mut postgres);
    assert_eq!(expected, [pair("c", "a"), pair("c", "c")]);
    assert_eq!(pairs(&translated(query), &mut sqlite), expected);
}

#[test]
fn a_recursive_walk_stepping_through_a_lateral_union_reads_the_same_rows_on_both_engines() {
    let mut postgres = postgres_harness::fresh_database();
    let mut sqlite = establish_connection();
    seed!(&mut postgres);
    seed!(&mut sqlite);

    let expected = pairs(WALK, &mut postgres);
    let reached_from = |node: &str| {
        expected
            .iter()
            .filter(|pair| pair.node == node)
            .map(|pair| pair.reached.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(reached_from("a"), ["a", "b"]);
    assert_eq!(reached_from("d"), ["a", "b", "c", "d"]);
    assert_eq!(reached_from("e"), ["e"]);
    assert_eq!(pairs(&translated(WALK), &mut sqlite), expected);
}

/// A lateral select keeps its rows on the right of a set operation other
/// than a union, beside an operand that joins no lateral, under the
/// condition its join states and the select's own, and after a left join.
#[test]
fn a_lateral_union_in_set_operations_conditions_and_joins_reads_the_same_rows_on_both_engines() {
    let mut postgres = postgres_harness::fresh_database();
    let mut sqlite = establish_connection();
    seed!(&mut postgres);
    seed!(&mut sqlite);

    let arms = "SELECT e.parent_id AS parent FROM edges e WHERE e.id = n.id
        UNION ALL
        SELECT o.project_id FROM owned o WHERE o.id = n.id";
    for query in [
        format!(
            "SELECT o.id AS node, o.project_id AS reached FROM owned o INTERSECT
             SELECT n.id AS node, step.parent AS reached FROM nodes n CROSS JOIN LATERAL ({arms}) step"
        ),
        format!("{PARENTS} UNION ALL SELECT o.id, o.project_id FROM owned o"),
        format!(
            "SELECT n.id AS node, step.parent AS reached FROM nodes n
             JOIN LATERAL ({arms}) step ON step.parent <> 'a' WHERE n.id <> 'b'"
        ),
        format!(
            "SELECT n.id AS node, step.parent AS reached FROM nodes n
             LEFT JOIN owned w ON w.project_id = n.id CROSS JOIN LATERAL ({arms}) step"
        ),
    ] {
        let expected = pairs(&query, &mut postgres);
        assert!(!expected.is_empty(), "{query} reads no row, which measures nothing");
        assert_eq!(pairs(&translated(&query), &mut sqlite), expected, "{query}");
    }
}

/// A lateral whose arms, written in place, could answer differently from the
/// lateral is refused: a bare column of the select or of an arm, or a nested
/// query, whose names an arm's relations or values could claim, arms whose
/// values the union would bring to another type or collation, a `COLLATE`
/// in a value, a value a copy of which could answer another value, a call
/// in the select's items, which may aggregate every arm's rows, a derived
/// table or a join in an arm, which loses sight of the relations before the
/// lateral, a join before it whose condition could read an arm, a table read
/// under a `WITH` query's name or an alias renaming its columns, a column of
/// the lateral no arm has a value for, an arm reading every column through
/// `*`, a lateral that limits its rows or a `UNION` that drops duplicates
/// across arms, and before a lateral of several arms, each of which reads it
/// again, a relation whose rows could differ per read: a derived table, a
/// view, or a `WITH` query drawing a random value.
#[test]
fn a_lateral_union_whose_arms_could_read_differently_in_place_is_refused() {
    let schema = Pg2Sqlite::default()
        .sql(&format!(
            "{SCHEMA}{LABELS}CREATE VIEW drawn AS SELECT nodes.id, random() AS r FROM nodes;"
        ))
        .unwrap()
        .build_schema()
        .unwrap();
    for query in [
        "SELECT id FROM nodes n CROSS JOIN LATERAL (
           SELECT o.project_id AS reached FROM owned o WHERE o.id = n.id
           UNION ALL SELECT e.parent_id FROM edges e WHERE e.id = n.id
         ) step",
        "SELECT n.id FROM nodes n CROSS JOIN LATERAL (
           SELECT e.parent_id AS parent FROM edges e WHERE e.id = n.id
           UNION ALL SELECT o.project_id FROM owned o WHERE o.id = n.id
         ) step WHERE EXISTS (SELECT 1 FROM edges e WHERE e.id = step.parent)",
        "SELECT n.id, step.reached FROM nodes n CROSS JOIN LATERAL (
           SELECT o.project_id AS reached FROM owned o WHERE o.id = n.id
           UNION ALL SELECT 1 FROM edges e WHERE e.id = n.id
         ) step",
        "SELECT step.drawn, step.drawn FROM nodes n CROSS JOIN LATERAL (
           SELECT random() AS drawn FROM edges e WHERE e.id = n.id
         ) step",
        "SELECT upper(step.reached) FROM nodes n CROSS JOIN LATERAL (
           SELECT e.parent_id AS reached FROM edges e WHERE e.id = n.id
         ) step",
        "SELECT n.id, step.reached FROM nodes n CROSS JOIN LATERAL (
           SELECT d.parent_id AS reached FROM (SELECT e.parent_id FROM edges e WHERE e.id = n.id) d
         ) step",
        "SELECT a.id, step.reached FROM nodes a FULL JOIN nodes b ON b.id = a.id
         CROSS JOIN LATERAL (SELECT e.parent_id AS reached FROM edges e WHERE e.id = a.id) step",
        "SELECT n.id, step.reached FROM nodes n CROSS JOIN LATERAL (
           SELECT x.b AS reached FROM edges AS x (b, a) WHERE x.a = n.id
           UNION ALL SELECT o.project_id FROM owned o WHERE o.id = n.id
         ) step",
        "WITH edges AS (SELECT o.id, o.project_id AS parent_id FROM owned o)
         SELECT n.id, step.reached FROM nodes n CROSS JOIN LATERAL (
           SELECT e.parent_id AS reached FROM edges e WHERE e.id = n.id
           UNION ALL SELECT o.project_id FROM owned o WHERE o.id = n.id
         ) step",
        "SELECT n.id, step.parent_id FROM nodes n CROSS JOIN LATERAL (
           SELECT e.parent_id::text FROM edges e WHERE e.id = n.id
         ) step",
        "SELECT n.id, step.reached FROM nodes n CROSS JOIN LATERAL (
           SELECT e.parent_id AS reached FROM edges e
           WHERE EXISTS (SELECT 1 FROM owned o WHERE o.id = id)
         ) step",
        "SELECT n.id, step.reached FROM nodes n CROSS JOIN LATERAL (
           SELECT l.name AS reached FROM labels l WHERE l.id = n.id
           UNION ALL SELECT o.project_id FROM owned o WHERE o.id = n.id
         ) step",
        "SELECT n.id, step.reached FROM nodes n CROSS JOIN LATERAL (
           SELECT e.parent_id COLLATE \"C\" AS reached FROM edges e WHERE e.id = n.id
         ) step",
        "SELECT s.id, step.reached FROM (SELECT n.id FROM nodes n) s CROSS JOIN LATERAL (
           SELECT e.parent_id AS reached FROM edges e WHERE e.id = s.id
           UNION ALL SELECT o.project_id FROM owned o WHERE o.id = s.id
         ) step",
        "SELECT d.id, d.r, step.reached FROM drawn d CROSS JOIN LATERAL (
           SELECT e.parent_id AS reached FROM edges e WHERE e.id = d.id
           UNION ALL SELECT o.project_id FROM owned o WHERE o.id = d.id
         ) step",
        "WITH drawn AS (SELECT n.id, random() AS r FROM nodes n)
         SELECT d.id, d.r, step.reached FROM drawn d CROSS JOIN LATERAL (
           SELECT e.parent_id AS reached FROM edges e WHERE e.id = d.id
           UNION ALL SELECT o.project_id FROM owned o WHERE o.id = d.id
         ) step",
        "SELECT n.id, step.reached FROM nodes n CROSS JOIN LATERAL (
           SELECT e.parent_id AS reached FROM edges e WHERE e.id = n.id LIMIT 1
         ) step",
        "SELECT n.id, step.reached FROM nodes n CROSS JOIN LATERAL (
           SELECT e.parent_id AS reached FROM edges e JOIN owned o ON o.id = e.id
           WHERE e.id = n.id
         ) step",
        "SELECT n.id, step.reached FROM nodes n CROSS JOIN LATERAL (
           SELECT e.parent_id AS reached FROM edges e WHERE e.id = n.id
           UNION SELECT o.project_id FROM owned o WHERE o.id = n.id
         ) step",
        "SELECT n.id, step.parent_id FROM nodes n CROSS JOIN LATERAL (
           SELECT parent_id FROM edges e WHERE e.id = n.id
         ) step",
        "SELECT n.id, step.parent_id FROM nodes n CROSS JOIN LATERAL (
           SELECT * FROM edges e WHERE e.id = n.id
         ) step",
    ] {
        let translated = Pg2Sqlite::default()
            .sql(query)
            .unwrap()
            .translate_with_schema(&schema, &Pg2SqliteOptions::default());
        assert!(translated.is_err(), "translated {query}: {translated:?}");
    }
}
