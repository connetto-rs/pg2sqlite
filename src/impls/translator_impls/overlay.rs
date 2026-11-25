//! The overlay each connection keeps over a shared attached table that
//! triggers derive writes into, under both
//! [`Pg2SqliteOptions::with_attached_tables`] and
//! [`Pg2SqliteOptions::with_server_derived_write_function`].
//!
//! Several connections share the attached database, each with its own
//! `main`. A derived write is provisional until the server settles the upload
//! that caused it, so it must not land in rows another connection reads. The
//! table's rows therefore live in a storage table in the attached database,
//! and the connection's provisional rows in an overlay table in its `main`,
//! keyed by the primary key, with a tombstone flag standing for a deleted row.
//! A `TEMP` view under the name every statement reads and writes the table by
//! merges the overlay over the storage rows, and its `INSTEAD OF` triggers
//! route each write:
//!
//! - while a derived run is open, which the derived-write function called with
//!   `0` answers, or when the overlay already holds the written key, to the
//!   overlay, a delete as a tombstone,
//! - otherwise to the storage table.
//!
//! SQLite refuses a qualified table name in the writes of a trigger, so every
//! object carries a name of its own and each trigger reaches it unqualified.
//!
//! Each write on the overlay stands for one write on the table, or for none:
//!
//! | Overlay write | Write on the table |
//! |---|---|
//! | `INSERT`, live, key not in storage | `INSERT` |
//! | `INSERT`, live, key in storage | none, a copy before a change |
//! | `INSERT`, tombstone | none, a key moved away |
//! | `UPDATE`, live to live | `UPDATE` |
//! | `UPDATE`, tombstone to live | `INSERT` |
//! | `UPDATE`, live to tombstone | `DELETE` |
//! | `DELETE` | none, the settled row's removal |
//!
//! Every `BEFORE` and `AFTER` trigger on the storage table is copied onto the
//! overlay under those conditions, so the table's own triggers run on the
//! writes that land there as they run on the server.

use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, BinaryOperator, ColumnDef, ColumnOption, ColumnOptionDef,
    CreateTable, CreateTrigger, DataType, DropTrigger, Expr, ObjectName, ObjectNamePart,
    SelectItem, SetExpr, SetOperator, SetQuantifier, Statement, TableConstraint, TriggerEvent,
    TriggerPeriod, UnaryOperator, visit_relations_mut,
};

use super::reentry;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        derived_writes::derived_targets,
        function_helpers::{integer_literal, simple_function_expr},
        object_name::{
            append_suffix, last_ident, last_ident_value_or_display, quoted_ident,
            resolve_translation_table,
        },
        privileges::has_backing_table,
        query_builder::{
            from_relation, make_query, make_simple_select, plain_table_factor, single_expr_query,
        },
        translator_impls::rls::{TriggerColumn, prefixed_column_expr, trigger_columns},
    },
    options::{Pg2SqliteOptions, TranslationContext},
    traits::translator::TranslatorWithContext,
    warnings::WarningSink,
};

const STORAGE_SUFFIX: &str = "_pg2sqlite_shared";
const OVERLAY_SUFFIX: &str = "_pg2sqlite_overlay";
const COPY_SUFFIX: &str = "_pg2sqlite_overlay";
const REVIVAL_SUFFIX: &str = "_pg2sqlite_revival";
pub(crate) const TOMBSTONE: &str = "pg2sqlite_tombstone";

/// Whether `table` keeps an overlay: it is placed in the attached database
/// and the derived region of some `BEFORE` or `AFTER` trigger writes it.
pub(crate) fn is_overlaid(table: &CreateTable, options: &Pg2SqliteOptions) -> bool {
    options.get_server_derived_write_function().is_some()
        && options
            .get_attached_tables()
            .is_some_and(|attached| attached.is_overlaid(table.table_name()))
}

/// Whether the table `name` resolves to keeps an overlay, `false` for a name
/// naming no table.
///
/// # Errors
///
/// Fails as [`resolve_translation_table`] does.
pub(crate) fn names_overlaid_table(
    schema: &ParserDB,
    name: &ObjectName,
    options: &Pg2SqliteOptions,
) -> Result<bool, Error> {
    Ok(resolve_translation_table(schema, name)?.is_some_and(|table| is_overlaid(table, options)))
}

/// Records on `options` the attached tables the derived regions of the
/// triggers `initial` holds and of `declared` write. Each declared trigger
/// comes with the schema it was declared against.
///
/// # Errors
///
/// Refuses a table `initial` holds that only a declared trigger derives
/// writes into, since the batch that created it emitted no overlay, and
/// fails as [`derived_targets`] does.
pub(crate) fn record_overlaid_tables<'t>(
    options: &mut Pg2SqliteOptions,
    initial: Option<&'t ParserDB>,
    declared: impl IntoIterator<Item = (&'t CreateTrigger, &'t ParserDB)>,
) -> Result<(), Error> {
    let Some(attached) = options.get_attached_tables() else { return Ok(()) };
    if options.get_server_derived_write_function().is_none() {
        return Ok(());
    }
    let context = TranslationContext::new(options);
    let seeded: Vec<(&CreateTrigger, &ParserDB)> = initial
        .into_iter()
        .flat_map(|schema| schema.triggers().map(move |trigger| (trigger, schema)))
        .collect();
    let before = derived_targets(seeded.iter().copied(), &context)?;
    let after = derived_targets(seeded.into_iter().chain(declared), &context)?;
    let writes = |targets: &Option<BTreeSet<String>>, table: &str| {
        targets
            .as_ref()
            .is_none_or(|written| written.iter().any(|name| name.eq_ignore_ascii_case(table)))
    };
    let mut overlaid = Vec::new();
    for table in attached.tables() {
        if !writes(&after, table) {
            continue;
        }
        let created_earlier = initial.is_some_and(|schema| {
            schema.tables().any(|held| held.table_name().eq_ignore_ascii_case(table))
        });
        if created_earlier && !writes(&before, table) {
            return Err(refusal(
                table,
                "an earlier batch created it without the overlay, which this batch cannot add",
            ));
        }
        overlaid.push(table.clone());
    }
    drop(context);
    options.set_overlaid_tables(overlaid);
    Ok(())
}

/// The name every statement reads and writes `table`'s rows by: the table's
/// own, or its backing table's where a view stands for it.
///
/// # Errors
///
/// Fails as [`has_backing_table`] does.
pub(crate) fn rows_name(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<String, Error> {
    let name = table.table_name();
    Ok(if has_backing_table(table, schema, options)? {
        format!("{name}{}", options.get_rls_table_suffix())
    } else {
        name.to_owned()
    })
}

/// The storage table of the overlaid table read by `rows`.
pub(crate) fn storage_name(rows: &str) -> String {
    format!("{rows}{STORAGE_SUFFIX}")
}

/// What `table`'s name takes on to name its storage table, when it is
/// overlaid.
///
/// # Errors
///
/// Fails as [`has_backing_table`] does.
pub(crate) fn storage_suffix(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<String, Error> {
    let backing = if has_backing_table(table, schema, options)? {
        options.get_rls_table_suffix()
    } else {
        ""
    };
    Ok(format!("{backing}{STORAGE_SUFFIX}"))
}

/// The overlay table of the overlaid table read by `rows`.
pub(crate) fn overlay_name(rows: &str) -> String {
    format!("{rows}{OVERLAY_SUFFIX}")
}

/// Whether `name` names an overlay table.
pub(crate) fn is_overlay_table(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(OVERLAY_SUFFIX)
}

/// The refusal for `what`, which the overlay of `table` cannot carry.
pub(crate) fn refusal(table: &str, what: &str) -> Error {
    Error::UnusableAttachedTables {
        reason: format!(
            "`{table}` keeps a per-connection overlay, since a trigger derives writes into it, \
             and {what}"
        ),
    }
}

fn name(value: &str) -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(value))])
}

fn column(value: &str) -> Expr {
    Expr::Identifier(quoted_ident(value))
}

fn binary(left: Expr, op: BinaryOperator, right: Expr) -> Expr {
    Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right) }
}

fn all(conditions: impl IntoIterator<Item = Expr>) -> Expr {
    conditions
        .into_iter()
        .reduce(|left, right| binary(left, BinaryOperator::And, right))
        .unwrap_or_else(|| Expr::Value(sqlparser::ast::Value::Boolean(true).into()))
}

fn any(conditions: impl IntoIterator<Item = Expr>) -> Expr {
    conditions
        .into_iter()
        .reduce(|left, right| binary(left, BinaryOperator::Or, right))
        .unwrap_or_else(|| Expr::Value(sqlparser::ast::Value::Boolean(false).into()))
}

fn not(condition: Expr) -> Expr {
    Expr::UnaryOp { op: UnaryOperator::Not, expr: Box::new(Expr::Nested(Box::new(condition))) }
}

fn nested(condition: Expr) -> Expr {
    Expr::Nested(Box::new(condition))
}

fn exists(table: &str, selection: Expr) -> Expr {
    Expr::Exists {
        subquery: Box::new(single_expr_query(
            integer_literal(1),
            from_relation(plain_table_factor(name(table))),
            Some(selection),
        )),
        negated: false,
    }
}

/// `SELECT <values> WHERE <condition>`, a source of one row or none.
fn row_where(values: Vec<Expr>, condition: Expr) -> sqlparser::ast::Query {
    make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            values.into_iter().map(SelectItem::UnnamedExpr).collect(),
            Vec::new(),
            Some(condition),
        ))),
    )
}

fn tombstone_is(prefix: &str, value: i64) -> Expr {
    binary(prefixed_column_expr(prefix, TOMBSTONE), BinaryOperator::Eq, integer_literal(value))
}

/// The objects standing for one overlaid table, with the expressions its
/// routing triggers share.
struct Routing<'t> {
    rows: &'t str,
    storage: String,
    overlay: String,
    key: Vec<String>,
    /// The derived-write function, which answers how many derived runs are
    /// open when called with `0`.
    derived: &'t str,
}

impl Routing<'_> {
    /// `<key> = <prefix>.<key>` over every key column.
    fn key_is(&self, prefix: &str) -> Expr {
        all(self
            .key
            .iter()
            .map(|key| binary(column(key), BinaryOperator::Eq, prefixed_column_expr(prefix, key))))
    }

    fn open(&self) -> Expr {
        binary(
            simple_function_expr(self.derived, vec![integer_literal(0)], None),
            BinaryOperator::Gt,
            integer_literal(0),
        )
    }

    /// Whether the overlay holds the key of `prefix`, live or as a
    /// tombstone.
    fn held(&self, prefix: &str) -> Expr {
        exists(&self.overlay, self.key_is(prefix))
    }

    fn held_as(&self, prefix: &str, tombstone: i64) -> Expr {
        exists(
            &self.overlay,
            binary(
                self.key_is(prefix),
                BinaryOperator::And,
                binary(column(TOMBSTONE), BinaryOperator::Eq, integer_literal(tombstone)),
            ),
        )
    }

    fn stored(&self, prefix: &str) -> Expr {
        exists(&self.storage, self.key_is(prefix))
    }

    /// Whether `prefix`'s key names a row the merged rows already show.
    fn shown(&self, prefix: &str) -> Expr {
        any([self.held_as(prefix, 0), nested(all([not(self.held(prefix)), self.stored(prefix)]))])
    }

    /// Whether the new row's key differs from the old row's.
    fn moved(&self) -> Expr {
        any(self.key.iter().map(|key| {
            Expr::IsDistinctFrom(
                Box::new(prefixed_column_expr("NEW", key)),
                Box::new(prefixed_column_expr("OLD", key)),
            )
        }))
    }

    fn unique_violation(&self, condition: Expr) -> Statement {
        let columns: Vec<String> =
            self.key.iter().map(|key| format!("{}.{key}", self.rows)).collect();
        ast_builder::raise_statement(
            "ABORT",
            Some(&format!("UNIQUE constraint failed: {}", columns.join(", "))),
            Some(condition),
        )
    }
}

/// The overlay table, the merging view and its routing triggers of `table`,
/// whose storage the translation emitted as `storage`.
///
/// # Errors
///
/// Refuses a table without a primary key, and propagates a lookup failure and
/// a default that cannot be translated.
pub(crate) fn overlay_statements(
    table: &CreateTable,
    storage: &CreateTable,
    rows: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let key: Vec<String> =
        table.primary_key_columns(schema)?.map(|key| key.column_name().to_owned()).collect();
    if key.is_empty() {
        return Err(refusal(
            table.table_name(),
            "has no primary key, which the overlay keys its rows by",
        ));
    }
    let derived = options
        .get_server_derived_write_function()
        .expect("an overlaid table has a derived-write function");
    let routing =
        Routing { rows, storage: storage_name(rows), overlay: overlay_name(rows), key, derived };
    let columns = trigger_columns(table, schema, options, None)?;
    let written: Vec<&TriggerColumn> =
        columns.iter().filter(|column| column.is_forwarded()).collect();
    let defaulted = written
        .iter()
        .map(|column| column.forwarded_value().translate_with_warnings(schema, options, emit))
        .collect::<Result<Vec<_>, Error>>()?;
    let rowid_key = rowid_alias(storage, &routing.key);

    let mut statements = vec![overlay_table(storage, &routing.overlay)];
    statements.push(merged_view(&routing, &columns));
    statements.push(insert_trigger(&routing, &written, defaulted, rowid_key));
    statements.push(update_trigger(&routing, &written));
    statements.push(delete_trigger(&routing, &written));
    Ok(statements)
}

/// The key column SQLite assigns as the `rowid` when an insert leaves it
/// out, where the key is one `INTEGER` column.
fn rowid_alias<'k>(storage: &CreateTable, key: &'k [String]) -> Option<&'k str> {
    let [key] = key else { return None };
    storage
        .columns
        .iter()
        .find(|column| column.name.value.eq_ignore_ascii_case(key))
        .filter(|column| matches!(column.data_type, DataType::Integer(None)))
        .map(|_| key.as_str())
}

/// The overlay table: `storage`'s columns with their types, defaults,
/// computations and checks, keyed by the primary key alone, and the
/// tombstone flag.
fn overlay_table(storage: &CreateTable, overlay: &str) -> Statement {
    let mut table = storage.clone();
    table.name = name(overlay);
    for column in &mut table.columns {
        column.options.retain(|option| {
            !matches!(option.option, ColumnOption::Unique(_) | ColumnOption::ForeignKey(_))
        });
    }
    table.constraints.retain(|constraint| {
        matches!(constraint, TableConstraint::PrimaryKey(_) | TableConstraint::Check(_))
    });
    table.columns.push(ColumnDef {
        name: quoted_ident(TOMBSTONE),
        data_type: DataType::Integer(None),
        options: vec![
            ColumnOptionDef { name: None, option: ColumnOption::NotNull },
            ColumnOptionDef { name: None, option: ColumnOption::Default(integer_literal(0)) },
            ColumnOptionDef {
                name: None,
                option: ColumnOption::Check(sqlparser::ast::CheckConstraint {
                    name: None,
                    expr: Box::new(Expr::In {
                        expr: Box::new(column(TOMBSTONE)),
                        target: sqlparser::ast::InTarget::List(vec![
                            integer_literal(0),
                            integer_literal(1),
                        ]),
                        negated: false,
                        global: false,
                    }),
                    enforced: None,
                    no_inherit: false,
                    on_conflict: None,
                }),
            },
        ],
    });
    Statement::CreateTable(table)
}

/// The view under the rows name: the overlay's live rows and the storage rows
/// whose key the overlay does not hold.
fn merged_view(routing: &Routing<'_>, columns: &[TriggerColumn]) -> Statement {
    let projection =
        || columns.iter().map(|each| SelectItem::UnnamedExpr(column(&each.name))).collect();
    let live = make_simple_select(
        projection(),
        from_relation(plain_table_factor(name(&routing.overlay))),
        Some(binary(column(TOMBSTONE), BinaryOperator::Eq, integer_literal(0))),
    );
    let shadowed = all(routing.key.iter().map(|key| {
        binary(
            Expr::CompoundIdentifier(vec![quoted_ident(&routing.overlay), quoted_ident(key)]),
            BinaryOperator::Eq,
            Expr::CompoundIdentifier(vec![quoted_ident(&routing.storage), quoted_ident(key)]),
        )
    }));
    let stored = make_simple_select(
        projection(),
        from_relation(plain_table_factor(name(&routing.storage))),
        Some(not(exists(&routing.overlay, shadowed))),
    );
    ast_builder::create_view(
        name(routing.rows),
        Vec::new(),
        make_query(
            None,
            SetExpr::SetOperation {
                op: SetOperator::Union,
                set_quantifier: SetQuantifier::All,
                left: Box::new(SetExpr::Select(Box::new(live))),
                right: Box::new(SetExpr::Select(Box::new(stored))),
            },
        ),
    )
}

fn route(
    routing: &Routing<'_>,
    verb: &str,
    event: TriggerEvent,
    body: Vec<Statement>,
) -> Statement {
    ast_builder::trigger(
        name(&format!("{}_{verb}_trigger", routing.rows)),
        name(routing.rows),
        TriggerPeriod::InsteadOf,
        event,
        true,
        None,
        body,
    )
}

fn insert(table: &str, columns: Vec<ObjectName>, source: sqlparser::ast::Query) -> Statement {
    ast_builder::insert(name(table), columns, source)
}

fn column_names<'c>(columns: impl IntoIterator<Item = &'c TriggerColumn>) -> Vec<ObjectName> {
    columns.into_iter().map(|each| name(&each.name)).collect()
}

/// The routed `INSTEAD OF INSERT`: to storage outside a run on a key the
/// overlay does not hold, refused on a key the merged rows show, reviving a
/// tombstone, and to the overlay inside a run.
fn insert_trigger(
    routing: &Routing<'_>,
    written: &[&TriggerColumn],
    defaulted: Vec<Expr>,
    rowid_key: Option<&str>,
) -> Statement {
    let routed = any([routing.open(), routing.held("NEW")]);
    let to_storage = insert(
        &routing.storage,
        column_names(written.iter().copied()),
        row_where(defaulted.clone(), all([not(routing.open()), not(routing.held("NEW"))])),
    );
    let refused = routing.unique_violation(all([nested(routed), nested(routing.shown("NEW"))]));
    let revived = ast_builder::update(
        name(&routing.overlay),
        written
            .iter()
            .zip(&defaulted)
            .map(|(each, value)| assign(&each.name, value.clone()))
            .chain([assign(TOMBSTONE, integer_literal(0))])
            .collect(),
        Some(all([
            routing.key_is("NEW"),
            binary(column(TOMBSTONE), BinaryOperator::Eq, integer_literal(1)),
        ])),
    );
    // SQLite numbers a left-out rowid key past the overlay's own rows alone,
    // which another connection's storage rows may already hold.
    let assigned: Vec<Expr> = written
        .iter()
        .zip(defaulted)
        .map(|(each, value)| {
            match rowid_key {
                Some(key) if key == each.name => {
                    simple_function_expr(
                        "COALESCE",
                        vec![
                            value,
                            binary(
                                next_key(routing, key),
                                BinaryOperator::Plus,
                                integer_literal(1),
                            ),
                        ],
                        None,
                    )
                }
                _ => value,
            }
        })
        .chain([integer_literal(0)])
        .collect();
    let mut columns = column_names(written.iter().copied());
    columns.push(name(TOMBSTONE));
    let to_overlay = insert(
        &routing.overlay,
        columns,
        row_where(assigned, all([routing.open(), not(routing.held("NEW"))])),
    );
    route(routing, "insert", TriggerEvent::Insert, vec![to_storage, refused, revived, to_overlay])
}

/// `MAX(<highest storage key>, <highest overlay key>)`, each `0` when empty.
fn next_key(routing: &Routing<'_>, key: &str) -> Expr {
    let highest = |table: &str| {
        Expr::Subquery(Box::new(single_expr_query(
            simple_function_expr(
                "COALESCE",
                vec![simple_function_expr("MAX", vec![column(key)], None), integer_literal(0)],
                None,
            ),
            from_relation(plain_table_factor(name(table))),
            None,
        )))
    };
    simple_function_expr("MAX", vec![highest(&routing.storage), highest(&routing.overlay)], None)
}

fn assign(target: &str, value: Expr) -> Assignment {
    Assignment { target: AssignmentTarget::ColumnName(name(target)), value }
}

/// The overlay copy of the old row, live or as a tombstone, written where
/// `condition` holds.
fn copy_old(
    routing: &Routing<'_>,
    written: &[&TriggerColumn],
    tombstone: i64,
    condition: Expr,
) -> Statement {
    let mut columns = column_names(written.iter().copied());
    columns.push(name(TOMBSTONE));
    let values = written
        .iter()
        .map(|each| prefixed_column_expr("OLD", &each.name))
        .chain([integer_literal(tombstone)])
        .collect();
    insert(&routing.overlay, columns, row_where(values, condition))
}

/// The routed `INSTEAD OF UPDATE`: to storage outside a run on keys the
/// overlay holds neither of, otherwise to the overlay, copying the old row
/// there first and leaving a tombstone at a key the update moves away from a
/// storage row.
fn update_trigger(routing: &Routing<'_>, written: &[&TriggerColumn]) -> Statement {
    let routed = any([routing.open(), routing.held("OLD"), routing.held("NEW")]);
    let assignments = || {
        written
            .iter()
            .map(|each| assign(&each.name, prefixed_column_expr("NEW", &each.name)))
            .collect::<Vec<_>>()
    };
    let to_storage = ast_builder::update(
        name(&routing.storage),
        assignments(),
        Some(all([routing.key_is("OLD"), not(routed.clone())])),
    );
    let refused = routing.unique_violation(all([
        nested(routed.clone()),
        nested(routing.moved()),
        nested(routing.shown("NEW")),
    ]));
    let copied = copy_old(routing, written, 0, all([nested(routed), not(routing.held("OLD"))]));
    let cleared = ast_builder::delete(
        name(&routing.overlay),
        Some(all([
            routing.key_is("NEW"),
            binary(column(TOMBSTONE), BinaryOperator::Eq, integer_literal(1)),
            nested(routing.moved()),
        ])),
    );
    let changed =
        ast_builder::update(name(&routing.overlay), assignments(), Some(routing.key_is("OLD")));
    let left = copy_old(
        routing,
        written,
        1,
        all([nested(routing.moved()), routing.stored("OLD"), not(routing.held("OLD"))]),
    );
    route(
        routing,
        "update",
        TriggerEvent::Update(Vec::new()),
        vec![to_storage, refused, copied, cleared, changed, left],
    )
}

/// The routed `INSTEAD OF DELETE`: from storage outside a run on a key the
/// overlay does not hold, otherwise a tombstone in the overlay over a copy of
/// the old row.
fn delete_trigger(routing: &Routing<'_>, written: &[&TriggerColumn]) -> Statement {
    let routed = any([routing.open(), routing.held("OLD")]);
    let from_storage = ast_builder::delete(
        name(&routing.storage),
        Some(all([routing.key_is("OLD"), not(routed)])),
    );
    let copied = copy_old(routing, written, 0, all([routing.open(), not(routing.held("OLD"))]));
    let buried = ast_builder::update(
        name(&routing.overlay),
        vec![assign(TOMBSTONE, integer_literal(1))],
        Some(all([
            routing.key_is("OLD"),
            binary(column(TOMBSTONE), BinaryOperator::Eq, integer_literal(0)),
        ])),
    );
    route(routing, "delete", TriggerEvent::Delete, vec![from_storage, copied, buried])
}

/// Moves every object `statements` creates on the rows name `rows` to the
/// storage table: the table itself, its indexes and its `BEFORE` and `AFTER`
/// triggers, whose own writes by `rowid` name it too.
pub(crate) fn move_to_storage(statements: &mut [Statement], rows: &str) {
    let storage = storage_name(rows);
    let is_rows = |candidate: &ObjectName| {
        last_ident(candidate).is_some_and(|ident| ident.value.eq_ignore_ascii_case(rows))
    };
    for statement in statements {
        match statement {
            Statement::CreateTable(create) if is_rows(&create.name) => {
                create.name = append_storage(&create.name, rows, &storage);
            }
            Statement::CreateIndex(index) if is_rows(&index.table_name) => {
                index.table_name = append_storage(&index.table_name, rows, &storage);
            }
            Statement::CreateTrigger(trigger)
                if is_rows(&trigger.table_name)
                    && !matches!(trigger.period, Some(TriggerPeriod::InsteadOf)) =>
            {
                trigger.table_name = append_storage(&trigger.table_name, rows, &storage);
            }
            _ => {}
        }
    }
}

fn append_storage(current: &ObjectName, rows: &str, storage: &str) -> ObjectName {
    let mut renamed = current.clone();
    if let Some(ObjectNamePart::Identifier(last)) = renamed.0.last_mut()
        && last.value.eq_ignore_ascii_case(rows)
    {
        storage.clone_into(&mut last.value);
        last.quote_style = quoted_ident(storage).quote_style;
    }
    renamed
}

/// One overlaid table as the triggers on its storage name it.
struct Mirrored {
    /// The table's name, which its marker tables are named after.
    logical: String,
    overlay: String,
    key: Vec<String>,
}

/// The overlay copies of the triggers on each overlaid table's storage,
/// followed through a batch so that a trigger dropped takes its copies.
#[derive(Default)]
pub(crate) struct OverlayMirrors {
    /// The copies of each live trigger, by folded trigger name.
    copies: BTreeMap<String, Vec<ObjectName>>,
}

impl OverlayMirrors {
    /// Appends, right after each `BEFORE` or `AFTER` trigger `emitted` creates
    /// on an overlaid table's storage, its copies on the overlay, and after
    /// each trigger it drops, the drops of its copies.
    ///
    /// # Errors
    ///
    /// Fails as [`is_overlaid`] does.
    pub(crate) fn follow(
        &mut self,
        emitted: &mut Vec<Statement>,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<(), Error> {
        let configured = options.get_attached_tables().is_some()
            && options.get_server_derived_write_function().is_some();
        let mirrors_any = configured
            && emitted.iter().any(|statement| {
                match statement {
                    Statement::CreateTrigger(trigger) => {
                        last_ident(&trigger.table_name).is_some_and(|table| {
                            table.value.to_ascii_lowercase().ends_with(STORAGE_SUFFIX)
                        })
                    }
                    Statement::DropTrigger(_) => true,
                    _ => false,
                }
            });
        if !mirrors_any {
            return Ok(());
        }
        let tables = mirrored_tables(schema, options)?;
        let mut followed = Vec::with_capacity(emitted.len());
        for statement in emitted.drain(..) {
            let extra = match &statement {
                Statement::CreateTrigger(trigger) => {
                    let mirrored = last_ident(&trigger.table_name)
                        .and_then(|table| tables.get(&table.value.to_ascii_lowercase()));
                    match mirrored {
                        Some(mirrored)
                            if matches!(
                                trigger.period,
                                Some(TriggerPeriod::Before | TriggerPeriod::After)
                            ) =>
                        {
                            let copies = copies_of(trigger, mirrored);
                            self.copies.insert(
                                folded(&trigger.name),
                                copies
                                    .iter()
                                    .filter_map(|copy| {
                                        match copy {
                                            Statement::CreateTrigger(copy) => {
                                                Some(copy.name.clone())
                                            }
                                            _ => None,
                                        }
                                    })
                                    .collect(),
                            );
                            copies
                        }
                        _ => Vec::new(),
                    }
                }
                // A trigger the batch's starting schema holds may have copies
                // the batch never saw created.
                Statement::DropTrigger(drop) => {
                    self.copies
                        .remove(&folded(&drop.trigger_name))
                        .unwrap_or_else(|| {
                            [COPY_SUFFIX, REVIVAL_SUFFIX]
                                .into_iter()
                                .map(|suffix| append_suffix(&drop.trigger_name, suffix))
                                .collect()
                        })
                        .into_iter()
                        .map(|copy| {
                            Statement::DropTrigger(DropTrigger {
                                if_exists: true,
                                trigger_name: copy,
                                table_name: None,
                                option: None,
                            })
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            followed.push(statement);
            followed.extend(extra);
        }
        *emitted = followed;
        Ok(())
    }
}

fn folded(name: &ObjectName) -> String {
    last_ident_value_or_display(name).to_ascii_lowercase()
}

/// Every overlaid table of `schema`, by its folded storage name.
fn mirrored_tables(
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<BTreeMap<String, Mirrored>, Error> {
    let mut tables = BTreeMap::new();
    for table in schema.tables() {
        if !is_overlaid(table, options) {
            continue;
        }
        let rows = rows_name(table, schema, options)?;
        tables.insert(
            storage_name(&rows).to_ascii_lowercase(),
            Mirrored {
                logical: table.table_name().to_owned(),
                overlay: overlay_name(&rows),
                key: table
                    .primary_key_columns(schema)?
                    .map(|key| key.column_name().to_owned())
                    .collect(),
            },
        );
    }
    Ok(tables)
}

/// The copies of `trigger`, a trigger on an overlaid table's storage, on the
/// overlay: each firing on the overlay write that stands for the trigger's
/// event, with the storage and the marker tables in its body renamed to the
/// overlay's.
fn copies_of(trigger: &CreateTrigger, mirrored: &Mirrored) -> Vec<Statement> {
    let Some(event) = trigger.events.first() else { return Vec::new() };
    let tombstone = || vec![quoted_ident(TOMBSTONE)];
    let fresh_key = not(exists(
        &last_ident_value_or_display(&trigger.table_name),
        all(mirrored
            .key
            .iter()
            .map(|key| binary(column(key), BinaryOperator::Eq, prefixed_column_expr("NEW", key)))),
    ));
    let mappings: Vec<(&str, TriggerEvent, Expr)> = match event {
        TriggerEvent::Insert => {
            vec![
                (COPY_SUFFIX, TriggerEvent::Insert, all([tombstone_is("NEW", 0), fresh_key])),
                (
                    REVIVAL_SUFFIX,
                    TriggerEvent::Update(tombstone()),
                    all([tombstone_is("OLD", 1), tombstone_is("NEW", 0)]),
                ),
            ]
        }
        TriggerEvent::Update(columns) => {
            vec![(
                COPY_SUFFIX,
                TriggerEvent::Update(columns.clone()),
                all([tombstone_is("OLD", 0), tombstone_is("NEW", 0)]),
            )]
        }
        TriggerEvent::Delete => {
            vec![(
                COPY_SUFFIX,
                TriggerEvent::Update(tombstone()),
                all([tombstone_is("OLD", 0), tombstone_is("NEW", 1)]),
            )]
        }
        TriggerEvent::Truncate => Vec::new(),
    };
    let storage = last_ident_value_or_display(&trigger.table_name).to_ascii_lowercase();
    let logical = ObjectName(vec![ObjectNamePart::Identifier(sqlparser::ast::Ident::new(
        mirrored.logical.clone(),
    ))]);
    let overlay = name(&mirrored.overlay);
    let markers: Vec<(String, ObjectName)> = [
        (reentry::marker_table(&logical), reentry::marker_table(&overlay)),
        (reentry::forward_marker_table(&logical), reentry::forward_marker_table(&overlay)),
        (reentry::record_table(&logical), reentry::record_table(&overlay)),
    ]
    .into_iter()
    .map(|(from, to)| (folded(&from), to))
    .collect();
    mappings
        .into_iter()
        .map(|(suffix, event, mapping)| {
            let mut copy = trigger.clone();
            copy.name = append_suffix(&trigger.name, suffix);
            copy.table_name = overlay.clone();
            copy.events = vec![event];
            // The mapping reads the storage itself, so it joins after the
            // renaming.
            let _ = visit_relations_mut(&mut copy, |relation| {
                let relation_name = folded(relation);
                if relation_name == storage {
                    *relation = overlay.clone();
                } else if let Some((_, renamed)) =
                    markers.iter().find(|(marker, _)| *marker == relation_name)
                {
                    *relation = renamed.clone();
                }
                ControlFlow::<()>::Continue(())
            });
            copy.condition = Some(match copy.condition.take() {
                Some(condition) => all([mapping, nested(condition)]),
                None => mapping,
            });
            Statement::CreateTrigger(copy)
        })
        .collect()
}
