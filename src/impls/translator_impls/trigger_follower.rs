//! Re-emits each live trigger whose translation a later change to its table, a
//! relation or function its body reaches, or its own function alters, since
//! PostgreSQL resolves a trigger body where it runs.

use alloc::collections::BTreeSet;

use sql_traits::{structs::ParserDB, traits::DatabaseLike};
use sqlparser::ast::{
    AlterFunction, AlterFunctionOperation, AlterFunctionSignature, AlterTableOperation,
    CreateTrigger, DropTrigger, ObjectName, ObjectType, RenameTableNameKind, Statement,
    TriggerEvent,
};

use super::{
    plpgsql::{ReadBodies, body_reach},
    trigger_slots::{folded, renamed_table},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{errors::Error, impls::object_name::same_identifier, warnings::TranslationWarning};

/// Translates a statement as the batch stands on one side of an event.
pub(crate) type Translate<'t> =
    dyn FnMut(&Statement, &mut dyn FnMut(TranslationWarning)) -> Result<Vec<Statement>, Error> + 't;

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
struct Touched {
    /// The relations it creates, alters, renames or drops, folded.
    relations: BTreeSet<String>,
    /// The functions it creates, replaces, or changes the owner or attributes
    /// of, folded.
    functions: BTreeSet<String>,
}

impl Touched {
    fn of(event: &Statement) -> Option<Self> {
        let folded_names = |names: &[&ObjectName]| names.iter().map(|name| folded(name)).collect();
        let relations = |relations| Some(Self { relations, functions: BTreeSet::new() });
        let functions = |functions| Some(Self { relations: BTreeSet::new(), functions });
        match event {
            Statement::AlterTable(alter) => {
                let mut touched: BTreeSet<String> = folded_names(&[&alter.name]);
                touched.extend(renamed_table(event).map(|(_, to)| folded(to)));
                relations(touched)
            }
            Statement::CreateTable(create) => relations(folded_names(&[&create.name])),
            Statement::CreateView(view) => relations(folded_names(&[&view.name])),
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View, names, ..
            } => relations(names.iter().map(folded).collect()),
            Statement::CreateFunction(function) => functions(folded_names(&[&function.name])),
            Statement::AlterFunction(AlterFunction {
                signature: AlterFunctionSignature::Function(function),
                operation:
                    AlterFunctionOperation::OwnerTo(_) | AlterFunctionOperation::Actions { .. },
            }) => functions(folded_names(&[&function.name])),
            _ => None,
        }
    }

    /// Whether `trigger`'s translation reads what this changes, its body read
    /// from `before` and from `after`, `bodies` holding what each body read
    /// before names.
    fn reaches(
        &self,
        trigger: &CreateTrigger,
        (before, after): (&ParserDB, &ParserDB),
        bodies: &mut ReadBodies,
    ) -> Result<bool, Error> {
        if self.relations.contains(&folded(&trigger.table_name)) {
            return Ok(true);
        }
        let Some(exec_body) = &trigger.exec_body else { return Ok(false) };
        for schema in [before, after] {
            match body_reach(&exec_body.func_desc.name, schema, bodies)? {
                Some(reach)
                    if reach.relations.is_disjoint(&self.relations)
                        && reach.functions.is_disjoint(&self.functions) => {}
                Some(_) | None => return Ok(true),
            }
        }
        Ok(false)
    }
}

/// Carries `trigger` along `event`, which renames its table or a column its
/// `UPDATE OF` list names.
fn carry(trigger: &mut CreateTrigger, event: &Statement) {
    let Statement::AlterTable(alter) = event else { return };
    if folded(&alter.name) != folded(&trigger.table_name) {
        return;
    }
    for operation in &alter.operations {
        match operation {
            AlterTableOperation::RenameTable {
                table_name: RenameTableNameKind::As(to) | RenameTableNameKind::To(to),
            } => trigger.table_name.clone_from(to),
            AlterTableOperation::RenameColumn { old_column_name, new_column_name } => {
                for event in &mut trigger.events {
                    let TriggerEvent::Update(columns) = event else { continue };
                    for column in columns {
                        if same_identifier(column, old_column_name) {
                            column.clone_from(new_column_name);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// The triggers live at the point of the batch being translated.
pub(crate) struct TriggerFollower {
    live: Vec<Followed>,
    bodies: ReadBodies,
}

impl TriggerFollower {
    /// The triggers of `initial`, translated before this batch.
    pub(crate) fn seeded(initial: Option<&ParserDB>) -> Self {
        let live = initial
            .into_iter()
            .flat_map(DatabaseLike::triggers)
            .map(|trigger| Followed { create: trigger.clone(), rendered: None })
            .collect();
        Self { live, bodies: ReadBodies::default() }
    }

    /// Whether no trigger is live.
    pub(crate) fn is_empty(&self) -> bool {
        self.live.is_empty()
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
                let dropped: BTreeSet<String> = names.iter().map(folded).collect();
                self.live
                    .retain(|followed| !dropped.contains(&folded(&followed.create.table_name)));
            }
            _ => {
                for followed in &mut self.live {
                    carry(&mut followed.create, source);
                }
            }
        }
    }

    fn forget(&mut self, name: &ObjectName, table: Option<&ObjectName>) {
        let (name, table) = (folded(name), table.map(folded));
        self.live.retain(|followed| !followed.is(&name, table.as_deref()));
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
        let (name, table) = (folded(name), table.map(folded));
        let Some(followed) =
            self.live.iter_mut().find(|followed| followed.is(&name, table.as_deref()))
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
        before: &ParserDB,
        after: &ParserDB,
        translate_seeded: &mut Translate<'_>,
        translate_after: &mut Translate<'_>,
    ) -> Result<Vec<Reemission>, Error> {
        let Some(touched) = Touched::of(event) else { return Ok(Vec::new()) };
        let dropped: BTreeSet<String> = match event {
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View, names, ..
            } => names.iter().map(folded).collect(),
            _ => BTreeSet::new(),
        };
        let mut reemissions = Vec::new();
        for followed in &mut self.live {
            let table = folded(&followed.create.table_name);
            if dropped.contains(&table)
                || !touched.reaches(&followed.create, (before, after), &mut self.bodies)?
            {
                continue;
            }
            let mut moved = followed.create.clone();
            carry(&mut moved, event);
            let create = Statement::CreateTrigger(moved);
            let mut warnings = Vec::new();
            let rendered = translate_after(&create, &mut |warning| warnings.push(warning))?;
            let last = core::mem::replace(followed.rendering(translate_seeded)?, rendered.clone());
            if rendered == last {
                continue;
            }
            let created = created_triggers(&last);
            reemissions.push(Reemission {
                drop: Statement::DropTrigger(DropTrigger {
                    if_exists: true,
                    trigger_name: followed.create.name.clone(),
                    table_name: Some(followed.create.table_name.clone()),
                    option: None,
                }),
                created,
                create,
                rendered,
                warnings,
            });
        }
        Ok(reemissions)
    }
}

impl Followed {
    /// Whether this is the trigger `name`, folded, on `table` when given.
    fn is(&self, name: &str, table: Option<&str>) -> bool {
        folded(&self.create.name) == name
            && table.is_none_or(|table| table == folded(&self.create.table_name))
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
