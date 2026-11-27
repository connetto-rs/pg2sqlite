//! The `INSTEAD OF` triggers through which the replica writes an
//! automatically updatable view as its owner, or as the caller for a
//! `security_invoker` view, refusing with `44000` a row a view `WITH CHECK
//! OPTION` would not show.
//!
//! Each refusal PostgreSQL makes before writing is its own trigger, created
//! after the write's since SQLite fires the newest trigger first.

use sql_traits::{
    structs::{ParserDB, View, ViewCheckOption, WrittenRelation},
    traits::{ColumnLike, DatabaseLike, RoleLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    BinaryOperator, ColumnOption, Expr, Ident, ObjectName, Statement, TriggerEvent, TriggerPeriod,
};
use sqlstate::pg;

use super::{
    definer,
    plpgsql::{PlPgSqlTranslator, TriggerFacts},
    view_writes::{output_names, rewritten_base, written_relation},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        derived_writes::Event,
        function_helpers::simple_function_expr,
        privileges::RoleTable,
        query_builder::{from_relation, plain_table_factor, single_expr_query},
        session_variable,
        translator_impls::rls::write_guard_condition_expr,
    },
    options::TranslationContext,
    sqlstate::message_prefix,
};

type Role = <ParserDB as DatabaseLike>::Role;

/// The role a write through the view runs as.
pub(super) enum Runner<'s> {
    /// The caller, for a `security_invoker` view, a view the acting role owns,
    /// and a translation naming no acting role.
    Caller,
    /// The view's owner, a role the schema declares.
    Owner(&'s Role),
    /// A superuser, the owner of a view whose owner the schema never
    /// declares.
    Superuser,
}

/// What a role may do with each output column of a view, or each column of
/// the relation a view writes, by position.
struct Held {
    insert: Vec<bool>,
    update: Vec<bool>,
    delete: bool,
}

impl Held {
    /// Every privilege on `width` columns.
    fn all(width: usize) -> Self {
        Self { insert: vec![true; width], update: vec![true; width], delete: true }
    }

    fn holds(&self, event: Event, position: usize) -> bool {
        let columns = match event {
            Event::Insert => &self.insert,
            Event::Update => &self.update,
            Event::Delete | Event::Truncate => return self.delete,
        };
        columns.get(position).copied().unwrap_or(false)
    }

    fn holds_any(&self, event: Event) -> bool {
        match event {
            Event::Insert => self.insert.contains(&true),
            Event::Update => self.update.contains(&true),
            Event::Delete | Event::Truncate => self.delete,
        }
    }

    fn holds_every(&self, event: Event) -> bool {
        match event {
            Event::Insert => !self.insert.contains(&false),
            Event::Update => !self.update.contains(&false),
            Event::Delete | Event::Truncate => self.delete,
        }
    }
}

/// One output column of the view, as a trigger on it reads it.
struct Output {
    /// The name, as PostgreSQL stores it.
    name: String,
    /// The column of the written relation it passes through, `None` for one
    /// it computes.
    written: Option<Ident>,
}

/// The view as the triggers of one command write through it.
struct Written<'s> {
    view: &'s View,
    /// The view on the replica, or the table holding it there.
    sqlite_name: &'s ObjectName,
    outputs: Vec<Output>,
    base: WrittenRelation<'s, ParserDB>,
    /// The relation written, as a PostgreSQL statement names it.
    relation: ObjectName,
    event: Event,
    /// `INSTEAD OF` on a view, `BEFORE` on the table holding one.
    period: TriggerPeriod,
}

/// The `INSTEAD OF` triggers making the view `sqlite_name` writable on the
/// replica as PostgreSQL makes `view` writable, one set per command
/// PostgreSQL rewrites onto the relation the view reads.
///
/// # Errors
///
/// Refuses a `LOCAL` check option the replica cannot hold apart from the
/// conditions of the views underneath, a view owner a table's policies bind,
/// and a write that cannot be translated as a trigger body, and returns the
/// lookup errors resolving the view runs into.
pub(crate) fn view_triggers(
    view: &View,
    sqlite_name: &ObjectName,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let names = output_names(view, schema)?;
    let Some(names) = names.into_iter().collect::<Option<Vec<String>>>() else {
        return Ok(Vec::new());
    };
    if trigger_reason(view, None, schema, options)?.is_none() {
        return Ok(Vec::new());
    }
    if view.check_option() == Some(ViewCheckOption::Local) {
        refuse_local_over_conditions(view, schema)?;
    }
    let runner = runner(view, schema, options)?;
    let caller = caller_privileges(view, names.len(), schema, options)?;
    let mut triggers = Vec::new();
    for event in [Event::Insert, Event::Update, Event::Delete] {
        let Some(base) = rewritten_base(view, event, schema, options)? else { continue };
        let (relation, columns) = written_relation(&base, schema)?;
        let outputs = names
            .iter()
            .zip(columns.into_iter().chain(core::iter::repeat(None)))
            .map(|(name, written)| Output { name: name.clone(), written })
            .collect();
        let written = Written {
            view,
            sqlite_name,
            outputs,
            base,
            relation,
            event,
            period: TriggerPeriod::InsteadOf,
        };
        triggers.extend(written.triggers(&runner, caller.as_ref(), schema, options, emit)?);
    }
    Ok(triggers)
}

/// The `BEFORE` triggers refusing on `sqlite_name`, the table holding `view`
/// on the replica, the client's writes PostgreSQL refuses through the view,
/// each command's in the reverse of the order PostgreSQL refuses them. The
/// rows the server delivers pass every one. `names` are the view's output
/// columns.
///
/// # Errors
///
/// Refuses a check option whose condition reads a column the view does not
/// pass through or another relation, and returns the lookup errors resolving
/// the view runs into.
pub(super) fn table_guards(
    view: &View,
    sqlite_name: &ObjectName,
    names: &[String],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let runner = runner(view, schema, options)?;
    let caller = caller_privileges(view, names.len(), schema, options)?;
    let delivered = write_guard_condition_expr(None, options);
    let mut triggers = Vec::new();
    for event in [Event::Insert, Event::Update, Event::Delete] {
        let Some(base) = view.updatable_base(schema)?.map(|base| base.relation) else {
            let message = format!(
                "{}cannot {} view \"{}\"",
                message_prefix(pg::OBJECT_NOT_IN_PREREQUISITE_STATE),
                verb(event),
                view.stored_view_name()
            );
            triggers.push(ast_builder::trigger(
                ObjectName::from(vec![quoted(&trigger_name(view, event, "not_updatable"))]),
                sqlite_name.clone(),
                TriggerPeriod::Before,
                command_event(event, Vec::new()),
                true,
                None,
                vec![ast_builder::raise_statement("ABORT", Some(&message), delivered.clone())],
            ));
            continue;
        };
        let (relation, columns) = written_relation(&base, schema)?;
        let outputs = names
            .iter()
            .zip(columns.into_iter().chain(core::iter::repeat(None)))
            .map(|(name, written)| Output { name: name.clone(), written })
            .collect();
        let written = Written {
            view,
            sqlite_name,
            outputs,
            base,
            relation,
            event,
            period: TriggerPeriod::Before,
        };
        if event != Event::Delete
            && let Some(condition) = written.row_condition(schema)?
        {
            triggers.push(written.row_check(condition, delivered.as_ref()));
        }
        triggers.extend(written.refusals(
            &runner,
            caller.as_ref(),
            schema,
            options,
            delivered.as_ref(),
        )?);
    }
    Ok(triggers)
}

/// What only the view's trigger applies to a write of `event` through
/// `view`, of any command when `None`: the check option, the owner's rights
/// or the caller's missing grant of a view on the way, with that view. `None`
/// when the static rewrite does as well and for a view whose writes land past
/// the relations the replica holds as PostgreSQL holds them.
///
/// # Errors
///
/// Returns the lookup errors resolving the views runs into.
pub(super) fn trigger_reason<'s>(
    view: &'s View,
    event: Option<Event>,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<(&'static str, &'s View)>, Error> {
    if super::view_table::writes_past_replica(view, schema, options)? {
        return Ok(None);
    }
    let acting = session_variable::acting_role(schema, options)?;
    let mut written = Some(view);
    while let Some(current) = written {
        if event != Some(Event::Delete) && current.check_option().is_some() {
            return Ok(Some(("the check option", current)));
        }
        if !matches!(runner(current, schema, options)?, Runner::Caller) {
            return Ok(Some(("the owner's rights", current)));
        }
        if let Some(acting) = acting {
            let held =
                view_privileges(current, acting, output_names(current, schema)?.len(), schema)?;
            let lacks = |event: Event| !held.holds_every(event);
            if event.map_or_else(
                || [Event::Insert, Event::Update, Event::Delete].into_iter().any(lacks),
                lacks,
            ) {
                return Ok(Some(("the caller's grant", current)));
            }
        }
        written = match current.updatable_base(schema)?.map(|base| base.relation) {
            Some(WrittenRelation::View { view: inner, .. }) => Some(inner),
            _ => None,
        };
    }
    Ok(None)
}

/// Refuses a view declared `WITH LOCAL CHECK OPTION` over a view that narrows
/// the rows without a check option of its own. PostgreSQL checks only the
/// outer view's condition then, and the replica reads the row back through
/// the outer view, which applies the inner condition as well.
fn refuse_local_over_conditions(view: &View, schema: &ParserDB) -> Result<(), Error> {
    let mut written = view.updatable_base(schema)?.map(|base| base.relation);
    while let Some(WrittenRelation::View { view: inner, .. }) = written {
        if inner.check_option().is_some() {
            return Ok(());
        }
        if has_condition(inner) {
            return Err(Error::forward_refusal(format!(
                "view {} is declared WITH LOCAL CHECK OPTION over view {}, whose condition \
                 PostgreSQL leaves unchecked on a write through {}. The replica checks a row by \
                 reading it back through the view, which applies every condition underneath. \
                 Declare the check option CASCADED, or give {} a check option of its own.",
                view.stored_view_name(),
                inner.stored_view_name(),
                view.stored_view_name(),
                inner.stored_view_name()
            )));
        }
        written = inner.updatable_base(schema)?.map(|base| base.relation);
    }
    Ok(())
}

/// Whether the view's definition narrows the rows it reads.
fn has_condition(view: &View) -> bool {
    let mut body = view.definition().body.as_ref();
    while let sqlparser::ast::SetExpr::Query(inner) = body {
        body = inner.body.as_ref();
    }
    matches!(body, sqlparser::ast::SetExpr::Select(select) if select.selection.is_some())
}

/// The role a write through `view` runs as.
pub(super) fn runner<'s>(
    view: &'s View,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Runner<'s>, Error> {
    if view.is_security_invoker() {
        return Ok(Runner::Caller);
    }
    let acting = session_variable::acting_role(schema, options)?;
    Ok(match view.owner(schema)?.and_then(|name| schema.role(name)) {
        Some(owner) if acting.is_none_or(|acting| acting.name() == owner.name()) => Runner::Caller,
        Some(owner) => Runner::Owner(owner),
        None if acting.is_none() => Runner::Caller,
        None => Runner::Superuser,
    })
}

/// What the acting role holds on the view's output columns, `None` without
/// an acting role, which holds every privilege.
fn caller_privileges(
    view: &View,
    width: usize,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Held>, Error> {
    let Some(acting) = session_variable::acting_role(schema, options)? else { return Ok(None) };
    Ok(Some(view_privileges(view, acting, width, schema)?))
}

/// The roles writes through the schema's automatically updatable views run
/// as in place of the caller, each view's owner, and whether one runs as a
/// superuser.
///
/// # Errors
///
/// Returns the lookup errors resolving a view runs into.
pub(super) fn view_owners<'s>(
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(Vec<&'s Role>, bool), Error> {
    let mut roles = Vec::new();
    let mut superuser = false;
    for view in schema.views() {
        if view.updatable_base(schema)?.is_none() {
            continue;
        }
        match runner(view, schema, options)? {
            Runner::Caller => {}
            Runner::Owner(owner) => roles.push(owner),
            Runner::Superuser => superuser = true,
        }
    }
    Ok((roles, superuser))
}

/// What `role` holds on the output columns of `view`, `width` of them.
fn view_privileges(
    view: &View,
    role: &Role,
    width: usize,
    schema: &ParserDB,
) -> Result<Held, Error> {
    let held = view.privileges(role, schema)?;
    Ok(Held {
        insert: (0..width).map(|position| held.insert.on_column(position)).collect(),
        update: (0..width).map(|position| held.update.on_column(position)).collect(),
        delete: held.delete,
    })
}

impl Written<'_> {
    /// The triggers of the command, the write's first and the refusals after
    /// it in the reverse of the order PostgreSQL makes them.
    fn triggers(
        &self,
        runner: &Runner<'_>,
        caller: Option<&Held>,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let mut triggers = Vec::new();
        if let Some(body) = self.body(runner, schema, options, emit)? {
            triggers.push(self.trigger("", Vec::new(), body));
        }
        triggers.extend(self.refusals(runner, caller, schema, options, None)?);
        Ok(triggers)
    }

    /// The refusals of the command in the reverse of the order PostgreSQL
    /// makes them, those of a column the view computes holding only when
    /// `computed_guard` does.
    fn refusals(
        &self,
        runner: &Runner<'_>,
        caller: Option<&Held>,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        computed_guard: Option<&Expr>,
    ) -> Result<Vec<Statement>, Error> {
        let mut triggers = Vec::new();
        if let Some(held) = self.running_privileges(runner, schema)? {
            triggers.extend(self.refusal(
                "owner_grant",
                |position| {
                    self.outputs[position].written.is_some() && !held.holds(self.event, position)
                },
                !held.holds_any(self.event),
                &relation_denied(&self.base),
                write_guard_condition_expr(None, options).as_ref(),
            ));
        }
        if let Some(held) = caller {
            let outside = definer::unless_running(
                write_guard_condition_expr(None, options),
                &definer::definer_owners(schema, options)?,
            );
            triggers.extend(self.refusal(
                "grant",
                |position| !held.holds(self.event, position),
                !held.holds_any(self.event),
                &format!(
                    "{}permission denied for view {}",
                    message_prefix(pg::INSUFFICIENT_PRIVILEGE),
                    self.view.stored_view_name()
                ),
                outside.as_ref(),
            ));
        }
        if self.event != Event::Delete {
            let verb = verb(self.event);
            if self.outputs.iter().all(|output| output.written.is_none()) {
                let message = format!(
                    "{}cannot {verb} view \"{}\"",
                    message_prefix(pg::OBJECT_NOT_IN_PREREQUISITE_STATE),
                    self.view.stored_view_name()
                );
                triggers.extend(self.refusal(
                    "not_updatable",
                    |_| false,
                    true,
                    &message,
                    computed_guard,
                ));
                return Ok(triggers);
            }
            for (position, output) in self.outputs.iter().enumerate() {
                if output.written.is_some() {
                    continue;
                }
                let message = format!(
                    "{}cannot {verb} column \"{}\" of view \"{}\"",
                    message_prefix(pg::FEATURE_NOT_SUPPORTED),
                    output.name,
                    self.view.stored_view_name()
                );
                triggers.extend(self.refusal(
                    &format!("computed_{position}"),
                    |listed| listed == position,
                    false,
                    &message,
                    computed_guard,
                ));
            }
        }
        Ok(triggers)
    }

    /// The trigger of the command named `suffix` after the view, firing for
    /// an update of `columns` or of any column when empty, running
    /// `statements`.
    fn trigger(&self, suffix: &str, columns: Vec<Ident>, statements: Vec<Statement>) -> Statement {
        ast_builder::trigger(
            ObjectName::from(vec![Ident::with_quote(
                '"',
                trigger_name(self.view, self.event, suffix),
            )]),
            self.sqlite_name.clone(),
            self.period,
            self.trigger_event(columns),
            true,
            None,
            statements,
        )
    }

    fn trigger_event(&self, columns: Vec<Ident>) -> TriggerEvent {
        command_event(self.event, columns)
    }

    /// The condition the check options PostgreSQL applies to a row written
    /// through the view test, over the row's own columns as `NEW` holds them,
    /// `None` when no check option applies. A condition holds under a view's
    /// own check option, and under one `CASCADED` on a view above it.
    ///
    /// # Errors
    ///
    /// Refuses a condition reading a column the view does not pass through
    /// or another relation, since the row holds no value of either, and
    /// returns the lookup errors resolving the views runs into.
    fn row_condition(&self, schema: &ParserDB) -> Result<Option<Expr>, Error> {
        let mut names: Vec<Option<String>> =
            self.outputs.iter().map(|output| Some(output.name.clone())).collect();
        let mut level = self.view;
        let mut cascaded = false;
        let mut conditions = Vec::new();
        loop {
            let option = level.check_option();
            cascaded |= option == Some(ViewCheckOption::Cascaded);
            let Some(base) = level.updatable_base(schema)? else { break };
            if let Some(condition) = base.condition.filter(|_| cascaded || option.is_some()) {
                conditions.push(self.over_row(condition, &base.relation, &names, level, schema)?);
            }
            let WrittenRelation::View { view: inner, columns, .. } = base.relation else { break };
            let width = output_names(inner, schema)?.len();
            names = (0..width)
                .map(|inner_position| {
                    columns
                        .iter()
                        .position(|written| *written == Some(inner_position))
                        .and_then(|position| names.get(position).cloned().flatten())
                })
                .collect();
            level = inner;
        }
        Ok(conditions.into_iter().reduce(|left, right| {
            Expr::BinaryOp {
                left: Box::new(Expr::Nested(Box::new(left))),
                op: BinaryOperator::And,
                right: Box::new(Expr::Nested(Box::new(right))),
            }
        }))
    }

    /// `condition`, `level`'s over `relation`, with each column it reads
    /// replaced by the `NEW` column of `names` its output passes it through
    /// as, `names` naming one per output column of `level`.
    fn over_row(
        &self,
        condition: &Expr,
        relation: &WrittenRelation<'_, ParserDB>,
        names: &[Option<String>],
        level: &View,
        schema: &ParserDB,
    ) -> Result<Expr, Error> {
        if let Some(read) = crate::impls::scoped_relations::relation_occurrences(condition).first()
        {
            return Err(Error::forward_refusal(format!(
                "the check option of view {} reads {read}, which a row written through view {} \
                 does not hold, so the replica holding the view as a table cannot check the row. \
                 Test only the columns the view passes through.",
                level.stored_view_name(),
                self.view.stored_view_name()
            )));
        }
        let passed = |position: usize| names.get(position).cloned().flatten();
        let mut replacements = Vec::new();
        match relation {
            WrittenRelation::Table { columns, condition_columns, .. }
            | WrittenRelation::ForeignTable { columns, condition_columns, .. } => {
                for (index, column) in condition_columns {
                    let name = columns
                        .iter()
                        .position(|written| {
                            written.is_some_and(|written| core::ptr::eq(written, *column))
                        })
                        .and_then(passed)
                        .ok_or_else(|| {
                            hidden_condition_column(level, self.view, column.column_name())
                        })?;
                    replacements.push((*index, new_column("NEW", &name)));
                }
            }
            WrittenRelation::View { view: inner, columns, condition_columns } => {
                let inner_names = output_names(inner, schema)?;
                for (index, inner_position) in condition_columns {
                    let name = columns
                        .iter()
                        .position(|written| *written == Some(*inner_position))
                        .and_then(passed)
                        .ok_or_else(|| {
                            let column = inner_names
                                .get(*inner_position)
                                .cloned()
                                .flatten()
                                .unwrap_or_default();
                            hidden_condition_column(level, self.view, &column)
                        })?;
                    replacements.push((*index, new_column("NEW", &name)));
                }
            }
        }
        let mut rewritten = condition.clone();
        let _: core::ops::ControlFlow<()> = sqlparser::ast::VisitMut::visit(
            &mut rewritten,
            &mut PreOrderReplacements { position: 0, replacements },
        );
        Ok(rewritten)
    }

    /// The trigger refusing with `44000` a row `condition` does not admit,
    /// `delivered` holding.
    fn row_check(&self, condition: Expr, delivered: Option<&Expr>) -> Statement {
        let message = format!(
            "{}new row violates check option for view \"{}\"",
            message_prefix(pg::WITH_CHECK_OPTION_VIOLATION),
            self.view.stored_view_name()
        );
        let violated = Expr::IsNotTrue(Box::new(Expr::Nested(Box::new(condition))));
        self.trigger(
            "check",
            Vec::new(),
            vec![ast_builder::raise_statement(
                "ABORT",
                Some(&message),
                and(delivered.cloned(), Some(violated)),
            )],
        )
    }
}

fn command_event(event: Event, columns: Vec<Ident>) -> TriggerEvent {
    match event {
        Event::Insert => TriggerEvent::Insert,
        Event::Update => TriggerEvent::Update(columns),
        Event::Delete | Event::Truncate => TriggerEvent::Delete,
    }
}

const fn verb(event: Event) -> &'static str {
    match event {
        Event::Insert => "insert into",
        Event::Update => "update",
        Event::Delete | Event::Truncate => "delete from",
    }
}

/// The refusal of a check option of `level`, reached through `view`, whose
/// condition reads `column`, which `view` does not pass through.
fn hidden_condition_column(level: &View, view: &View, column: &str) -> Error {
    Error::forward_refusal(format!(
        "the check option of view {} reads column {column}, which view {} does not pass \
         through, so the replica holding the view as a table cannot check a row written \
         through it. Pass {column} through the view, or test only the columns it passes \
         through.",
        level.stored_view_name(),
        view.stored_view_name()
    ))
}

/// Replaces the expressions at the positions of a pre-order walk, each a
/// column reference, which holds no expression of its own, so the walk's
/// count is the same with the replacement in place.
struct PreOrderReplacements {
    position: usize,
    replacements: Vec<(usize, Expr)>,
}

impl sqlparser::ast::VisitorMut for PreOrderReplacements {
    type Break = ();

    fn pre_visit_expr(&mut self, expression: &mut Expr) -> core::ops::ControlFlow<()> {
        if let Some((_, replacement)) =
            self.replacements.iter().find(|(position, _)| *position == self.position)
        {
            *expression = replacement.clone();
        }
        self.position += 1;
        core::ops::ControlFlow::Continue(())
    }
}

/// The name of the generated trigger of `event` on `view` named `suffix`.
fn trigger_name(view: &View, event: Event, suffix: &str) -> String {
    let event = match event {
        Event::Insert => "insert",
        Event::Update => "update",
        Event::Delete | Event::Truncate => "delete",
    };
    let mut name = format!("{}_view_{event}", view.stored_view_name());
    if !suffix.is_empty() {
        name.push('_');
        name.push_str(suffix);
    }
    name
}

/// Drops the generated triggers an `INSTEAD OF` trigger of the schema created
/// on a view after it takes the writes of, since PostgreSQL then runs that
/// trigger alone.
///
/// # Errors
///
/// Returns the lookup errors resolving the view runs into.
pub(crate) fn superseded(
    trigger: &sqlparser::ast::CreateTrigger,
    schema: &ParserDB,
) -> Result<Vec<Statement>, Error> {
    if trigger.period != Some(TriggerPeriod::InsteadOf) {
        return Ok(Vec::new());
    }
    let Some(view) = super::view_writes::resolve_view(&trigger.table_name, schema)? else {
        return Ok(Vec::new());
    };
    let width = output_names(view, schema)?.len();
    let suffixes = ["", "owner_grant", "grant", "not_updatable"]
        .into_iter()
        .map(String::from)
        .chain((0..width).map(|position| format!("computed_{position}")));
    let mut drops = Vec::new();
    for suffix in suffixes {
        for event in trigger.events.iter().map(Event::of) {
            drops.push(Statement::DropTrigger(sqlparser::ast::DropTrigger {
                if_exists: true,
                trigger_name: ObjectName::from(vec![Ident::with_quote(
                    '"',
                    trigger_name(view, event, &suffix),
                )]),
                table_name: None,
                option: None,
            }));
        }
    }
    Ok(drops)
}

impl Written<'_> {
    /// The trigger refusing with `message` a write reaching an output column
    /// `refused` picks, or every write when `whole`, `guard` holding.
    ///
    /// An update reaches the columns it assigns, which `UPDATE OF` names. An
    /// insert hands the trigger an omitted column as `NULL`, so only a value
    /// tells a supplied column apart. A delete reaches no column.
    fn refusal(
        &self,
        suffix: &str,
        refused: impl Fn(usize) -> bool,
        whole: bool,
        message: &str,
        guard: Option<&Expr>,
    ) -> Option<Statement> {
        let positions: Vec<usize> =
            (0..self.outputs.len()).filter(|position| refused(*position)).collect();
        let raise = |condition: Option<Expr>| {
            vec![ast_builder::raise_statement(
                "ABORT",
                Some(message),
                and(guard.cloned(), condition),
            )]
        };
        if whole {
            return Some(self.trigger(suffix, Vec::new(), raise(None)));
        }
        if positions.is_empty() || self.event == Event::Delete {
            return None;
        }
        Some(match self.event {
            Event::Update => {
                let columns = positions
                    .iter()
                    .map(|position| quoted(&self.outputs[*position].name))
                    .collect();
                self.trigger(suffix, columns, raise(None))
            }
            Event::Insert | Event::Delete | Event::Truncate => {
                let supplied = positions
                    .iter()
                    .map(|position| {
                        Expr::IsNotNull(Box::new(new_column("NEW", &self.outputs[*position].name)))
                    })
                    .reduce(|left, right| {
                        Expr::BinaryOp {
                            left: Box::new(left),
                            op: BinaryOperator::Or,
                            right: Box::new(right),
                        }
                    });
                self.trigger(
                    suffix,
                    Vec::new(),
                    raise(supplied.map(|supplied| Expr::Nested(Box::new(supplied)))),
                )
            }
        })
    }

    /// What the role the write runs as holds on the columns of the written
    /// relation each output column writes, `None` when the relation's own
    /// guards hold the caller to its privileges or every privilege holds.
    fn running_privileges(
        &self,
        runner: &Runner<'_>,
        schema: &ParserDB,
    ) -> Result<Option<Held>, Error> {
        let Runner::Owner(owner) = runner else { return Ok(None) };
        let held = match &self.base {
            WrittenRelation::Table { table, columns, .. }
            | WrittenRelation::ForeignTable { table, columns, .. } => {
                let grants = RoleTable::of(table, Some(owner), schema)?;
                let column = |position: usize| {
                    columns.get(position).copied().flatten().map(ColumnLike::column_name)
                };
                Held {
                    insert: (0..self.outputs.len())
                        .map(|position| column(position).is_some_and(|name| grants.inserts(name)))
                        .collect(),
                    update: (0..self.outputs.len())
                        .map(|position| column(position).is_some_and(|name| grants.updates(name)))
                        .collect(),
                    delete: grants.deletes(),
                }
            }
            WrittenRelation::View { view: inner, columns, .. } => {
                let width = output_names(inner, schema)?.len();
                let inner_held = view_privileges(inner, owner, width, schema)?;
                let reaches = |held: &[bool], position: usize| {
                    columns
                        .get(position)
                        .copied()
                        .flatten()
                        .is_some_and(|inner| held.get(inner).copied().unwrap_or(false))
                };
                Held {
                    insert: (0..self.outputs.len())
                        .map(|position| reaches(&inner_held.insert, position))
                        .collect(),
                    update: (0..self.outputs.len())
                        .map(|position| reaches(&inner_held.update, position))
                        .collect(),
                    delete: inner_held.delete,
                }
            }
        };
        Ok(Some(held))
    }

    /// The statements of the write's trigger, `None` when it writes no
    /// column: the write PostgreSQL makes on the relation, as the role it
    /// runs as, then the check option's read back.
    fn body(
        &self,
        runner: &Runner<'_>,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Option<Vec<Statement>>, Error> {
        let key = self.key(schema)?;
        let held = self
            .running_privileges(runner, schema)?
            .unwrap_or_else(|| Held::all(self.outputs.len()));
        let write = match (self.event, &key) {
            (Event::Insert, _) => self.insert(&held, schema),
            (Event::Update, Some(key)) => self.update(&held, key),
            (Event::Delete | Event::Truncate, Some(key)) => {
                Some(format!("DELETE FROM {} WHERE {}", self.relation, self.identity(key)))
            }
            (Event::Update | Event::Delete | Event::Truncate, None) => {
                let message = format!(
                    "{}cannot {} view \"{}\" because it does not pass every column of the primary \
                     key of {} through, so the replica cannot tell which row to write",
                    message_prefix(pg::FEATURE_NOT_SUPPORTED),
                    if self.event == Event::Update { "update" } else { "delete from" },
                    self.view.stored_view_name(),
                    self.relation
                );
                return Ok(Some(vec![ast_builder::raise_statement("ABORT", Some(&message), None)]));
            }
        };
        let Some(write) = write else { return Ok(None) };
        let mut statements = self.run(&write, runner, schema, options, emit)?;
        if self.view.check_option().is_some() && self.event != Event::Delete {
            statements.push(self.check(key.as_deref()));
        }
        Ok(Some(statements))
    }

    /// `write`, a PL/pgSQL statement, translated as a trigger body on the view
    /// running as `runner`.
    fn run(
        &self,
        write: &str,
        runner: &Runner<'_>,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let returned = if self.event == Event::Delete { "OLD" } else { "NEW" };
        let text = format!("BEGIN {write}; RETURN {returned}; END");
        let block = sqlparser_plpgsql::embedded::embedded_sql(&text).map_err(|error| {
            Error::forward_refusal(format!(
                "the write through view {} does not parse: {error}",
                self.view.stored_view_name()
            ))
        })?;
        let view_name = self.view.stored_view_name().into_owned();
        let events = [self.trigger_event(Vec::new())];
        let facts = TriggerFacts {
            events: events.to_vec(),
            table: Some(view_name.clone()),
            maintenance_refusal: None,
            after: false,
            skip_clears: None,
        };
        let function = format!("{view_name} view write");
        let superuser = definer::key_action_superuser();
        let owner = match runner {
            Runner::Caller => {
                return PlPgSqlTranslator::translate_body(
                    &function,
                    block,
                    facts,
                    None,
                    schema,
                    &options.with_trigger_events(&events),
                    emit,
                );
            }
            Runner::Owner(owner) => *owner,
            Runner::Superuser => &superuser,
        };
        let running_as = options.with_running_as(owner);
        let translated = PlPgSqlTranslator::translate_body(
            &function,
            block,
            facts,
            None,
            schema,
            &running_as.with_trigger_events(&events),
            emit,
        )?;
        let refusal = |table: &sqlparser::ast::CreateTable| {
            format!(
                "view '{view_name}' writes table '{}' as its owner '{}', whom the table's \
                 row-level security policies bind. The translation cannot apply another role's \
                 policies, only the caller's or none. Make the owner the table's owner or a \
                 BYPASSRLS role, or declare the view WITH (security_invoker = true).",
                table.table_name(),
                owner.name()
            )
        };
        definer::run_as(owner, translated, &refusal, schema, options)
    }

    /// The condition picking the row `OLD` stands for by `key`.
    fn identity(&self, key: &[(usize, Ident)]) -> String {
        key.iter()
            .map(|(position, column)| {
                format!(
                    "{column} IS NOT DISTINCT FROM OLD.{}",
                    quoted(&self.outputs[*position].name)
                )
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// The update PostgreSQL makes on the relation, of every column the view
    /// passes through that the writing role may update, `None` for none.
    fn update(&self, held: &Held, key: &[(usize, Ident)]) -> Option<String> {
        let assignments: Vec<String> = self
            .outputs
            .iter()
            .enumerate()
            .filter(|(position, _)| held.holds(Event::Update, *position))
            .filter_map(|(_, output)| {
                output
                    .written
                    .as_ref()
                    .map(|column| format!("{column} = NEW.{}", quoted(&output.name)))
            })
            .collect();
        (!assignments.is_empty()).then(|| {
            format!(
                "UPDATE {} SET {} WHERE {}",
                self.relation,
                assignments.join(", "),
                self.identity(key)
            )
        })
    }

    /// The insert PostgreSQL makes on the relation, of every column the view
    /// passes through that the writing role may insert into, a column the
    /// insert leaves `NULL` taking the relation's default, `None` for none.
    fn insert(&self, held: &Held, schema: &ParserDB) -> Option<String> {
        let mut columns = Vec::new();
        let mut values = Vec::new();
        for (position, output) in self.outputs.iter().enumerate() {
            let Some(column) = &output.written else { continue };
            if !held.holds(Event::Insert, position) {
                continue;
            }
            let value = format!("NEW.{}", quoted(&output.name));
            values.push(match self.default(position, schema) {
                Some(default) => format!("COALESCE({value}, {default})"),
                None => value,
            });
            columns.push(column.to_string());
        }
        (!columns.is_empty()).then(|| {
            format!(
                "INSERT INTO {} ({}) VALUES ({})",
                self.relation,
                columns.join(", "),
                values.join(", ")
            )
        })
    }

    /// The default of the table column the output column at `position`
    /// writes, `None` for a view, a serial, whose rowid alias draws its value,
    /// and a column without one.
    fn default(&self, position: usize, schema: &ParserDB) -> Option<&Expr> {
        let (WrittenRelation::Table { columns, .. }
        | WrittenRelation::ForeignTable { columns, .. }) = &self.base
        else {
            return None;
        };
        let column = columns.get(position).copied().flatten()?;
        if column.is_serial(schema) {
            return None;
        }
        column.attribute().options.iter().find_map(|option| {
            match &option.option {
                ColumnOption::Default(expr) => Some(expr),
                _ => None,
            }
        })
    }

    /// The output column at each position passing a column of the written
    /// relation's key through, with that column: its primary key, or every
    /// column of a table without one. `None` when the view leaves one out.
    fn key(&self, schema: &ParserDB) -> Result<Option<Vec<(usize, Ident)>>, Error> {
        let Some(key) = relation_key(&self.base, schema)? else { return Ok(None) };
        let mut found = Vec::with_capacity(key.len());
        for column in key {
            let Some(position) = self.outputs.iter().position(|output| {
                output.written.as_ref().is_some_and(|written| same_column(written, &column))
            }) else {
                return Ok(None);
            };
            found.push((position, column));
        }
        Ok(Some(found))
    }

    /// Refuses a row the view does not show, read back through the view by
    /// the key it was written with, an omitted single key column by the rowid
    /// the insert drew.
    fn check(&self, key: Option<&[(usize, Ident)]>) -> Statement {
        let message = format!(
            "{}new row violates check option for view \"{}\"",
            message_prefix(pg::WITH_CHECK_OPTION_VIOLATION),
            self.view.stored_view_name()
        );
        let Some(key) = key else {
            let refusal = format!(
                "{}cannot check the new row of view \"{}\" against its check option because the \
                 view does not pass every column of the primary key of {} through",
                message_prefix(pg::FEATURE_NOT_SUPPORTED),
                self.view.stored_view_name(),
                self.relation
            );
            return ast_builder::raise_statement("ABORT", Some(&refusal), None);
        };
        let drawn = self.event == Event::Insert && key.len() == 1;
        let matched = key
            .iter()
            .map(|(position, _)| {
                let name = &self.outputs[*position].name;
                let mut written = new_column("NEW", name);
                if drawn {
                    written = simple_function_expr(
                        "COALESCE",
                        vec![written, simple_function_expr("last_insert_rowid", Vec::new(), None)],
                        None,
                    );
                }
                Expr::IsNotDistinctFrom(Box::new(Expr::Identifier(quoted(name))), Box::new(written))
            })
            .reduce(|left, right| {
                Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOperator::And,
                    right: Box::new(right),
                }
            });
        let shown = Expr::Exists {
            subquery: Box::new(single_expr_query(
                Expr::Value(sqlparser::ast::Value::Number("1".to_owned(), false).into()),
                from_relation(plain_table_factor(self.sqlite_name.clone())),
                matched,
            )),
            negated: true,
        };
        ast_builder::raise_statement("ABORT", Some(&message), Some(shown))
    }
}

/// The key of the relation a view writes, as a statement on that relation
/// names its columns: a table's primary key, or every column of a table
/// without one, and for a view the output columns passing its own relation's
/// key through. `None` when a view leaves a key column out.
fn relation_key(
    base: &WrittenRelation<'_, ParserDB>,
    schema: &ParserDB,
) -> Result<Option<Vec<Ident>>, Error> {
    match base {
        WrittenRelation::Table { table, .. } | WrittenRelation::ForeignTable { table, .. } => {
            let primary: Vec<Ident> =
                table.primary_key_columns(schema)?.map(declared_ident).collect();
            if primary.is_empty() {
                Ok(Some(table.columns(schema)?.map(declared_ident).collect()))
            } else {
                Ok(Some(primary))
            }
        }
        WrittenRelation::View { view: inner, .. } => {
            let Some(inner_base) = inner.updatable_base(schema)?.map(|base| base.relation) else {
                return Ok(None);
            };
            let Some(key) = relation_key(&inner_base, schema)? else { return Ok(None) };
            let (_, columns) = written_relation(&inner_base, schema)?;
            let names = output_names(inner, schema)?;
            let mut passed = Vec::with_capacity(key.len());
            for column in key {
                let Some(position) = columns.iter().position(|written| {
                    written.as_ref().is_some_and(|written| same_column(written, &column))
                }) else {
                    return Ok(None);
                };
                let Some(Some(name)) = names.get(position) else { return Ok(None) };
                passed.push(quoted(name));
            }
            Ok(Some(passed))
        }
    }
}

fn declared_ident(column: &<ParserDB as DatabaseLike>::Column) -> Ident {
    crate::impls::object_name::declared_column_ident(column)
}

/// Whether two names of a relation's column name the same one.
fn same_column(left: &Ident, right: &Ident) -> bool {
    let stored = |ident: &Ident| {
        if ident.quote_style.is_some() {
            ident.value.clone()
        } else {
            ident.value.to_ascii_lowercase()
        }
    };
    stored(left) == stored(right)
}

/// `name`, quoted as PostgreSQL stores it.
fn quoted(name: &str) -> Ident {
    Ident::with_quote('"', name)
}

fn new_column(row: &str, name: &str) -> Expr {
    Expr::CompoundIdentifier(vec![Ident::new(row), quoted(name)])
}

/// `left AND right`, either absent.
fn and(left: Option<Expr>, right: Option<Expr>) -> Option<Expr> {
    match (left, right) {
        (Some(left), Some(right)) => {
            Some(Expr::BinaryOp {
                left: Box::new(Expr::Nested(Box::new(left))),
                op: BinaryOperator::And,
                right: Box::new(right),
            })
        }
        (left, right) => left.or(right),
    }
}

/// The refusal PostgreSQL reports when the view's owner lacks a privilege on
/// the relation `base` the view writes.
fn relation_denied(base: &WrittenRelation<'_, ParserDB>) -> String {
    match base {
        WrittenRelation::Table { table, .. } | WrittenRelation::ForeignTable { table, .. } => {
            pg2sqlite_runtime::write_guard::permission_denied(table.table_name())
        }
        WrittenRelation::View { view, .. } => {
            format!(
                "{}permission denied for view {}",
                message_prefix(pg::INSUFFICIENT_PRIVILEGE),
                view.stored_view_name()
            )
        }
    }
}
