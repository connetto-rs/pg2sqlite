//! The relation a write lands on. PostgreSQL rewrites a write to an
//! automatically updatable view onto the relation the view reads, each view
//! column onto the column it passes through, unless an `INSTEAD OF` trigger
//! on the view takes the write, and the triggers of the relation written then
//! fire as for a write to it.

use sql_traits::{
    structs::{ColumnScope, IdentifierCase, ParserDB, QueryOutput, UpdatableViewBase, View},
    traits::{ColumnLike, DatabaseLike, TableLike, ViewLike},
};

use super::{DerivedWrite, Fresh, firing::FiringTrigger, rows::Rows};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{errors::Error, options::TranslationContext};

/// The schema writes run against and the triggers it fires once the
/// translation unit has run.
#[derive(Clone, Copy)]
pub(super) struct Landing<'s> {
    pub(super) schema: &'s ParserDB,
    triggers: &'s [FiringTrigger],
}

impl<'s> Landing<'s> {
    pub(super) fn new(schema: &'s ParserDB, options: &'s TranslationContext<'_>) -> Self {
        Self { schema, triggers: options.firing_triggers() }
    }

    /// `write` on the relation it lands on, through every automatically
    /// updatable view it targets that no `INSTEAD OF` trigger takes it for.
    /// It gives values to the columns of that relation the view columns it
    /// sets pass through, and its rows are held to no labels, since the
    /// filters it was read with name the view's columns.
    ///
    /// # Errors
    ///
    /// Returns the lookup errors reading a view's definition runs into.
    pub(super) fn land(self, mut write: DerivedWrite) -> Result<DerivedWrite, Error> {
        while write.statement != "TRUNCATE"
            && let Some(view) =
                self.schema.views().find(|view| view.stored_view_name() == write.relation)
            && !self.triggers.iter().any(|trigger| trigger.takes(&write.relation, write.events()))
            && let Some(base) = view.updatable_base(self.schema)?
        {
            let (relation, columns): (String, Vec<Option<String>>) = match base {
                UpdatableViewBase::Table { table, columns }
                | UpdatableViewBase::ForeignTable { table, columns } => {
                    (
                        table.stored_table_name().into_owned(),
                        columns
                            .iter()
                            .map(|column| {
                                column.map(|column| column.stored_column_name().into_owned())
                            })
                            .collect(),
                    )
                }
                UpdatableViewBase::View { view: inner, columns } => {
                    let names = output_names(inner, self.schema)?;
                    (
                        inner.stored_view_name().into_owned(),
                        columns
                            .iter()
                            .map(|position| names.get((*position)?).cloned().flatten())
                            .collect(),
                    )
                }
            };
            if let Fresh::Columns(fresh) = &mut write.fresh {
                let names = output_names(view, self.schema)?;
                *fresh = fresh
                    .iter()
                    .filter_map(|set| {
                        let position = names.iter().position(|name| name.as_ref() == Some(set))?;
                        columns.get(position).cloned().flatten()
                    })
                    .collect();
            }
            write.rows = Rows::any();
            write.relation = relation;
        }
        Ok(write)
    }

    /// Calls `each` on every write of `writes`, those an `INSTEAD OF` trigger
    /// takes replaced by the writes of that trigger's function, and theirs in
    /// turn. A trigger already followed adds nothing again.
    ///
    /// # Errors
    ///
    /// Fails when a trigger's function cannot be read, and as `each` does.
    pub(super) fn taken(
        self,
        writes: &[DerivedWrite],
        each: &mut impl FnMut(&DerivedWrite) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.follow(writes, &mut Vec::new(), each)
    }

    /// [`Self::taken`], `followed` holding the triggers already followed.
    fn follow(
        self,
        writes: &[DerivedWrite],
        followed: &mut Vec<&'s FiringTrigger>,
        each: &mut impl FnMut(&DerivedWrite) -> Result<(), Error>,
    ) -> Result<(), Error> {
        for write in writes {
            let taker =
                self.triggers.iter().find(|trigger| trigger.takes(&write.relation, write.events()));
            match taker {
                None => each(write)?,
                Some(trigger) if !followed.iter().any(|seen| seen.same(trigger)) => {
                    followed.push(trigger);
                    self.follow(&trigger.writes(self)?, followed, each)?;
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

/// The name of each output column of `view` in order, as PostgreSQL stores
/// it: the one its declaration gives, or else the one its definition
/// produces. None follow a wildcard whose columns cannot be enumerated, since
/// their positions are unknown.
///
/// # Errors
///
/// Returns the lookup errors reading the definition runs into.
pub(crate) fn output_names(view: &View, schema: &ParserDB) -> Result<Vec<Option<String>>, Error> {
    let definition = view.definition();
    let graph = ColumnScope::from_query(definition, schema, IdentifierCase::AsWritten)?;
    let mut declared = view.declared_column_names().iter();
    let outputs = graph.scope_for_query(definition).into_iter().flat_map(|scope| scope.outputs());
    Ok(outputs
        .map_while(|output| {
            match output {
                QueryOutput::Column { name, .. } => Some(name),
                QueryOutput::Unexpanded => None,
            }
        })
        .map(|produced| {
            declared.next().map(|(name, quoted)| (name.as_str(), *quoted)).or(produced).map(
                |(name, quoted)| if quoted { name.to_owned() } else { name.to_ascii_lowercase() },
            )
        })
        .collect())
}
