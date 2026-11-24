//! The triggers a schema holds once a translation unit has run, as the server
//! fires them.

use alloc::borrow::Cow;

use sqlparser::ast::{CreateTrigger, ObjectName, ObjectType, Statement, TriggerPeriod};

use super::Event;
use crate::impls::{
    object_name::last_ident,
    translator_impls::{function_cascade::runs_dropped, function_moves, plpgsql::fold},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// A trigger the schema holds once the translation unit has run, as the
/// server fires it.
#[derive(Clone, Debug)]
pub(crate) struct FiringTrigger {
    /// The trigger's name, folded.
    name: String,
    /// The table or view it fires on, its last name part folded.
    table: String,
    events: Vec<Event>,
    instead_of: bool,
    function: ObjectName,
}

impl FiringTrigger {
    pub(crate) fn of(trigger: &CreateTrigger) -> Option<Self> {
        Some(Self {
            name: fold(last_ident(&trigger.name)?),
            table: fold(last_ident(&trigger.table_name)?),
            events: trigger.events.iter().map(Event::of).collect(),
            instead_of: matches!(trigger.period, Some(TriggerPeriod::InsteadOf)),
            function: trigger.exec_body.as_ref()?.func_desc.name.clone(),
        })
    }

    /// Whether the trigger takes, in place of the view `relation`, folded,
    /// a write of one of `events` to it.
    pub(crate) fn takes(&self, relation: &str, events: &[Event]) -> bool {
        self.instead_of
            && self.table == relation
            && events.iter().any(|event| self.events.contains(event))
    }

    /// Whether `other` is the same trigger, by its name and the table it
    /// fires on.
    fn same(&self, other: &Self) -> bool {
        (self.name.as_str(), self.table.as_str()) == (other.name.as_str(), other.table.as_str())
    }
}

/// The triggers the initial schema and `statements` leave in place, each
/// created one replacing an earlier one of its name on its table, carried to
/// the name its function takes.
pub(super) fn live_triggers<'a>(
    initial: impl Iterator<Item = &'a CreateTrigger>,
    statements: &'a [Statement],
) -> Vec<Cow<'a, CreateTrigger>> {
    let mut triggers: Vec<(Cow<'a, CreateTrigger>, FiringTrigger)> = initial
        .filter_map(|trigger| Some((Cow::Borrowed(trigger), FiringTrigger::of(trigger)?)))
        .collect();
    for statement in statements {
        match statement {
            Statement::CreateTrigger(trigger) => {
                let Some(created) = FiringTrigger::of(trigger) else { continue };
                triggers.retain(|(_, kept)| !kept.same(&created));
                triggers.push((Cow::Borrowed(trigger), created));
            }
            Statement::DropTrigger(drop) => {
                let name = last_ident(&drop.trigger_name).map(fold);
                let table = drop.table_name.as_ref().and_then(last_ident).map(fold);
                triggers.retain(|(_, kept)| {
                    name.as_deref() != Some(kept.name.as_str())
                        || table.as_deref().is_some_and(|table| table != kept.table)
                });
            }
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View, names, ..
            } => {
                let dropped: Vec<String> = names.iter().filter_map(last_ident).map(fold).collect();
                triggers.retain(|(_, kept)| !dropped.contains(&kept.table));
            }
            Statement::DropFunction(_) => {
                triggers.retain(|(_, kept)| !runs_dropped(statement, &kept.function));
            }
            _ if function_moves::moves_functions(statement) => {
                for (trigger, kept) in &mut triggers {
                    if let Some(moved) = function_moves::moved(statement, &kept.function) {
                        function_moves::carry(trigger.to_mut(), statement);
                        kept.function = moved;
                    }
                }
            }
            _ => {}
        }
    }
    triggers.into_iter().map(|(trigger, _)| trigger).collect()
}
