//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `CreateIndex` type.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::{
    ast::{
        BinaryOperator, CreateIndex, DataType, Expr, Function, FunctionArg, FunctionArgExpr,
        FunctionArguments, Ident, InTarget, IndexColumn, IndexType, KeyOrIndexDisplay,
        NullsDistinctOption, ObjectName, ObjectNamePart, OrderByExpr, OrderByOptions, Query,
        SetExpr, SqliteTableOption, Statement, TableConstraint, TriggerEvent, TriggerPeriod,
        UniqueConstraint, Value, VirtualTableModuleArgument, VisitMut, VisitorMut,
    },
    tokenizer::Span,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        function_helpers::{plain_catalog_function_args, simple_function_expr, string_literal},
        object_name::{
            append_suffix, fts_keys_table_name, fts_table_name, last_ident_value_or_display,
            normalize_schema_qualified_object_name_for_sqlite, postgres_catalog_function_name,
            quoted_ident, resolve_translation_table, sqlite_unqualified_object_name,
            sync_trigger_name,
        },
        privileges::names_table_with_backing,
        query_builder::{
            from_relation, make_query, make_simple_select, plain_table_factor, single_expr_query,
        },
        shared_helpers::{ColumnReferences, extract_columns_from_expr},
        translator_impls::{
            data_type::{is_serial_type, is_string_type},
            nulls_not_distinct::companion_index,
            postgis,
            rls::{generated_key_column, generated_table_column, resolve_trigger_table_name},
        },
    },
};

/// Represents the result of translating a GIN/GiST index - either an FTS5
/// virtual table or an error if the index pattern isn't supported.
pub(crate) enum FtsTranslation {
    /// FTS5 virtual table for full-text search
    Fts5 { table_name: ObjectName, columns: Vec<String> },
    /// Index pattern not supported (e.g., JSONB, arrays, spatial data)
    Unsupported(String),
}

fn plain_argument_expr(arg: &FunctionArg) -> Option<&Expr> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
        _ => None,
    }
}

fn tsvector_document(function: &Function) -> Option<&Expr> {
    let FunctionArguments::List(arguments) = &function.args else { return None };
    if !arguments.clauses.is_empty() {
        return None;
    }
    match arguments.args.as_slice() {
        [document] => plain_argument_expr(document),
        [config, document]
            if matches!(
                plain_argument_expr(config),
                Some(Expr::Value(value))
                    if matches!(value.value, Value::SingleQuotedString(_))
            ) =>
        {
            plain_argument_expr(document)
        }
        _ => None,
    }
}

fn lower_argument(function: &Function) -> Option<&Expr> {
    let arguments = plain_catalog_function_args(function, "lower")?;
    let [argument] = arguments.args.as_slice() else { return None };
    plain_argument_expr(argument)
}

#[cfg_attr(feature = "std", recursive::recursive)]
fn is_reproducible_fts_document(expr: &Expr) -> bool {
    match expr {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        Expr::Nested(inner) => is_reproducible_fts_document(inner),
        Expr::Cast { expr, data_type, .. } => {
            is_string_type(data_type) && is_reproducible_fts_document(expr)
        }
        Expr::BinaryOp { left, op: BinaryOperator::StringConcat, right } => {
            is_reproducible_fts_document(left) && is_reproducible_fts_document(right)
        }
        Expr::Value(value) => {
            matches!(&value.value, Value::SingleQuotedString(text) if text.chars().all(char::is_whitespace))
        }
        Expr::Function(function) => {
            lower_argument(function).is_some_and(is_reproducible_fts_document)
        }
        _ => false,
    }
}

/// Returns the columns in a `to_tsvector` document that FTS5 can reproduce.
#[cfg_attr(feature = "std", recursive::recursive)]
fn analyze_fts_expression(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::Function(function) => {
            if postgres_catalog_function_name(&function.name)
                .is_none_or(|name| name != "to_tsvector")
            {
                return None;
            }
            let document = tsvector_document(function)?;
            if !is_reproducible_fts_document(document) {
                return None;
            }
            match extract_columns_from_expr(document) {
                ColumnReferences::Complete(columns) if !columns.is_empty() => Some(columns),
                ColumnReferences::Complete(_) | ColumnReferences::Unknown => None,
            }
        }
        Expr::Nested(inner) => analyze_fts_expression(inner),
        _ => None,
    }
}

/// Analyze a GIN/GiST index and return how to translate it as FTS5.
pub(crate) fn analyze_fts_index(create_index: &CreateIndex) -> FtsTranslation {
    let table_name = create_index.table_name.clone();
    let index_type = match create_index.using {
        Some(IndexType::GIN) => "GIN",
        Some(IndexType::GiST) => "GiST",
        _ => "Index",
    };

    // Collect all columns from to_tsvector expressions
    let mut fts_columns: Vec<String> = Vec::new();

    for index_col in &create_index.columns {
        if let Some(cols) = analyze_fts_expression(&index_col.column.expr) {
            fts_columns.extend(cols);
        } else {
            // If any column is not a to_tsvector expression, we can't translate
            return FtsTranslation::Unsupported(format!(
                "{index_type} index on expression '{}' is not supported. Only to_tsvector() \
                 expressions can be translated to FTS5. For spatial data, consider SpatiaLite.",
                index_col.column.expr
            ));
        }
    }

    if fts_columns.is_empty() {
        return FtsTranslation::Unsupported(format!(
            "{index_type} index with to_tsvector() must reference at least one column"
        ));
    }

    // Deduplicate columns while preserving order
    let mut seen = alloc::collections::BTreeSet::new();
    fts_columns.retain(|col| seen.insert(col.clone()));

    FtsTranslation::Fts5 { table_name, columns: fts_columns }
}

/// How the rows of the FTS5 index over a table are tied to the table's rows.
pub(crate) enum FtsKey<'db> {
    /// A primary key of one integer column, which serves as the FTS5 rowid.
    Rowid(&'db str),
    /// Any other primary key, which the keys table pairs with the integer the
    /// FTS5 rows carry. The table's own rowid cannot serve, since `VACUUM` may
    /// renumber it when the table has no `INTEGER PRIMARY KEY`.
    Mapped {
        /// The keys table.
        keys_table: String,
        /// The primary key columns.
        columns: Vec<&'db str>,
    },
}

/// The keys table's integer key, the FTS5 rowid, as
/// [`generated_key_column`] names it.
const KEYS_ID: &str = "id";

/// The keys table's column holding the key column `column`, prefixed so no
/// key column can take [`KEYS_ID`].
fn keys_column(column: &str) -> String {
    format!("key_{column}")
}

impl<'db> FtsKey<'db> {
    /// The key of `table`, whose FTS5 index is named after `base_name`.
    ///
    /// # Errors
    ///
    /// Refuses a table without a primary key, which leaves nothing to tie the
    /// FTS5 rows to.
    pub(crate) fn of(
        table: &'db <ParserDB as DatabaseLike>::Table,
        schema: &'db ParserDB,
        base_name: &str,
    ) -> Result<Self, Error> {
        let key: Vec<_> = table.primary_key_columns(schema)?.collect();
        match *key.as_slice() {
            [] => {
                Err(Error::forward_refusal(format!(
                    "A full-text index on '{base_name}' cannot be translated, because FTS5 ties \
                     its rows to the table's through the primary key and '{base_name}' has none. \
                     Declare a primary key."
                )))
            }
            [column] if is_rowid_type(&column.attribute().data_type) => {
                Ok(Self::Rowid(column.column_name()))
            }
            _ => {
                Ok(Self::Mapped {
                    keys_table: fts_keys_table_name(base_name),
                    columns: key.iter().map(|&column| column.column_name()).collect(),
                })
            }
        }
    }

    /// The condition that the row `key_reference` names a key column of is
    /// among the rows `matching` answers the FTS5 rowids of.
    pub(crate) fn matched_by(&self, key_reference: impl Fn(&str) -> Expr, matching: Query) -> Expr {
        let in_query = |expr: Expr, query: Query| {
            Expr::In {
                expr: Box::new(expr),
                target: InTarget::Subquery(Box::new(query)),
                negated: false,
                global: false,
            }
        };
        match self {
            Self::Rowid(column) => in_query(key_reference(column), matching),
            Self::Mapped { keys_table, columns } => {
                let keys = make_query(
                    None,
                    SetExpr::Select(Box::new(make_simple_select(
                        ast_builder::select_items(
                            columns
                                .iter()
                                .map(|column| Expr::Identifier(quoted_ident(&keys_column(column))))
                                .collect(),
                        ),
                        from_relation(plain_table_factor(plain_name(keys_table))),
                        Some(in_query(qualified(&quoted_ident(keys_table), KEYS_ID), matching)),
                    ))),
                );
                let key = match columns.as_slice() {
                    [column] => key_reference(column),
                    _ => Expr::Tuple(columns.iter().map(|column| key_reference(column)).collect()),
                };
                in_query(key, keys)
            }
        }
    }

    /// The FTS5 rowid of the row `row` names, `NEW`, `OLD` or the indexed
    /// table itself.
    fn rowid_of(&self, row: &Ident) -> Expr {
        match self {
            Self::Rowid(column) => qualified(row, column),
            Self::Mapped { keys_table, .. } => {
                Expr::Subquery(Box::new(single_expr_query(
                    qualified(&quoted_ident(keys_table), KEYS_ID),
                    from_relation(plain_table_factor(plain_name(keys_table))),
                    self.key_of(row),
                )))
            }
        }
    }

    /// `<keys>.key_<column> = <row>.<column>` for every key column, the keys
    /// table row of the row `row` names. `None` when the key is the rowid.
    fn key_of(&self, row: &Ident) -> Option<Expr> {
        let Self::Mapped { keys_table, columns } = self else { return None };
        let keys_table = quoted_ident(keys_table);
        columns
            .iter()
            .map(|column| {
                Expr::BinaryOp {
                    left: Box::new(qualified(&keys_table, &keys_column(column))),
                    op: BinaryOperator::Eq,
                    right: Box::new(qualified(row, column)),
                }
            })
            .reduce(|left, right| {
                Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOperator::And,
                    right: Box::new(right),
                }
            })
    }

    /// `INSERT INTO <keys> (key_<column>, ...) <source>`, recording the keys
    /// of the rows `source` selects the key columns of. `None` when the key is
    /// the rowid.
    fn record(&self, source: impl FnOnce(&[&str]) -> Query) -> Option<Statement> {
        let Self::Mapped { keys_table, columns } = self else { return None };
        Some(ast_builder::insert(
            plain_name(keys_table),
            columns.iter().map(|column| plain_name(&keys_column(column))).collect(),
            source(columns),
        ))
    }

    /// `DELETE FROM <keys>` the keys table row of the row `row` names, when
    /// `condition` holds. `None` when the key is the rowid.
    fn forget(&self, row: &Ident, condition: Option<Expr>) -> Option<Statement> {
        let Self::Mapped { keys_table, .. } = self else { return None };
        let selection = self.key_of(row).into_iter().chain(condition).reduce(|left, right| {
            Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::And, right: Box::new(right) }
        });
        Some(ast_builder::delete(plain_name(keys_table), selection))
    }

    /// `CREATE TABLE <keys> (id INTEGER PRIMARY KEY, key_<column> ANY NOT NULL,
    /// ..., UNIQUE (key_<column>, ...)) STRICT`. `None` when the key is the
    /// rowid.
    fn create_keys_table(&self) -> Option<Statement> {
        let Self::Mapped { keys_table, columns } = self else { return None };
        let keys_columns: Vec<String> = columns.iter().map(|column| keys_column(column)).collect();
        let any = DataType::Custom(
            ObjectName(vec![ObjectNamePart::Identifier(Ident::new("ANY"))]),
            Vec::new(),
        );
        let mut table = ast_builder::blank_create_table(plain_name(keys_table));
        table.columns = core::iter::once(generated_key_column())
            .chain(
                keys_columns.iter().map(|column| generated_table_column(column, any.clone(), true)),
            )
            .collect();
        table.constraints = vec![TableConstraint::Unique(UniqueConstraint {
            name: None,
            index_name: None,
            index_type_display: KeyOrIndexDisplay::None,
            index_type: None,
            columns: keys_columns
                .iter()
                .map(|column| {
                    IndexColumn {
                        column: OrderByExpr {
                            expr: Expr::Identifier(quoted_ident(column)),
                            options: OrderByOptions::default(),
                            with_fill: None,
                        },
                        operator_class: None,
                    }
                })
                .collect(),
            include: Vec::new(),
            index_options: Vec::new(),
            characteristics: None,
            nulls_distinct: NullsDistinctOption::None,
            on_conflict: None,
        })];
        table.sqlite_table_options.options = vec![SqliteTableOption::Strict];
        Some(Statement::CreateTable(table))
    }
}

/// True when every value of `data_type` is an integer, which a single key
/// column needs to be the FTS5 rowid.
fn is_rowid_type(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::SmallInt(_)
            | DataType::Int(_)
            | DataType::Integer(_)
            | DataType::BigInt(_)
            | DataType::Int2(_)
            | DataType::Int4(_)
            | DataType::Int8(_)
    ) || is_serial_type(data_type)
}

fn plain_name(value: &str) -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(value))])
}

fn qualified(row: &Ident, column: &str) -> Expr {
    Expr::CompoundIdentifier(vec![row.clone(), quoted_ident(column)])
}

/// Generate the FTS5 virtual table statement.
///
/// A rowid key reads the indexed text from the table in external content mode
/// (`content=<table>`), and a mapped key keeps no text at all (`content=''`).
/// Neither stores a second copy of the text, and so the sync triggers remove
/// a row with the FTS5 `'delete'` command, which takes the indexed values.
fn create_fts5_virtual_table(
    base_name: &str,
    content_table: &str,
    key: &FtsKey<'_>,
    columns: &[String],
) -> Statement {
    let content = match key {
        FtsKey::Rowid(column) => {
            vec![format!("content={content_table}"), format!("content_rowid={column}")]
        }
        FtsKey::Mapped { .. } => vec!["content=''".to_string()],
    };
    let module_args = columns
        .iter()
        .map(|column| quoted_ident(column).to_string())
        .chain(content)
        .map(|value| VirtualTableModuleArgument { value, span: Span::empty() })
        .collect();

    Statement::CreateVirtualTable {
        name: plain_name(&fts_table_name(base_name)),
        if_not_exists: false,
        module_name: Ident::new("fts5"),
        module_args,
    }
}

fn qualify_predicate(predicate: &Expr, qualifier: &str) -> Expr {
    use core::ops::ControlFlow;

    struct QualifyIdents(Ident);

    impl VisitorMut for QualifyIdents {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Identifier(column) = expr {
                let column = column.clone();
                *expr = Expr::CompoundIdentifier(vec![self.0.clone(), column]);
            }
            ControlFlow::Continue(())
        }
    }

    let mut qualified_predicate = predicate.clone();
    let _: ControlFlow<()> =
        VisitMut::visit(&mut qualified_predicate, &mut QualifyIdents(Ident::new(qualifier)));
    qualified_predicate
}

/// `VALUES (<values>)`, or `SELECT <values> WHERE <condition>` when the row is
/// written only under `condition`.
fn row_source(values: Vec<Expr>, condition: Option<Expr>) -> Query {
    match condition {
        None => ast_builder::values(vec![values]),
        Some(condition) => {
            make_query(
                None,
                SetExpr::Select(Box::new(make_simple_select(
                    ast_builder::select_items(values),
                    Vec::new(),
                    Some(condition),
                ))),
            )
        }
    }
}

/// Generate the FTS5 sync triggers.
///
/// A row leaves the index through the FTS5 `'delete'` command, which the
/// index needs since it keeps no copy of the text it would otherwise read the
/// old values from. A mapped key also writes and deletes the row's keys table
/// row, before the FTS5 row is written and after it is deleted.
///
/// With a `predicate`, the INSERT trigger guards with the NEW-qualified
/// predicate and the DELETE trigger with the OLD-qualified one. The UPDATE
/// trigger needs both, so it carries them on its statements rather than in a
/// `WHEN` clause, and removes the old row before it adds the new one. A row
/// crossing the predicate boundary therefore enters or leaves the index, and
/// a row staying inside keeps the words its old and new text share.
fn create_fts5_triggers(
    trigger_table: &str,
    fts_table_base: &str,
    key: &FtsKey<'_>,
    columns: &[String],
    predicate: Option<&Expr>,
) -> Vec<Statement> {
    let fts_name = fts_table_name(fts_table_base);
    let new = Ident::new("new");
    let old = Ident::new("old");
    let target_columns = || {
        core::iter::once(plain_name("rowid")).chain(columns.iter().map(|column| plain_name(column)))
    };
    let insert_new = |condition: Option<Expr>| {
        let values = core::iter::once(key.rowid_of(&new))
            .chain(columns.iter().map(|column| qualified(&new, column)))
            .collect();
        let insert = ast_builder::insert(
            plain_name(&fts_name),
            target_columns().collect(),
            row_source(values, condition.clone()),
        );
        key.record(|key_columns| {
            row_source(
                key_columns.iter().map(|column| qualified(&new, column)).collect(),
                condition,
            )
        })
        .into_iter()
        .chain([insert])
        .collect::<Vec<_>>()
    };
    let delete_old = |condition: Option<Expr>| {
        let values = [string_literal("delete"), key.rowid_of(&old)]
            .into_iter()
            .chain(columns.iter().map(|column| qualified(&old, column)))
            .collect();
        let delete = ast_builder::insert(
            plain_name(&fts_name),
            core::iter::once(plain_name(&fts_name)).chain(target_columns()).collect(),
            row_source(values, condition.clone()),
        );
        core::iter::once(delete).chain(key.forget(&old, condition)).collect::<Vec<_>>()
    };
    let trigger = |event: TriggerEvent, condition: Option<Expr>, statements: Vec<Statement>| {
        ast_builder::trigger(
            plain_name(&sync_trigger_name(&fts_name, &event)),
            plain_name(trigger_table),
            TriggerPeriod::After,
            event,
            false,
            condition,
            statements,
        )
    };

    let new_predicate = predicate.map(|predicate| qualify_predicate(predicate, "NEW"));
    let old_predicate = predicate.map(|predicate| qualify_predicate(predicate, "OLD"));
    vec![
        trigger(TriggerEvent::Insert, new_predicate.clone(), insert_new(None)),
        trigger(TriggerEvent::Delete, old_predicate.clone(), delete_old(None)),
        trigger(
            TriggerEvent::Update(Vec::new()),
            None,
            [delete_old(old_predicate), insert_new(new_predicate)].concat(),
        ),
    ]
}

/// Generate all FTS5 statements: the keys table of a mapped key, the virtual
/// table, the sync triggers and the backfill of the rows already present.
fn create_fts5_statements(
    table_name: &ObjectName,
    columns: &[String],
    predicate: Option<&Expr>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let base_name = table_name
        .0
        .last()
        .and_then(|p| p.as_ident())
        .map_or_else(|| "unknown".to_string(), |i| i.value.clone());

    let table = resolve_translation_table(schema, table_name)?.ok_or_else(|| {
        Error::forward_refusal(format!(
            "Could not find table '{base_name}' in schema for FTS5 index creation"
        ))
    })?;
    let key = FtsKey::of(table, schema, &base_name)?;
    let trigger_table_name = resolve_trigger_table_name(&base_name, table, schema, options)?;

    let mut statements: Vec<Statement> = key.create_keys_table().into_iter().collect();
    statements.push(create_fts5_virtual_table(&base_name, &trigger_table_name, &key, columns));
    statements.extend(create_fts5_triggers(
        &trigger_table_name,
        &base_name,
        &key,
        columns,
        predicate,
    ));

    let from_table = |projection: Vec<Expr>| {
        make_query(
            None,
            SetExpr::Select(Box::new(make_simple_select(
                ast_builder::select_items(projection),
                from_relation(plain_table_factor(plain_name(&trigger_table_name))),
                predicate.cloned(),
            ))),
        )
    };
    statements.extend(key.record(|key_columns| {
        from_table(
            key_columns.iter().map(|column| Expr::Identifier(quoted_ident(column))).collect(),
        )
    }));
    let rowid = match &key {
        FtsKey::Rowid(column) => Expr::Identifier(quoted_ident(column)),
        FtsKey::Mapped { .. } => key.rowid_of(&quoted_ident(&trigger_table_name)),
    };
    let projection = core::iter::once(rowid)
        .chain(columns.iter().map(|column| Expr::Identifier(quoted_ident(column))))
        .collect();
    let target_columns = core::iter::once(plain_name("rowid"))
        .chain(columns.iter().map(|column| plain_name(column)))
        .collect();
    statements.push(ast_builder::insert(
        plain_name(&fts_table_name(&base_name)),
        target_columns,
        from_table(projection),
    ));

    Ok(statements)
}

/// Inspects a GiST `CreateIndex` and, if every indexed column resolves to a
/// `geometry` or `geography` data type in `schema`, returns
/// `SELECT CreateSpatialIndex('tbl','col')` statements (one per column) for
/// SQLiteGIS to execute at runtime. Returns `Ok(None)` when no indexed column
/// is spatial so the caller can fall through to the FTS5 / error path.
///
/// Errors when:
/// - the GiST has a `WHERE` predicate (SQLiteGIS's `CreateSpatialIndex` doesn't
///   honor partial indexes).
/// - the GiST mixes spatial and non-spatial columns (silently dropping the
///   non-spatial side would change query semantics).
fn try_spatial_index_routing(
    create_index: &CreateIndex,
    sqlite_table_name: &ObjectName,
    schema: &ParserDB,
) -> Result<Option<Vec<Statement>>, Error> {
    let Some(spatial_columns) = postgis::classify_gist_spatial_columns(create_index, schema)?
    else {
        return Ok(None);
    };

    let table_name = last_ident_value_or_display(sqlite_table_name);
    let statements = spatial_columns
        .into_iter()
        .map(|column| {
            Statement::Query(Box::new(single_expr_query(
                simple_function_expr(
                    "CreateSpatialIndex",
                    vec![string_literal(&table_name), string_literal(&column)],
                    None,
                ),
                Vec::new(),
                None,
            )))
        })
        .collect();
    Ok(Some(statements))
}

/// One CREATE INDEX column, settled on the comparison key when it is an
/// interval, since the key's byte order is PostgreSQL's interval order.
fn translate_index_column(
    column: &IndexColumn,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<IndexColumn, crate::errors::Error> {
    use crate::traits::translator::TranslatorWithContext;
    let mut translated = IndexColumn {
        column: column.column.translate_with_warnings(schema, options, emit)?,
        operator_class: None,
    };
    if crate::impls::expression_type::is_interval_typed(&column.column.expr, schema, options) {
        translated.column.expr =
            crate::impls::interval::keys::key_expr(&translated.column.expr, options)?;
    }
    Ok(translated)
}

/// Reports the PostgreSQL-only clauses the regular index path drops.
///
/// Each one is result-neutral, so the index still enforces what it enforced,
/// but D2 puts a result-neutral drop in the warn bucket rather than the silent
/// one. Only the clauses a `PostgreSqlDialect` parse can deliver are here:
/// `index_options` and `alter_options` belong to other dialects and never
/// arrive populated.
fn report_dropped_index_clauses(
    index: &CreateIndex,
    name: &ObjectName,
    emit: crate::warnings::WarningSink<'_>,
) {
    let location = || {
        crate::warnings::DowngradeLocation::Object(crate::warnings::DeclaredObject::Index {
            table: last_ident_value_or_display(&index.table_name),
            name: last_ident_value_or_display(name),
        })
    };
    if !index.include.is_empty() {
        let included = index.include.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
        let key = index.columns.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
        emit(crate::warnings::TranslationWarning::LossyDowngrade {
            construct: "index INCLUDE".to_string(),
            from: format!("({key}) INCLUDE ({included})"),
            to: format!("({key})"),
            location: location(),
            reason: "SQLite has no covering index, so a query PostgreSQL answers from the index \
                     alone, an index-only scan, visits the table here instead. An INCLUDE \
                     column is payload rather than key, so nothing about which rows the index \
                     accepts changes."
                .to_string(),
        });
    }

    if let Some(method) = &index.using {
        emit(crate::warnings::TranslationWarning::LossyDowngrade {
            construct: "index method".to_string(),
            from: format!("USING {method}"),
            to: "a SQLite b-tree index".to_string(),
            location: location(),
            reason: "SQLite builds one kind of index, so the emitted one serves whatever a \
                     b-tree serves. For hash and BRIN that loses nothing, since a b-tree \
                     answers equality and ranges alike, but a method chosen for a query shape \
                     a b-tree cannot serve no longer serves it."
                .to_string(),
        });
    }

    if index.concurrently {
        emit(crate::warnings::TranslationWarning::LossyDrop {
            construct: "CREATE INDEX CONCURRENTLY".to_string(),
            reason: "SQLite builds an index while holding a write lock and has no concurrent \
                     form. The resulting index is identical, so only the lock the build takes \
                     differs."
                .to_string(),
        });
    }

    if !index.with.is_empty() {
        let parameters = index.with.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
        emit(crate::warnings::TranslationWarning::LossyDrop {
            construct: "index storage parameters".to_string(),
            reason: format!(
                "SQLite has no storage parameters, so WITH ({parameters}) is dropped. These \
                 tune how PostgreSQL lays the index out on disk and say nothing about what it \
                 answers."
            ),
        });
    }

    // A class changes a UNIQUE index's rows only under a collation
    // `Expr::Collate` refuses.
    for column in &index.columns {
        if let Some(operator_class) = &column.operator_class {
            emit(crate::warnings::TranslationWarning::LossyDowngrade {
                construct: "index operator class".to_string(),
                from: format!("{} {operator_class}", column.column),
                to: column.column.to_string(),
                location: location(),
                reason: "SQLite has no operator classes, so the index serves fewer queries than \
                         the PostgreSQL one, notably the pattern matches a text pattern class \
                         exists for."
                    .to_string(),
            });
        }
    }
}

impl crate::traits::translator::TranslatorWithContext for CreateIndex {
    type SQLiteEntry = Vec<Statement>;

    fn translate_with_warnings(
        &self,
        schema: &sql_traits::structs::ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // An index expression and a partial index predicate name the indexed
        // table's own columns as they stand when the index is written.
        let written = options.written_schema().unwrap_or(schema);
        let indexed_source = resolve_translation_table(written, &self.table_name)?;
        let index_scope = crate::options::table_column_scope(indexed_source, written);
        let scoped = index_scope.as_ref().map(|scope| options.with_scope(scope));
        let options = scoped.as_ref().unwrap_or(options);
        let sqlite_table_name =
            normalize_schema_qualified_object_name_for_sqlite(schema, &self.table_name)?;

        // PostGIS GiST on geometry/geography -> SQLiteGIS's CreateSpatialIndex.
        // Routed before the FTS5 path so spatial columns don't fall into the
        // tsvector-only analyzer below.
        if options.is_sqlitegis_enabled()
            && matches!(self.using, Some(IndexType::GiST))
            && let Some(spatial_stmts) =
                try_spatial_index_routing(self, &sqlite_table_name, schema)?
        {
            return Ok(spatial_stmts);
        }

        if matches!(self.using, Some(IndexType::GIN | IndexType::GiST)) {
            let predicate = self
                .predicate
                .as_ref()
                .map(|predicate| predicate.translate_with_warnings(schema, options, emit))
                .transpose()?;

            let mut normalized_index = self.clone();
            normalized_index.table_name = self.table_name.clone();

            return match analyze_fts_index(&normalized_index) {
                FtsTranslation::Fts5 { table_name, columns } => {
                    create_fts5_statements(
                        &table_name,
                        &columns,
                        predicate.as_ref(),
                        schema,
                        options,
                    )
                }
                FtsTranslation::Unsupported(reason) => Err(Error::forward_refusal(reason)),
            };
        }

        // `Pg2Sqlite` names every index whose PostgreSQL name it can predict.
        let Some(name) = &self.name else {
            return Err(Error::forward_refusal(format!(
                "{self} cannot be translated without a name, because the name PostgreSQL gives \
                 this index cannot be predicted. Name the index."
            )));
        };
        report_dropped_index_clauses(self, name, emit);
        let backing_table = if names_table_with_backing(schema, &self.table_name, options)? {
            append_suffix(&sqlite_table_name, options.get_rls_table_suffix())
        } else {
            sqlite_table_name
        };
        let definition_options = options.without_subqueries();
        let options = &definition_options;
        let columns = self
            .columns
            .iter()
            .map(|column| translate_index_column(column, schema, options, emit))
            .collect::<Result<Vec<_>, _>>()?;
        let predicate = self
            .predicate
            .as_ref()
            .map(|predicate| predicate.translate_with_warnings(schema, options, emit))
            .transpose()?;

        let companion = if self.unique && self.nulls_distinct == Some(false) {
            companion_index(
                &last_ident_value_or_display(name),
                &backing_table,
                &columns,
                indexed_source,
                schema,
                predicate.clone(),
                self.if_not_exists,
            )?
        } else {
            None
        };

        // Regular index - translate normally, explicitly dropping PG-only
        // fields (only, using, concurrently, include, nulls_distinct, with,
        // index_options, alter_options) that are not valid in SQLite.
        let mut statements = vec![Statement::CreateIndex(CreateIndex {
            name: Some(sqlite_unqualified_object_name(name)),
            only: false,
            table_name: backing_table,
            using: None,
            columns,
            unique: self.unique,
            concurrently: false,
            r#async: false,
            if_not_exists: self.if_not_exists,
            include: vec![],
            nulls_distinct: None,
            with: vec![],
            predicate,
            index_options: vec![],
            alter_options: vec![],
        })];
        statements.extend(companion);
        for statement in &mut statements {
            if let Statement::CreateIndex(index) = statement
                && index.table_name.0.len() > 1
                && let Some(name) = &mut index.name
            {
                name.0.insert(0, index.table_name.0[0].clone());
            }
        }
        Ok(statements)
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            Expr, FunctionArg, FunctionArgExpr, FunctionArgOperator, FunctionArgumentList,
            FunctionArguments, Ident, Statement,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        ColumnReferences, analyze_fts_expression, analyze_fts_index, extract_columns_from_expr,
    };
    use crate::prelude::{Pg2SqliteOptions, Translator};

    fn parse_create_index(sql: &str) -> sqlparser::ast::CreateIndex {
        let stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse").remove(0);
        let Statement::CreateIndex(create_index) = stmt else {
            panic!("expected create index");
        };
        create_index
    }

    #[test]
    fn create_index_helpers_cover_named_wildcard_and_non_tsvector_paths() {
        let mut idx =
            parse_create_index("CREATE INDEX idx_docs ON docs USING GIN (to_tsvector(title))");
        let Expr::Function(func) = &mut idx.columns[0].column.expr else {
            panic!("expected function expression");
        };
        func.args = FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Named {
                    name: Ident::new("doc"),
                    arg: FunctionArgExpr::Expr(Expr::CompoundIdentifier(vec![
                        Ident::new("docs"),
                        Ident::new("title"),
                    ])),
                    operator: FunctionArgOperator::RightArrow,
                },
                FunctionArg::Unnamed(FunctionArgExpr::Wildcard),
            ],
            clauses: vec![],
        });

        let columns = extract_columns_from_expr(&idx.columns[0].column.expr);
        assert_eq!(columns, ColumnReferences::Complete(vec!["title".to_string()]));

        let nested = Expr::Nested(Box::new(idx.columns[0].column.expr.clone()));
        assert!(analyze_fts_expression(&nested).is_none());

        if let Expr::Function(func) = &mut idx.columns[0].column.expr {
            func.args = FunctionArguments::None;
        }
        assert_eq!(
            extract_columns_from_expr(&idx.columns[0].column.expr),
            ColumnReferences::Complete(Vec::new())
        );

        let mut idx_non_tsvector =
            parse_create_index("CREATE INDEX idx_docs2 ON docs USING GIN (to_tsvector(title))");
        if let Expr::Function(func) = &mut idx_non_tsvector.columns[0].column.expr {
            func.name =
                sqlparser::ast::ObjectName(vec![sqlparser::ast::ObjectNamePart::Identifier(
                    Ident::new("lower"),
                )]);
        }
        assert!(analyze_fts_expression(&idx_non_tsvector.columns[0].column.expr).is_none());

        let regular = parse_create_index("CREATE INDEX idx_plain ON docs (title)");
        let analysis = analyze_fts_index(&regular);
        assert!(matches!(analysis, super::FtsTranslation::Unsupported(_)));
    }

    #[test]
    fn fts_generation_handles_quoted_table_identifiers() {
        let schema_sql =
            r#"CREATE TABLE "Order Items" ("id" INTEGER PRIMARY KEY, "body text" TEXT);"#;
        let schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, schema_sql).expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");

        let index = parse_create_index(
            r#"CREATE INDEX order_body_fts ON "Order Items" USING GIN (to_tsvector("body text"))"#,
        );
        let translated =
            index.translate(&schema, &Pg2SqliteOptions::default()).expect("index should translate");

        assert!(
            translated.iter().any(|stmt| stmt.to_string().contains("CREATE TRIGGER")),
            "expected generated FTS synchronization triggers"
        );
    }

    #[test]
    fn analyze_fts_expression_refuses_named_arguments() {
        let mut idx =
            parse_create_index("CREATE INDEX idx_docs ON docs USING GIN (to_tsvector(title))");
        let Expr::Function(func) = &mut idx.columns[0].column.expr else {
            panic!("expected function expression");
        };
        func.args = FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::ExprNamed {
                    name: Expr::Identifier(Ident::new("doc")),
                    arg: FunctionArgExpr::Expr(Expr::CompoundIdentifier(vec![
                        Ident::new("docs"),
                        Ident::new("title"),
                    ])),
                    operator: FunctionArgOperator::Equals,
                },
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    sqlparser::ast::ValueWithSpan::from(sqlparser::ast::Value::SingleQuotedString(
                        "english".to_string(),
                    )),
                ))),
            ],
            clauses: vec![],
        });

        assert!(analyze_fts_expression(&idx.columns[0].column.expr).is_none());
    }
}
