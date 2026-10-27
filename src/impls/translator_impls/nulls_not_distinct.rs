//! `NULLS NOT DISTINCT` as a companion unique index beside the plain key.
//!
//! The companion encodes each key that can be NULL as `(key IS NULL,
//! ifnull(key, 0))`, so NULLs collide while distinct values stay apart, and a
//! collision is an ordinary unique violation. The plain key stays for foreign
//! keys and `ON CONFLICT` targets.

#[cfg(not(feature = "std"))]
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, IndexLike, TableLike},
    utils::identifier_resolution::identifiers_match,
};
use sqlparser::ast::{
    ColumnOption, CreateIndex, CreateTable, Expr, IndexColumn, NullsDistinctOption, ObjectName,
    ObjectNamePart, ObjectType, OrderByExpr, OrderByOptions, Statement, TableConstraint,
};

use crate::{
    errors::Error,
    impls::{
        function_helpers::{integer_literal, simple_function_expr},
        object_name::{
            COLUMN_LOOKUP_CASE, last_ident, last_ident_value_or_display, quoted_ident,
            resolve_declared_table,
        },
        translator_impls::{expr::sqlite_collation, insert::postgres_constraint_name},
    },
    traits::translator::TranslatorWithContext,
};

/// Appended to the key's name, which a later `DROP INDEX` follows.
const COMPANION_SUFFIX: &str = "_nnd";

/// Companions of `create_table`'s `UNIQUE NULLS NOT DISTINCT` constraints.
pub(crate) fn table_companions(
    create_table: &CreateTable,
    emitted_table: &ObjectName,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let mut constraints = create_table
        .constraints
        .iter()
        .filter_map(|constraint| {
            match constraint {
                TableConstraint::Unique(unique) => Some(unique),
                _ => None,
            }
        })
        .filter(|unique| unique.nulls_distinct == NullsDistinctOption::NotDistinct)
        .peekable();
    if constraints.peek().is_none() {
        return Ok(Vec::new());
    }

    let table = resolve_declared_table(schema, create_table)?;
    let scope = table.map(|table| {
        sql_traits::structs::ColumnScope::for_table(
            table,
            schema,
            sql_traits::structs::IdentifierCase::AsWritten,
        )
    });
    let scoped = scope.as_ref().map(|scope| options.with_scope(scope));
    let options = scoped.as_ref().unwrap_or(options);

    let mut companions = Vec::new();
    for unique in constraints {
        let generated;
        let name = if let Some(name) = unique.name.as_ref().or(unique.index_name.as_ref()) {
            name.value.as_str()
        } else {
            let columns: Vec<String> = unique
                .columns
                .iter()
                .map(|column| {
                    match &column.column.expr {
                        Expr::Identifier(ident) => ident.value.clone(),
                        other => other.to_string(),
                    }
                })
                .collect();
            let table = last_ident_value_or_display(&create_table.name);
            generated = postgres_constraint_name(&table, &columns, false);
            generated.as_str()
        };
        let keys = unique
            .columns
            .iter()
            .map(|column| column.translate_with_warnings(schema, options, emit))
            .collect::<Result<Vec<_>, _>>()?;
        companions.extend(companion_index(
            name,
            emitted_table,
            &keys,
            table,
            schema,
            None,
            create_table.if_not_exists,
        )?);
    }
    Ok(companions)
}

/// The companion of translated `keys`, or `None` when no key can be NULL.
pub(crate) fn companion_index(
    name: &str,
    emitted_table: &ObjectName,
    keys: &[IndexColumn],
    table: Option<&CreateTable>,
    schema: &ParserDB,
    predicate: Option<Expr>,
    if_not_exists: bool,
) -> Result<Option<Statement>, Error> {
    let mut columns = Vec::with_capacity(keys.len() * 2);
    let mut nullable = false;
    for key in keys {
        let expr = &key.column.expr;
        let (operand, explicit_collation) = match expr {
            Expr::Collate { expr, collation } => (expr.as_ref(), Some(collation)),
            other => (other, None),
        };
        let column = match (operand, table) {
            (Expr::Identifier(ident), Some(table)) => {
                table.column(&ident.value, schema, COLUMN_LOOKUP_CASE)?
            }
            _ => None,
        };
        if let Some(column) = column
            && !column.is_nullable(schema)?
        {
            columns.push(index_column(expr.clone()));
            continue;
        }

        nullable = true;
        // A function result compares under BINARY, so collation is restated.
        let collation = match explicit_collation {
            Some(collation) => Some(collation.clone()),
            None => {
                column
                    .and_then(|column| {
                        column.attribute().options.iter().find_map(|option| {
                            match &option.option {
                                ColumnOption::Collation(collation) => Some(collation),
                                _ => None,
                            }
                        })
                    })
                    .map(sqlite_collation)
                    .transpose()?
            }
        };
        let value = simple_function_expr("ifnull", vec![operand.clone(), integer_literal(0)], None);
        columns.push(index_column(Expr::IsNull(Box::new(operand.clone()))));
        columns.push(index_column(match collation {
            Some(collation) => Expr::Collate { expr: Box::new(value), collation },
            None => value,
        }));
    }
    if !nullable {
        return Ok(None);
    }

    Ok(Some(Statement::CreateIndex(CreateIndex {
        name: Some(single_name(&companion_name(name))),
        table_name: emitted_table.clone(),
        using: None,
        columns,
        unique: true,
        concurrently: false,
        r#async: false,
        if_not_exists,
        include: Vec::new(),
        nulls_distinct: None,
        with: Vec::new(),
        predicate,
        index_options: Vec::new(),
        alter_options: Vec::new(),
    })))
}

/// Drops of the companions of the `NULLS NOT DISTINCT` indexes `names` reach.
pub(crate) fn companion_drops(names: &[ObjectName], schema: &ParserDB) -> Vec<Statement> {
    names
        .iter()
        .filter_map(last_ident)
        .filter_map(|written| {
            schema.indexes().find_map(|index| {
                let create = index.attribute();
                let stored = index.name()?;
                (create.unique
                    && create.nulls_distinct == Some(false)
                    && identifiers_match(
                        stored,
                        index.name_is_quoted(),
                        &written.value,
                        written.quote_style.is_some(),
                    ))
                .then(|| {
                    Statement::Drop {
                        object_type: ObjectType::Index,
                        // An index whose keys cannot be NULL has no companion.
                        if_exists: true,
                        names: vec![single_name(&companion_name(stored))],
                        cascade: false,
                        restrict: false,
                        purge: false,
                        temporary: false,
                        table: None,
                    }
                })
            })
        })
        .collect()
}

fn companion_name(name: &str) -> String {
    format!("{name}{COMPANION_SUFFIX}")
}

fn single_name(name: &str) -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(name))])
}

fn index_column(expr: Expr) -> IndexColumn {
    IndexColumn {
        column: OrderByExpr { expr, options: OrderByOptions::default(), with_fill: None },
        operator_class: None,
    }
}
