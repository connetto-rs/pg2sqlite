//! The helpers a replica runs as table-valued functions.

use alloc::{string::String, vec::Vec};

/// A helper the replica runs as a table-valued function, as PostgreSQL calls
/// it instead of inlining its body.
///
/// A call site reads it as `name(arguments)` in a `FROM` list, binding each
/// argument to the parameter `?k` of `body` at the same position, and reads
/// its rows under `columns`. The body runs as a statement of its own, so its
/// relations open no cursor in the program of the statement calling it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperFunction<S> {
    /// The name a call site reads the function by.
    pub name: String,
    /// The parameters, which a call site passes as arguments in this order.
    pub parameters: Vec<String>,
    /// The columns of each row the function answers.
    pub columns: Vec<String>,
    /// The query answering a call, reading each argument as `?k`.
    pub body: S,
}

#[cfg(feature = "diesel")]
impl<S: core::fmt::Display> HelperFunction<S> {
    /// Registers the function on `connection` as a table-valued function
    /// running its body as a statement of its own on that connection, with
    /// each argument bound to its parameter as SQLite passed it.
    ///
    /// # Errors
    ///
    /// The error SQLite answers when it refuses the registration.
    pub fn register(
        &self,
        connection: &mut diesel::sqlite::SqliteConnection,
    ) -> diesel::QueryResult<()> {
        use alloc::string::ToString;

        use diesel::RunQueryDsl;
        use diesel_dynamic_schema::dynamic_value::{Any, DynamicRow, SqliteAnyValue};

        let function = diesel_dynamic_schema::table(self.name.clone());
        let columns = |names: &[String]| -> Vec<_> {
            names.iter().map(|name| function.column::<Any, _>(name.clone())).collect()
        };
        let body = self.body.to_string();
        connection.register_table_valued_function(
            columns(&self.columns),
            columns(&self.parameters),
            diesel::sqlite::SqliteFunctionBehavior::INNOCUOUS,
            // The body is translated SQL text, which no typed query can stand
            // for.
            move |connection, arguments: DynamicRow<SqliteAnyValue>| {
                arguments
                    .iter()
                    .fold(
                        diesel::sql_query(body.as_str()).into_boxed::<diesel::sqlite::Sqlite>(),
                        |query, argument| argument.bind_to(query),
                    )
                    .load::<DynamicRow<SqliteAnyValue>>(connection)
            },
        )
    }
}
