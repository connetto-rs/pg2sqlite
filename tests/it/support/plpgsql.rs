//! Shared PL/pgSQL trigger scaffolding.

/// Wraps a trigger `body` in a `t` table, a `log` table the body may write
/// into, and a BEFORE INSERT trigger on `t`.
pub fn plpgsql_trigger_sql(body: &str) -> String {
    format!(
        "CREATE TABLE t (id INT PRIMARY KEY, val INT);
         CREATE TABLE log (note TEXT);
         CREATE FUNCTION f() RETURNS trigger AS $$
         {body}
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER go BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();"
    )
}
