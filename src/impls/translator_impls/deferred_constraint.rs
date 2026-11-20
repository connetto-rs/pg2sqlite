//! `DEFERRABLE INITIALLY DEFERRED` constraint triggers, whose bodies
//! PostgreSQL runs at commit against the rows as they stand then.
//!
//! SQLite has no commit-time hook in SQL, and the one check it defers is the
//! foreign key counter. So each firing is queued with the `OLD` and `NEW`
//! values its body reads. A queued row's key is a deferred foreign key into
//! [`BARRIERS`], which never holds a row, so a `COMMIT` fails with `FOREIGN
//! KEY constraint failed` while any firing is still queued, and so does an
//! autocommit write that queues one. [`drain`] runs the queue: deleting a row
//! from [`CHECKS`] cascades to the firing's payload row, whose `AFTER DELETE`
//! trigger is the translated body, so a violation aborts the drain with the
//! body's own message and a passing body clears the row. Firings drain in the
//! order they were queued, and a rollback takes its queued rows with it.

use alloc::collections::BTreeMap;
use core::ops::ControlFlow;

use sqlparser::ast::{
    BinaryOperator, ColumnDef, ColumnOption, ColumnOptionDef, ConditionalStatements,
    ConstraintCharacteristics, CreateTrigger, DataType, DeferrableInitial, DropTrigger, Expr,
    ForeignKeyConstraint, Ident, ObjectName, ObjectNamePart, ObjectType, ReferentialAction,
    Statement, TriggerEvent, TriggerPeriod, VisitMut, VisitorMut,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        function_helpers::{simple_function_expr, string_literal},
        object_name::{append_suffix, last_ident, quoted_ident},
        translator_impls::rls::{generated_key_column, generated_text_column, one_of},
    },
    traits::translator::TranslatorWithContext,
};

/// The queue of firings not yet checked, in firing order.
pub(crate) const CHECKS: &str = "pg2sqlite_deferred_checks";

/// The always-empty table a queued firing's deferred foreign key names.
const BARRIERS: &str = "pg2sqlite_deferred_barriers";

/// The key of [`BARRIERS`], of [`CHECKS`] and of every payload table, which
/// orders the firings.
const ID: &str = "id";

/// The payload column naming the event that queued the firing.
const EVENT: &str = "event";

/// A row event a deferred constraint trigger fires on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredEvent {
    Insert,
    Update,
    Delete,
}

impl DeferredEvent {
    const ALL: [Self; 3] = [Self::Insert, Self::Update, Self::Delete];

    fn keyword(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }

    /// Whether the event gives the body an `OLD` row and a `NEW` row.
    fn rows(self) -> (bool, bool) {
        match self {
            Self::Insert => (false, true),
            Self::Update => (true, true),
            Self::Delete => (true, false),
        }
    }

    fn of(event: &TriggerEvent) -> Option<Self> {
        match event {
            TriggerEvent::Insert => Some(Self::Insert),
            TriggerEvent::Update(_) => Some(Self::Update),
            TriggerEvent::Delete => Some(Self::Delete),
            TriggerEvent::Truncate => None,
        }
    }
}

/// A deferred constraint trigger the translation unit creates, kept so that
/// dropping it or its table drops the objects it translates into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeferredTrigger {
    name: ObjectName,
    table: String,
    events: Vec<DeferredEvent>,
}

impl DeferredTrigger {
    /// The record of `trigger`, `None` unless it is a deferred constraint
    /// trigger.
    pub(crate) fn of(trigger: &CreateTrigger) -> Option<Self> {
        is_deferred(trigger).then(|| {
            Self {
                name: trigger.name.clone(),
                table: last_ident(&trigger.table_name)
                    .map(|ident| ident.value.to_ascii_lowercase())
                    .unwrap_or_default(),
                events: trigger.events.iter().filter_map(DeferredEvent::of).collect(),
            }
        })
    }

    /// Whether the trigger is named `name`, as SQLite compares names.
    pub(crate) fn is_named(&self, name: &ObjectName) -> bool {
        match (last_ident(&self.name), last_ident(name)) {
            (Some(own), Some(other)) => own.value.eq_ignore_ascii_case(&other.value),
            _ => false,
        }
    }

    /// Whether the trigger is attached to `table`, as SQLite compares names.
    pub(crate) fn is_on(&self, table: &ObjectName) -> bool {
        last_ident(table).is_some_and(|ident| ident.value.eq_ignore_ascii_case(&self.table))
    }

    /// The statements dropping every object the trigger translates into,
    /// `if_exists` as the source `DROP TRIGGER` says.
    pub(crate) fn drop_statements(&self, if_exists: bool) -> Vec<Statement> {
        let mut statements: Vec<Statement> = self
            .events
            .iter()
            .map(|event| {
                Statement::DropTrigger(DropTrigger {
                    if_exists,
                    trigger_name: queue_trigger_name(&self.name, *event),
                    table_name: None,
                    option: None,
                })
            })
            .collect();
        statements.push(self.drop_payload(if_exists));
        statements
    }

    /// The statement dropping the payload table, which takes the check
    /// triggers attached to it along.
    pub(crate) fn drop_payload(&self, if_exists: bool) -> Statement {
        Statement::Drop {
            object_type: ObjectType::Table,
            if_exists,
            names: vec![payload_name(&self.name)],
            cascade: false,
            restrict: false,
            purge: false,
            temporary: false,
            table: None,
        }
    }
}

/// Whether `trigger` is a constraint trigger PostgreSQL runs at commit.
///
/// `INITIALLY DEFERRED` without `DEFERRABLE` is deferrable in PostgreSQL, and
/// `NOT DEFERRABLE INITIALLY DEFERRED` is refused by [`check_clauses`].
#[must_use]
pub(crate) fn is_deferred(trigger: &CreateTrigger) -> bool {
    trigger.is_constraint
        && trigger.characteristics.as_ref().is_some_and(|characteristics| {
            characteristics.initially == Some(DeferrableInitial::Deferred)
        })
}

/// Refuses the clauses PostgreSQL refuses on a constraint trigger, and the
/// `FROM` clause, which only PostgreSQL's own foreign keys use.
///
/// # Errors
///
/// Returns [`Error::TranslationRefusal`] for each such clause.
pub(crate) fn check_clauses(trigger: &CreateTrigger) -> Result<(), Error> {
    if trigger.or_replace {
        return Err(Error::forward_refusal(
            "CREATE OR REPLACE CONSTRAINT TRIGGER is refused by PostgreSQL. Drop the trigger and \
             create it again.",
        ));
    }
    if !matches!(trigger.period, Some(TriggerPeriod::After)) {
        return Err(Error::forward_refusal(
            "a constraint trigger must be an AFTER trigger, as PostgreSQL requires.",
        ));
    }
    if let Some(table) = &trigger.referenced_table_name {
        return Err(Error::forward_refusal(format!(
            "the FROM {table} clause of a constraint trigger serves PostgreSQL's own foreign keys, \
             and SQLite enforces foreign keys without it. Declare the foreign key instead."
        )));
    }
    if let Some(ConstraintCharacteristics { deferrable, initially, enforced }) =
        &trigger.characteristics
    {
        if enforced.is_some() {
            return Err(Error::forward_refusal(
                "a constraint trigger takes no ENFORCED or NOT ENFORCED clause in PostgreSQL.",
            ));
        }
        if *deferrable == Some(false) && *initially == Some(DeferrableInitial::Deferred) {
            return Err(Error::forward_refusal(
                "a constraint trigger declared INITIALLY DEFERRED must be DEFERRABLE, as \
                 PostgreSQL requires.",
            ));
        }
    }
    Ok(())
}

/// `CREATE TABLE IF NOT EXISTS` for [`BARRIERS`] and [`CHECKS`], which a
/// translation creating a deferred constraint trigger needs.
///
/// The key of [`CHECKS`] references the key of [`BARRIERS`], so a queued row
/// extends a barrier row that never exists. The `ON DELETE CASCADE` such an
/// extension carries never fires.
pub(crate) fn queue_tables() -> [Statement; 2] {
    let barriers = ast_builder::create_table(plain_name(BARRIERS), vec![generated_key_column()]);
    let mut key = generated_key_column();
    key.options.push(foreign_key(BARRIERS, Some(ReferentialAction::Cascade), true));
    let checks = ast_builder::create_table(plain_name(CHECKS), vec![key]);
    [barriers, checks]
}

/// `DELETE FROM` [`CHECKS`], which runs every queued body and fails on the
/// first violation with that body's message.
#[must_use]
pub(crate) fn drain() -> Statement {
    ast_builder::delete(plain_name(CHECKS), None)
}

/// Translates a deferred constraint trigger into its payload table, one
/// trigger per event queueing the firing, and one per event running the body
/// when the drain deletes the firing.
///
/// Each event is first translated as the immediate trigger it would be, so
/// the `WHEN` clause, the write exemption and the body read as they do there.
/// The `WHEN` clause stays on the queueing trigger, since PostgreSQL evaluates
/// it when the event happens. The body moves to the payload table, its `NEW`
/// and `OLD` columns read from the values queued with the firing, `NULL` where
/// the event has no such row as in PostgreSQL.
///
/// # Errors
///
/// Returns [`Error::TranslationRefusal`] when the trigger's clauses or body
/// are refused, or when the body reads `NEW` or `OLD` other than one column at
/// a time.
pub(crate) fn translate(
    trigger: &CreateTrigger,
    schema: &sql_traits::structs::ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    check_clauses(trigger)?;
    let mut columns = PayloadColumns::default();
    let mut events = Vec::with_capacity(trigger.events.len());
    for event in &trigger.events {
        let mut immediate = immediate(trigger, event, schema, options, emit)?;
        let Some(kind) = DeferredEvent::of(event) else {
            unreachable!("the immediate translation refuses a TRUNCATE trigger")
        };
        let Some(ConditionalStatements::BeginEnd(body)) = &mut immediate.statements else {
            return Err(Error::forward_refusal(format!(
                "constraint trigger `{}` translated without a body",
                trigger.name
            )));
        };
        for statement in &mut body.statements {
            columns.read_from(statement)?;
        }
        events.push((kind, immediate));
    }

    let payload = payload_name(&trigger.name);
    let mut statements = vec![columns.table(&payload)];
    for (kind, immediate) in events {
        let CreateTrigger { table_name, condition, statements: body, events: mut fired, .. } =
            immediate;
        let (Some(ConditionalStatements::BeginEnd(body)), Some(event)) = (body, fired.pop()) else {
            unreachable!("every queued event was checked for a BEGIN ... END body above")
        };
        statements.push(ast_builder::trigger(
            queue_trigger_name(&trigger.name, kind),
            table_name,
            TriggerPeriod::After,
            event,
            true,
            condition,
            columns.queue(&payload, kind),
        ));
        statements.push(ast_builder::trigger(
            append_suffix(&trigger.name, &format!("_pg2sqlite_check_{}", kind.suffix())),
            payload.clone(),
            TriggerPeriod::After,
            TriggerEvent::Delete,
            true,
            Some(Expr::BinaryOp {
                left: Box::new(pseudo_column("OLD", quoted_ident(EVENT))),
                op: BinaryOperator::Eq,
                right: Box::new(string_literal(kind.keyword())),
            }),
            body.statements,
        ));
    }
    Ok(statements)
}

/// The trigger as PostgreSQL would fire it immediately on `event` alone,
/// translated as such.
fn immediate(
    trigger: &CreateTrigger,
    event: &TriggerEvent,
    schema: &sql_traits::structs::ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<CreateTrigger, Error> {
    let mut single = trigger.clone();
    single.events = vec![event.clone()];
    single.characteristics = None;
    let mut translated = single.translate_with_warnings(schema, options, emit)?;
    match (translated.pop(), translated.is_empty()) {
        (Some((None, translated)), true) => Ok(translated),
        _ => {
            Err(Error::forward_refusal(format!(
                "constraint trigger `{}` translates into more than one SQLite trigger per event, \
                 which a deferred check cannot queue as one firing",
                trigger.name
            )))
        }
    }
}

/// The name of the trigger queueing the firings of `event`.
fn queue_trigger_name(name: &ObjectName, event: DeferredEvent) -> ObjectName {
    append_suffix(name, &format!("_pg2sqlite_{}", event.suffix()))
}

/// The name of the table holding the trigger's queued firings.
fn payload_name(name: &ObjectName) -> ObjectName {
    append_suffix(name, "_pg2sqlite_rows")
}

fn plain_name(name: &str) -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))])
}

fn foreign_key(
    table: &str,
    on_delete: Option<ReferentialAction>,
    deferred: bool,
) -> ColumnOptionDef {
    ColumnOptionDef {
        name: None,
        option: ColumnOption::ForeignKey(ForeignKeyConstraint {
            name: None,
            index_name: None,
            columns: Vec::new(),
            foreign_table: plain_name(table),
            referred_columns: vec![quoted_ident(ID)],
            on_delete,
            on_update: None,
            match_kind: None,
            characteristics: deferred.then_some(ConstraintCharacteristics {
                deferrable: Some(true),
                initially: Some(DeferrableInitial::Deferred),
                enforced: None,
            }),
        }),
    }
}

fn pseudo_column(row: &str, column: Ident) -> Expr {
    Expr::CompoundIdentifier(vec![Ident::new(row), column])
}

/// Which pseudo-row a body column read names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PseudoRow {
    Old,
    New,
}

impl PseudoRow {
    fn of(ident: &Ident) -> Option<Self> {
        if ident.value.eq_ignore_ascii_case("OLD") {
            Some(Self::Old)
        } else if ident.value.eq_ignore_ascii_case("NEW") {
            Some(Self::New)
        } else {
            None
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            Self::Old => "OLD",
            Self::New => "NEW",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Old => "old_",
            Self::New => "new_",
        }
    }
}

/// The `OLD` and `NEW` columns the bodies read, each stored in a payload
/// column named after it, keyed as SQLite folds column names.
#[derive(Default)]
struct PayloadColumns {
    read: BTreeMap<(PseudoRow, String), Ident>,
}

impl PayloadColumns {
    fn payload_ident(row: PseudoRow, column: &Ident) -> Ident {
        quoted_ident(&format!("{}{}", row.prefix(), column.value))
    }

    /// Records the pseudo-row columns `statement` reads and rewrites each
    /// read to the payload column of the deleted queue row.
    fn read_from(&mut self, statement: &mut Statement) -> Result<(), Error> {
        let mut rewriter = Rewriter { columns: self };
        match statement.visit(&mut rewriter) {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        }
    }

    fn table(&self, name: &ObjectName) -> Statement {
        let mut key = generated_key_column();
        key.options.push(foreign_key(CHECKS, Some(ReferentialAction::Cascade), false));
        let mut columns = vec![
            key,
            generated_text_column(
                EVENT,
                one_of(EVENT, &DeferredEvent::ALL.map(DeferredEvent::keyword)),
            ),
        ];
        columns.extend(self.read.iter().map(|((row, _), column)| {
            ColumnDef {
                name: Self::payload_ident(*row, column),
                data_type: DataType::Custom(plain_name("ANY"), Vec::new()),
                options: Vec::new(),
            }
        }));
        ast_builder::create_table(name.clone(), columns)
    }

    /// The statements queueing one firing of `event`.
    fn queue(&self, payload: &ObjectName, event: DeferredEvent) -> Vec<Statement> {
        let (has_old, has_new) = event.rows();
        let mut targets = vec![plain_column(ID), plain_column(EVENT)];
        let mut values = vec![
            simple_function_expr("last_insert_rowid", Vec::new(), None),
            string_literal(event.keyword()),
        ];
        for ((row, _), column) in &self.read {
            targets.push(ObjectName(vec![ObjectNamePart::Identifier(Self::payload_ident(
                *row, column,
            ))]));
            let present = match row {
                PseudoRow::Old => has_old,
                PseudoRow::New => has_new,
            };
            values.push(if present {
                pseudo_column(row.keyword(), column.clone())
            } else {
                Expr::Value(sqlparser::ast::Value::Null.into())
            });
        }
        vec![
            ast_builder::insert(
                plain_name(CHECKS),
                vec![plain_column(ID)],
                ast_builder::values(vec![vec![Expr::Value(sqlparser::ast::Value::Null.into())]]),
            ),
            ast_builder::insert(payload.clone(), targets, ast_builder::values(vec![values])),
        ]
    }
}

fn plain_column(name: &str) -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(name))])
}

struct Rewriter<'a> {
    columns: &'a mut PayloadColumns,
}

impl VisitorMut for Rewriter<'_> {
    type Break = Error;

    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        match expr {
            Expr::CompoundIdentifier(parts) => {
                let Some(row) = parts.first().and_then(PseudoRow::of) else {
                    return ControlFlow::Continue(());
                };
                let [_, column] = parts.as_slice() else {
                    return ControlFlow::Break(whole_row_refusal(row));
                };
                let column = column.clone();
                let payload = PayloadColumns::payload_ident(row, &column);
                self.columns.read.entry((row, column.value.to_ascii_lowercase())).or_insert(column);
                *expr = pseudo_column("OLD", payload);
            }
            Expr::Identifier(ident) => {
                if let Some(row) = PseudoRow::of(ident) {
                    return ControlFlow::Break(whole_row_refusal(row));
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

fn whole_row_refusal(row: PseudoRow) -> Error {
    Error::forward_refusal(format!(
        "a deferred constraint trigger body reads {} other than one column at a time, and its \
         firing is queued as the columns the body reads.",
        row.keyword()
    ))
}
