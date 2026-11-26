//! Re-emits each live trigger whose translation a later change to its table, a
//! relation or function its body reaches, its own function, or the tables
//! exempting the owner a definer body runs as alters, since PostgreSQL
//! resolves a trigger body where it runs.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use core::ops::ControlFlow;

use sql_traits::{structs::ParserDB, traits::DatabaseLike};
use sqlparser::ast::{
    AlterFunction, AlterFunctionOperation, AlterFunctionSignature, AlterRoleOperation,
    AlterTableOperation, CreateTrigger, DropTrigger, Expr, Ident, ObjectName, ObjectType,
    RenameTableNameKind, RoleOption, Statement, TriggerEvent, visit_expressions_mut,
};

use super::{
    definer::{exempt_somewhere, exempting_tables},
    function_cascade::{drop_of, runs_dropped},
    function_moves,
    hidden_referenced_rows::body_statement_keys::Captures,
    plpgsql::{BodyReach, ReadBodies, body_reach},
    trigger_slots::{TableKey, folded, renamed_owner, renamed_table, table_key},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error, impls::object_name::same_identifier, traits::schema::trigger_function,
    warnings::TranslationWarning,
};

/// Translates a statement as the batch stands on one side of an event.
pub(crate) type Translate<'t> =
    dyn FnMut(&Statement, &mut dyn FnMut(TranslationWarning)) -> Result<Vec<Statement>, Error> + 't;

/// What [`body_reach`] answered for each function a trigger runs, on the
/// schema before an event and on the schema after it, and how the event
/// changes the tables exempting each owner a definer body runs as.
#[derive(Default)]
struct Reached {
    bodies: [BTreeMap<ObjectName, Option<BodyReach>>; 2],
    owners: BTreeMap<String, ExemptionChange>,
}

/// One live trigger.
struct Followed {
    /// The trigger as PostgreSQL holds it, naming its table and the columns
    /// of its `UPDATE OF` list by their current names.
    create: CreateTrigger,
    /// Its last translation before slot merging, `None` until an event
    /// translates a seeded one.
    rendered: Option<Vec<Statement>>,
}

/// A live trigger whose translation an event changes.
pub(crate) struct Reemission {
    /// The `DROP TRIGGER` taking the trigger off before the event.
    pub(crate) drop: Statement,
    /// The SQLite triggers its last translation created.
    pub(crate) created: Vec<ObjectName>,
    /// The trigger as PostgreSQL holds it after the event.
    pub(crate) create: Statement,
    /// Its translation after the event, before slot merging.
    pub(crate) rendered: Vec<Statement>,
    /// What that translation reported.
    pub(crate) warnings: Vec<TranslationWarning>,
}

/// What an event changes that a trigger's translation reads.
#[derive(Default)]
struct Touched {
    /// The relations it creates, alters, renames or drops, folded.
    relations: BTreeSet<String>,
    /// The tables it creates or drops a trigger on, folded, which only the
    /// translation of a trigger whose body writes one, or of a trigger on one
    /// with an `UPDATE OF` list, reads.
    trigger_tables: BTreeSet<String>,
    /// The functions it creates, replaces, or changes the owner or attributes
    /// of, folded.
    functions: BTreeSet<String>,
    /// How it can change which tables' row-level security exempts a role,
    /// which decides whether a `SECURITY DEFINER` body runs as its owner.
    exemptions: Option<Exemptions>,
    /// Whether the captures body statements keep move, which every body
    /// statement's checks read.
    body_keys: bool,
}

/// What an event can change about which tables' row-level security exempts a
/// role.
#[derive(Clone, Copy)]
enum Exemptions {
    /// The row-level security, owner or existence of the tables it names.
    Tables,
    /// A role's attributes or memberships, which reach every table.
    Roles,
}

/// How an event changes the tables exempting one role.
struct ExemptionChange {
    /// Whether some table exempts the role on one side of the event only.
    somewhere: bool,
    /// The tables, folded, exempting the role on one side only, worked out for
    /// a role event alone. A table event changes only the tables it names,
    /// which reach a trigger as relations.
    tables: BTreeSet<String>,
}

impl Exemptions {
    /// How the event changes the tables exempting the role stored as
    /// `owner`, from `before` to `after`.
    fn change(
        self,
        owner: &str,
        (before, after): (&ParserDB, &ParserDB),
    ) -> Result<ExemptionChange, Error> {
        match self {
            Self::Tables => {
                let somewhere = |schema: &ParserDB| {
                    schema.role(owner).map_or(Ok(false), |role| exempt_somewhere(role, schema))
                };
                Ok(ExemptionChange {
                    somewhere: somewhere(before)? != somewhere(after)?,
                    tables: BTreeSet::new(),
                })
            }
            Self::Roles => {
                let tables = |schema: &ParserDB| {
                    schema
                        .role(owner)
                        .map_or_else(|| Ok(BTreeSet::new()), |role| exempting_tables(role, schema))
                };
                let (before, after) = (tables(before)?, tables(after)?);
                Ok(ExemptionChange {
                    somewhere: before.is_empty() != after.is_empty(),
                    tables: before.symmetric_difference(&after).cloned().collect(),
                })
            }
        }
    }
}

impl Touched {
    fn of(event: &Statement) -> Option<Self> {
        let folded_names = |names: &[&ObjectName]| names.iter().map(|name| folded(name)).collect();
        let touched = match event {
            Statement::AlterTable(alter) => {
                let mut relations: BTreeSet<String> = folded_names(&[&alter.name]);
                relations.extend(renamed_table(event).map(|(_, to)| folded(to)));
                let exemptions = alter
                    .operations
                    .iter()
                    .any(|operation| {
                        matches!(
                            operation,
                            AlterTableOperation::EnableRowLevelSecurity
                                | AlterTableOperation::DisableRowLevelSecurity
                                | AlterTableOperation::ForceRowLevelSecurity
                                | AlterTableOperation::NoForceRowLevelSecurity
                                | AlterTableOperation::OwnerTo { .. }
                        )
                    })
                    .then_some(Exemptions::Tables);
                Self { relations, exemptions, ..Self::default() }
            }
            Statement::CreateTable(create) => {
                Self { relations: folded_names(&[&create.name]), ..Self::default() }
            }
            Statement::CreateView(view) => {
                Self { relations: folded_names(&[&view.name]), ..Self::default() }
            }
            Statement::CreateTrigger(trigger) => {
                Self { trigger_tables: folded_names(&[&trigger.table_name]), ..Self::default() }
            }
            Statement::DropTrigger(drop) => {
                Self {
                    trigger_tables: folded_names(&[drop.table_name.as_ref()?]),
                    ..Self::default()
                }
            }
            Statement::Drop {
                object_type: object_type @ (ObjectType::Table | ObjectType::View),
                names,
                ..
            } => {
                Self {
                    relations: names.iter().map(folded).collect(),
                    exemptions: (*object_type == ObjectType::Table).then_some(Exemptions::Tables),
                    ..Self::default()
                }
            }
            Statement::CreateFunction(function) => {
                Self { functions: folded_names(&[&function.name]), ..Self::default() }
            }
            Statement::AlterFunction(AlterFunction {
                signature: AlterFunctionSignature::Function(function),
                operation:
                    AlterFunctionOperation::OwnerTo(_) | AlterFunctionOperation::Actions { .. },
            }) => Self { functions: folded_names(&[&function.name]), ..Self::default() },
            Statement::AlterRole {
                operation: AlterRoleOperation::WithOptions { options }, ..
            } if options.iter().any(|option| {
                matches!(
                    option,
                    RoleOption::BypassRLS(_) | RoleOption::SuperUser(_) | RoleOption::Inherit(_)
                )
            }) =>
            {
                Self { exemptions: Some(Exemptions::Roles), ..Self::default() }
            }
            Statement::GrantRole(_) | Statement::RevokeRole(_) => {
                Self { exemptions: Some(Exemptions::Roles), ..Self::default() }
            }
            // An event so a seeded trigger renders before its function's move
            // is carried.
            _ if function_moves::moves_functions(event) => Self::default(),
            _ => return None,
        };
        Some(touched)
    }

    /// Whether `trigger`'s translation reads what this changes, its body read
    /// from `before` and from `after`, `bodies` holding what each body read
    /// before names and `reached` what each function and owner reached across
    /// this event, or `event` moves the function it runs.
    fn reaches(
        &self,
        event: &Statement,
        trigger: &CreateTrigger,
        (before, after): (&ParserDB, &ParserDB),
        (bodies, reached): (&mut ReadBodies, &mut Reached),
    ) -> Result<bool, Error> {
        if self.body_keys {
            return Ok(true);
        }
        let table = folded(&trigger.table_name);
        let gated = trigger
            .events
            .iter()
            .any(|fired| matches!(fired, TriggerEvent::Update(columns) if !columns.is_empty()));
        if self.relations.contains(&table) || (gated && self.trigger_tables.contains(&table)) {
            return Ok(true);
        }
        let Some(exec_body) = &trigger.exec_body else { return Ok(false) };
        let function = &exec_body.func_desc.name;
        if function_moves::moved(event, function).is_some() {
            return Ok(true);
        }
        let Reached { bodies: side_reaches, owners } = reached;
        for (schema, reached) in [before, after].into_iter().zip(side_reaches.iter_mut()) {
            if !reached.contains_key(function) {
                let declared = trigger_function(schema, function)?;
                let reach = body_reach(function, declared, schema, bodies);
                reached.insert(function.clone(), reach);
            }
            let Some(Some(reach)) = reached.get(function) else { return Ok(true) };
            if !reach.relations.is_disjoint(&self.relations)
                || !reach.written.is_disjoint(&self.trigger_tables)
                || !reach.functions.is_disjoint(&self.functions)
            {
                return Ok(true);
            }
            let Some(exemptions) = self.exemptions else { continue };
            for owner in &reach.definers {
                if !owners.contains_key(owner) {
                    owners.insert(owner.clone(), exemptions.change(owner, (before, after))?);
                }
                if owners.get(owner).is_some_and(|change| {
                    change.somewhere || !change.tables.is_disjoint(&reach.relations)
                }) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// Carries `trigger` along `event`, which renames its table, a column its
/// `UPDATE OF` list or `WHEN` condition names, or moves the function it runs.
fn carry(trigger: &mut CreateTrigger, event: &Statement) {
    function_moves::carry(trigger, event);
    let Statement::AlterTable(alter) = event else { return };
    if table_key(&alter.name) != table_key(&trigger.table_name) {
        return;
    }
    for operation in &alter.operations {
        match operation {
            AlterTableOperation::RenameTable {
                table_name: RenameTableNameKind::As(to) | RenameTableNameKind::To(to),
            } => trigger.table_name = renamed_owner(&alter.name, to),
            AlterTableOperation::RenameColumn { old_column_name, new_column_name } => {
                for event in &mut trigger.events {
                    let TriggerEvent::Update(columns) = event else { continue };
                    for column in columns {
                        if same_identifier(column, old_column_name) {
                            column.clone_from(new_column_name);
                        }
                    }
                }
                let rows = [Ident::new("new"), Ident::new("old")];
                let _ = visit_expressions_mut(&mut trigger.condition, |expr| {
                    if let Expr::CompoundIdentifier(parts) = expr
                        && let [row, column] = parts.as_mut_slice()
                        && rows.iter().any(|name| same_identifier(row, name))
                        && same_identifier(column, old_column_name)
                    {
                        column.clone_from(new_column_name);
                    }
                    ControlFlow::<()>::Continue(())
                });
            }
            _ => {}
        }
    }
}

/// The triggers live at the point of the batch being translated.
pub(crate) struct TriggerFollower {
    live: Vec<Followed>,
    bodies: ReadBodies,
    /// The captures body statements kept when the live triggers were last
    /// translated.
    captures: Option<Rc<Captures>>,
}

impl TriggerFollower {
    /// The triggers of `initial`, translated before this batch.
    pub(crate) fn seeded(initial: Option<&ParserDB>) -> Self {
        let live = initial
            .into_iter()
            .flat_map(DatabaseLike::triggers)
            .map(|trigger| Followed { create: trigger.clone(), rendered: None })
            .collect();
        Self { live, bodies: ReadBodies::default(), captures: None }
    }

    /// Whether the captures body statements keep move from `before`, unless
    /// the live triggers were translated since, to `after`, which they are
    /// then translated with.
    fn captures_move(&mut self, (before, after): (&Rc<Captures>, &Rc<Captures>)) -> bool {
        let held = self.captures.replace(Rc::clone(after));
        let held = held.as_ref().unwrap_or(before);
        !Rc::ptr_eq(held, after) && **held != **after
    }

    /// Whether no trigger is live.
    pub(crate) fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// The `DROP TRIGGER` of each live trigger running a function `statement`
    /// drops with `CASCADE`.
    pub(crate) fn cascaded(&self, statement: &Statement) -> Vec<Statement> {
        self.live
            .iter()
            .filter(|followed| {
                followed
                    .create
                    .exec_body
                    .as_ref()
                    .is_some_and(|body| runs_dropped(statement, &body.func_desc.name))
            })
            .map(|followed| drop_of(&followed.create))
            .collect()
    }

    /// Whether `statement` can change what a live trigger's translation reads.
    pub(crate) fn is_event(statement: &Statement) -> bool {
        Touched::of(statement).is_some()
    }

    /// Follows `source`, whose translation before slot merging is `rendered`.
    pub(crate) fn follow(&mut self, source: &Statement, rendered: &[Statement]) {
        match source {
            Statement::CreateTrigger(create) => {
                self.forget(&create.name, Some(&create.table_name));
                self.live
                    .push(Followed { create: create.clone(), rendered: Some(rendered.to_vec()) });
            }
            Statement::DropTrigger(drop) => {
                self.forget(&drop.trigger_name, drop.table_name.as_ref());
            }
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View, names, ..
            } => {
                let dropped: BTreeSet<TableKey> = names.iter().map(table_key).collect();
                self.live
                    .retain(|followed| !dropped.contains(&table_key(&followed.create.table_name)));
            }
            _ => {
                for followed in &mut self.live {
                    carry(&mut followed.create, source);
                }
            }
        }
    }

    fn forget(&mut self, name: &ObjectName, table: Option<&ObjectName>) {
        let (name, table) = (folded(name), table.map(table_key));
        self.live.retain(|followed| !followed.is(&name, table.as_ref()));
    }

    /// The SQLite triggers the last translation of the live trigger `name`,
    /// on `table` when given, created, a seeded one first translated with
    /// `translate_seeded`.
    ///
    /// # Errors
    ///
    /// Returns the error translating a seeded trigger returns.
    pub(crate) fn created(
        &mut self,
        name: &ObjectName,
        table: Option<&ObjectName>,
        translate_seeded: &mut Translate<'_>,
    ) -> Result<Vec<ObjectName>, Error> {
        let (name, table) = (folded(name), table.map(table_key));
        let Some(followed) =
            self.live.iter_mut().find(|followed| followed.is(&name, table.as_ref()))
        else {
            return Ok(Vec::new());
        };
        Ok(created_triggers(followed.rendering(translate_seeded)?))
    }

    /// The live triggers whose translation changes across `event`, from
    /// `before` to `after`, a seeded trigger compared with what
    /// `translate_seeded` gives on the schema it came from.
    ///
    /// # Errors
    ///
    /// Returns the error a trigger's translation on either side returns.
    pub(crate) fn reemissions(
        &mut self,
        event: &Statement,
        (before, after): (&ParserDB, &ParserDB),
        captures: (&Rc<Captures>, &Rc<Captures>),
        translate_seeded: &mut Translate<'_>,
        translate_after: &mut Translate<'_>,
    ) -> Result<Vec<Reemission>, Error> {
        let Some(mut touched) = Touched::of(event) else { return Ok(Vec::new()) };
        touched.body_keys = self.captures_move(captures);
        let dropped: BTreeSet<TableKey> = match event {
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View, names, ..
            } => names.iter().map(table_key).collect(),
            _ => BTreeSet::new(),
        };
        let mut reemissions = Vec::new();
        let mut reached = Reached::default();
        for followed in &mut self.live {
            let replaced = match event {
                Statement::CreateTrigger(trigger) => {
                    followed.is(&folded(&trigger.name), Some(&table_key(&trigger.table_name)))
                }
                Statement::DropTrigger(drop) => {
                    followed.is(
                        &folded(&drop.trigger_name),
                        drop.table_name.as_ref().map(table_key).as_ref(),
                    )
                }
                _ => false,
            };
            if replaced {
                continue;
            }
            let table = table_key(&followed.create.table_name);
            if dropped.contains(&table)
                || !touched.reaches(
                    event,
                    &followed.create,
                    (before, after),
                    (&mut self.bodies, &mut reached),
                )?
            {
                continue;
            }
            reemissions.extend(followed.reemission(
                Some(event),
                translate_seeded,
                translate_after,
            )?);
        }
        Ok(reemissions)
    }

    /// The live triggers on `tables`, or every live trigger when the
    /// `captures` body statements keep move, whose translation changes as
    /// those tables take a new replica shape, a seeded trigger compared with
    /// what `translate_seeded` gives on the schema it came from.
    ///
    /// # Errors
    ///
    /// Returns the error a trigger's translation on either side returns.
    pub(crate) fn reshaped(
        &mut self,
        tables: &[ObjectName],
        captures: (&Rc<Captures>, &Rc<Captures>),
        translate_seeded: &mut Translate<'_>,
        translate_after: &mut Translate<'_>,
    ) -> Result<Vec<Reemission>, Error> {
        let every = self.captures_move(captures);
        let tables: BTreeSet<TableKey> = tables.iter().map(table_key).collect();
        let mut reemissions = Vec::new();
        for followed in &mut self.live {
            if every || tables.contains(&table_key(&followed.create.table_name)) {
                reemissions.extend(followed.reemission(None, translate_seeded, translate_after)?);
            }
        }
        Ok(reemissions)
    }
}

impl Followed {
    /// This trigger carried along `event` when given and translated after
    /// it, absent when that translation is its last one.
    fn reemission(
        &mut self,
        event: Option<&Statement>,
        translate_seeded: &mut Translate<'_>,
        translate_after: &mut Translate<'_>,
    ) -> Result<Option<Reemission>, Error> {
        let mut moved = self.create.clone();
        if let Some(event) = event {
            carry(&mut moved, event);
        }
        let create = Statement::CreateTrigger(moved);
        let mut warnings = Vec::new();
        let mut rendered = translate_after(&create, &mut |warning| warnings.push(warning))?;
        let previous = self.rendering(translate_seeded)?;
        if let Some(owner) = previous.iter().find_map(|statement| {
            match statement {
                Statement::CreateTrigger(trigger) => Some(trigger),
                _ => None,
            }
        }) {
            for statement in &mut rendered {
                if let Statement::CreateTrigger(trigger) = statement {
                    if trigger.name.0.len() == 1 && owner.name.0.len() > 1 {
                        trigger.name = renamed_owner(&owner.name, &trigger.name);
                    }
                    if trigger.table_name.0.len() == 1 && owner.table_name.0.len() > 1 {
                        trigger.table_name = renamed_owner(&owner.table_name, &trigger.table_name);
                    }
                }
            }
        }
        let last = core::mem::replace(previous, rendered.clone());
        if rendered == last {
            return Ok(None);
        }
        Ok(Some(Reemission {
            drop: Statement::DropTrigger(DropTrigger {
                if_exists: true,
                trigger_name: self.create.name.clone(),
                table_name: Some(self.create.table_name.clone()),
                option: None,
            }),
            created: created_triggers(&last),
            create,
            rendered,
            warnings,
        }))
    }

    /// Whether this is the trigger `name`, folded, on `table` when given.
    fn is(&self, name: &str, table: Option<&TableKey>) -> bool {
        folded(&self.create.name) == name
            && table.is_none_or(|table| *table == table_key(&self.create.table_name))
    }

    /// Its last translation, a seeded one first translated with
    /// `translate_seeded` and recorded.
    fn rendering(
        &mut self,
        translate_seeded: &mut Translate<'_>,
    ) -> Result<&mut Vec<Statement>, Error> {
        match &mut self.rendered {
            Some(rendered) => Ok(rendered),
            unrendered @ None => {
                let seeded = Statement::CreateTrigger(self.create.clone());
                Ok(unrendered.insert(translate_seeded(&seeded, &mut |_| {})?))
            }
        }
    }
}

/// The names of the SQLite triggers `rendered` creates.
fn created_triggers(rendered: &[Statement]) -> Vec<ObjectName> {
    rendered
        .iter()
        .filter_map(|statement| {
            match statement {
                Statement::CreateTrigger(part) => Some(part.name.clone()),
                _ => None,
            }
        })
        .collect()
}
