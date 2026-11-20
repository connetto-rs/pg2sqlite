//! Transactions committed through the statements a translated script runs
//! before `COMMIT`.

use diesel::{QueryResult, SqliteConnection, connection::SimpleConnection};

/// Runs `writes` in one transaction, committing through `before_commit` and
/// rolling back when the commit fails, as PostgreSQL does.
pub fn transaction(
    connection: &mut SqliteConnection,
    before_commit: &[String],
    writes: &str,
) -> QueryResult<()> {
    connection.batch_execute("BEGIN").expect("begin");
    let committed = connection.batch_execute(writes).and_then(|()| {
        before_commit.iter().try_for_each(|statement| connection.batch_execute(statement))?;
        connection.batch_execute("COMMIT")
    });
    if committed.is_err() {
        connection.batch_execute("ROLLBACK").expect("roll back");
    }
    committed
}
