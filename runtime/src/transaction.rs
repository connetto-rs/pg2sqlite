//! Named savepoints mapped to Diesel transaction depths.

use alloc::{string::String, vec::Vec};

use diesel::{
    connection::{AnsiTransactionManager, SimpleConnection, TransactionManager},
    result::Error as DatabaseError,
    sqlite::SqliteConnection,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum TransactionError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("savepoint \"{0}\" does not exist")]
    MissingSavepoint(String),
}

struct NamedSavepoint {
    name: String,
    depth: u32,
}

#[derive(Default)]
pub(crate) struct TransactionState {
    savepoints: Vec<NamedSavepoint>,
    /// The statements run before every commit ending a transaction.
    pub(crate) before_commit: Vec<String>,
}

impl TransactionState {
    /// The nesting depth maintained by Diesel.
    pub(crate) fn depth(connection: &mut SqliteConnection) -> Result<u32, TransactionError> {
        Ok(AnsiTransactionManager::transaction_manager_status_mut(connection)
            .transaction_depth()?
            .map_or(0, core::num::NonZeroU32::get))
    }

    /// Begins an explicit transaction with the selected SQLite mode.
    pub(crate) fn begin(
        &mut self,
        connection: &mut SqliteConnection,
        statement: &str,
    ) -> Result<(), TransactionError> {
        AnsiTransactionManager::begin_transaction_sql(connection, statement)?;
        self.savepoints.clear();
        Ok(())
    }

    /// Commits every nested level and the outer transaction, after the
    /// pre-commit program.
    pub(crate) fn commit(
        &mut self,
        connection: &mut SqliteConnection,
    ) -> Result<(), TransactionError> {
        self.drain(connection)?;
        loop {
            let result = AnsiTransactionManager::commit_transaction(connection);
            self.synchronize(connection)?;
            result?;
            if Self::depth(connection)? == 0 {
                return Ok(());
            }
        }
    }

    /// Rolls back every nested level and the outer transaction.
    pub(crate) fn rollback(
        &mut self,
        connection: &mut SqliteConnection,
    ) -> Result<(), TransactionError> {
        loop {
            let result = AnsiTransactionManager::rollback_transaction(connection);
            self.synchronize(connection)?;
            result?;
            if Self::depth(connection)? == 0 {
                return Ok(());
            }
        }
    }

    /// Records the source name of a newly nested Diesel savepoint. Outside a
    /// transaction the savepoint opens one, as SQLite's `SAVEPOINT` does.
    pub(crate) fn savepoint(
        &mut self,
        connection: &mut SqliteConnection,
        name: &str,
    ) -> Result<(), TransactionError> {
        AnsiTransactionManager::begin_transaction(connection)?;
        let depth = Self::depth(connection)?;
        self.savepoints.push(NamedSavepoint { name: String::from(name), depth });
        Ok(())
    }

    /// Opens one more Diesel level without a source name, as Diesel's own
    /// nested transactions do.
    pub(crate) fn nest(
        &mut self,
        connection: &mut SqliteConnection,
    ) -> Result<(), TransactionError> {
        AnsiTransactionManager::begin_transaction(connection)?;
        Ok(())
    }

    /// Releases the innermost level alone, below the outermost one.
    pub(crate) fn release_innermost(
        &mut self,
        connection: &mut SqliteConnection,
    ) -> Result<(), TransactionError> {
        let result = AnsiTransactionManager::commit_transaction(connection);
        self.synchronize(connection)?;
        Ok(result?)
    }

    /// Rolls back the innermost level alone, below the outermost one.
    pub(crate) fn rollback_innermost(
        &mut self,
        connection: &mut SqliteConnection,
    ) -> Result<(), TransactionError> {
        let result = AnsiTransactionManager::rollback_transaction(connection);
        self.synchronize(connection)?;
        Ok(result?)
    }

    /// Releases the savepoint and every level nested inside it.
    pub(crate) fn release(
        &mut self,
        connection: &mut SqliteConnection,
        name: &str,
    ) -> Result<(), TransactionError> {
        let position = self.position(name)?;
        let target = self.savepoints[position].depth;
        if target == 1 {
            self.drain(connection)?;
        }
        while Self::depth(connection)? >= target {
            let result = AnsiTransactionManager::commit_transaction(connection);
            self.synchronize(connection)?;
            result?;
        }
        Ok(())
    }

    /// Rolls back through the savepoint's depth and restores it open.
    pub(crate) fn rollback_to(
        &mut self,
        connection: &mut SqliteConnection,
        name: &str,
    ) -> Result<(), TransactionError> {
        let position = self.position(name)?;
        let target = self.savepoints[position].depth;
        while Self::depth(connection)? >= target {
            if let Err(error) = AnsiTransactionManager::rollback_transaction(connection) {
                self.synchronize(connection)?;
                return Err(error.into());
            }
        }
        if let Err(error) = AnsiTransactionManager::begin_transaction(connection) {
            self.synchronize(connection)?;
            return Err(error.into());
        }
        self.savepoints.truncate(position + 1);
        Ok(())
    }

    /// The latest savepoint named `name`, matched without regard to ASCII
    /// case, as SQLite compares them.
    fn position(&self, name: &str) -> Result<usize, TransactionError> {
        self.savepoints
            .iter()
            .rposition(|savepoint| savepoint.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| TransactionError::MissingSavepoint(String::from(name)))
    }

    /// Runs the pre-commit program inside a transaction, rolling the whole
    /// transaction back when a statement fails, as PostgreSQL ends a
    /// transaction whose commit fails.
    fn drain(&mut self, connection: &mut SqliteConnection) -> Result<(), TransactionError> {
        if Self::depth(connection)? == 0 {
            return Ok(());
        }
        let failure = self
            .before_commit
            .iter()
            .find_map(|statement| connection.batch_execute(statement).err());
        match failure {
            Some(error) => {
                self.rollback(connection)?;
                Err(error.into())
            }
            None => Ok(()),
        }
    }

    /// Removes names whose Diesel transaction levels have ended.
    fn synchronize(&mut self, connection: &mut SqliteConnection) -> Result<(), TransactionError> {
        let depth = Self::depth(connection)?;
        self.savepoints.retain(|savepoint| savepoint.depth <= depth);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use diesel::{
        Connection,
        connection::{AnsiTransactionManager, TransactionManager},
        sqlite::SqliteConnection,
    };

    use super::TransactionState;

    /// The depth reported by Diesel's transaction manager.
    fn depth(connection: &mut SqliteConnection) -> u32 {
        AnsiTransactionManager::transaction_manager_status_mut(connection)
            .transaction_depth()
            .expect("read transaction depth")
            .map_or(0, core::num::NonZeroU32::get)
    }

    /// Rolling back through three names keeps the outermost named savepoint
    /// open.
    #[test]
    fn three_savepoints_roll_back_to_outermost_depth() {
        let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
        let mut transactions = TransactionState::default();
        transactions.begin(&mut connection, "BEGIN").expect("begin transaction");
        assert_eq!(depth(&mut connection), 1);
        for (name, expected_depth) in [("outermost", 2), ("middle", 3), ("innermost", 4)] {
            transactions.savepoint(&mut connection, name).expect("create named savepoint");
            assert_eq!(depth(&mut connection), expected_depth);
        }
        transactions.rollback_to(&mut connection, "outermost").expect("rollback to outermost");
        assert_eq!(depth(&mut connection), 2);
        let error =
            transactions.release(&mut connection, "middle").expect_err("middle was removed");
        assert_eq!(error.to_string(), "savepoint \"middle\" does not exist");
        assert_eq!(depth(&mut connection), 2);
        transactions
            .rollback_to(&mut connection, "outermost")
            .expect("repeat rollback to outermost");
        assert_eq!(depth(&mut connection), 2);
        transactions.release(&mut connection, "outermost").expect("release outermost");
        assert_eq!(depth(&mut connection), 1);
        transactions.commit(&mut connection).expect("commit transaction");
        assert_eq!(depth(&mut connection), 0);
    }

    /// Release removes every nested name through the requested savepoint.
    #[test]
    fn release_commits_down_through_named_depth() {
        let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
        let mut transactions = TransactionState::default();
        transactions
            .begin(&mut connection, "BEGIN IMMEDIATE")
            .expect("begin immediate transaction");
        for name in ["outermost", "middle", "innermost"] {
            transactions.savepoint(&mut connection, name).expect("create named savepoint");
        }
        transactions.release(&mut connection, "middle").expect("release middle and innermost");
        assert_eq!(depth(&mut connection), 2);
        assert!(transactions.rollback_to(&mut connection, "innermost").is_err());
        transactions.rollback(&mut connection).expect("rollback whole transaction");
        assert_eq!(depth(&mut connection), 0);
    }

    /// A savepoint outside a transaction opens it, and names match without
    /// regard to ASCII case, as SQLite's do.
    #[test]
    fn outermost_savepoint_opens_the_transaction() {
        let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
        let mut transactions = TransactionState::default();
        transactions.savepoint(&mut connection, "Outer").expect("open by savepoint");
        assert_eq!(depth(&mut connection), 1);
        transactions.rollback_to(&mut connection, "OUTER").expect("roll back to it");
        assert_eq!(depth(&mut connection), 1);
        transactions.release(&mut connection, "outer").expect("release it");
        assert_eq!(depth(&mut connection), 0);
    }
}
