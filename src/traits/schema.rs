//! Submodule defining a schema for the translation between `PostgreSQL` and
//! `SQLite`.

use sql_traits::{
    structs::{IdentifierCase, TargetName},
    traits::{DatabaseLike, FunctionLike},
};
use sqlparser::ast::{CreateFunction, CreateTable, ObjectName, ObjectNamePart};
use sqlparser_plpgsql::embedded::Block;

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{errors::Error, impls::unlabelled_body::unlabelled_body};

/// Trait to define a schema for the translation between `PostgreSQL` and
/// `SQLite`.
pub trait Schema: DatabaseLike<Table = CreateTable, Function = CreateFunction> {
    /// Returns the PL/pgSQL body of the named function as a statement tree,
    /// if it exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LookupError`] if the name carries several
    /// declarations, which a lookup by name cannot choose between, and
    /// [`Error::TranslationRefusal`] if the body is not PL/pgSQL or an SQL
    /// statement in it does not parse.
    fn function_body(&self, name: &ObjectName) -> Result<Option<Block>, Error> {
        let Some(function) = written_function(self, name)? else { return Ok(None) };
        let Some(text) = function.body() else { return Ok(None) };
        unlabelled_body(function, text, |error| {
            Error::unsupported_source_syntax(format!(
                "Failed to parse trigger function '{name}' body: {error}"
            ))
        })
        .map(Some)
    }
}

/// Returns the function a written reference names, if it exists.
///
/// # Errors
///
/// Returns [`Error::LookupError`] if the name carries several declarations,
/// which a lookup by name cannot choose between.
pub(crate) fn written_function<'s, S: Schema>(
    schema: &'s S,
    name: &ObjectName,
) -> Result<Option<&'s CreateFunction>, Error> {
    let Some(target) = lookup_target(name) else {
        return Ok(None);
    };
    Ok(schema.resolve_target_function(target, IdentifierCase::AsWritten)?)
}

/// Reads a written function or relation reference as the name to resolve,
/// keeping each part spelled as the statement wrote it so that quoting
/// decides whether the part folds and an unqualified name goes through the
/// search path.
///
/// Returns [`None`] for a reference no catalog lookup can answer: a part built
/// while the statement runs, or a name reaching past a schema into another
/// catalog.
pub(crate) fn lookup_target(name: &ObjectName) -> Option<TargetName<'_>> {
    fn written(part: &ObjectNamePart) -> Option<(&str, bool)> {
        match part {
            ObjectNamePart::Identifier(ident) => {
                Some((ident.value.as_str(), ident.quote_style.is_some()))
            }
            ObjectNamePart::Function(_) => None,
        }
    }

    match name.0.as_slice() {
        [function] => {
            let (value, quoted) = written(function)?;
            Some(TargetName::new(value, quoted))
        }
        [schema, function] => {
            let (schema_value, schema_quoted) = written(schema)?;
            let (value, quoted) = written(function)?;
            Some(TargetName::new(value, quoted).with_schema(schema_value, schema_quoted))
        }
        _ => None,
    }
}

impl<S> Schema for S where S: DatabaseLike<Table = CreateTable, Function = CreateFunction> {}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{Ident, ObjectName, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use crate::traits::Schema;

    fn parse_statements(sql: &str) -> Vec<Statement> {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
    }

    fn function_name(value: &str) -> ObjectName {
        ObjectName::from(vec![Ident::new(value)])
    }

    fn schema_with_function(name: &str, body: &str) -> ParserDB {
        let sql =
            format!("CREATE FUNCTION {name}() RETURNS trigger AS $$\n{body}\n$$ LANGUAGE plpgsql;");
        ParserDB::from_statements(parse_statements(&sql), "test".to_string())
            .expect("schema should build")
    }

    #[test]
    fn function_body_reports_a_body_that_does_not_read() {
        for (name, body) in [
            ("trg_missing_begin", "SELECT 1;"),
            ("trg_missing_end", "BEGIN\n    SELECT 1;"),
            ("trg_parse_error", "BEGIN\n    SELECT FROM;\nEND;"),
            ("trg_tokenize_error", "BEGIN\n    SELECT \"unterminated;\nEND;"),
        ] {
            let err = schema_with_function(name, body)
                .function_body(&function_name(name))
                .expect_err("the body should not read");
            assert!(
                err.to_string()
                    .contains(&format!("Failed to parse trigger function '{name}' body")),
                "{name}: {err}"
            );
        }
    }

    /// The resolver, unlike the translator that refuses `SET search_path`,
    /// answers an unqualified reference from whichever schema the recorded
    /// path names.
    #[test]
    fn function_body_follows_the_search_path_for_an_unqualified_name() {
        let sql = r#"
            CREATE SCHEMA app;
            SET search_path TO app;
            CREATE FUNCTION app.log_doc() RETURNS trigger AS $$
            BEGIN
                INSERT INTO logs(id) VALUES (1);
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
        "#;
        let schema = ParserDB::from_statements(parse_statements(sql), "test".to_string())
            .expect("schema should build");

        let body = schema
            .function_body(&function_name("log_doc"))
            .expect("function body extraction should succeed")
            .expect("the recorded search path names `app`");
        assert_eq!(body.statements.len(), 2, "got {:?}", body.statements);
    }
}
