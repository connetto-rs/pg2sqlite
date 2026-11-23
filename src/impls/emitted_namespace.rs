//! Collision checks for the emitted `main` and `temp` database namespaces.
//!
//! Tables, views and indexes share a namespace within each database, while
//! triggers have their own. Dropping a relation releases its dependent indexes
//! and triggers.

use alloc::collections::BTreeMap;

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, IndexLike, TableLike, TriggerLike, ViewLike},
};
use sqlparser::ast::{AlterTableOperation, ObjectName, ObjectType, RenameTableNameKind, Statement};

use super::{
    object_name::last_ident_value_or_display,
    translator_impls::create_trigger::{TriggerRelation, trigger_relation},
};
use crate::errors::Error;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// Where an emitted statement came from, for the message a collision raises.
#[derive(Clone)]
pub(crate) enum Source<'a> {
    /// The input statement it was translated from.
    Input(&'a Statement),
    /// Something the translation adds on its own.
    Generated(&'a str),
    /// An object already represented by the supplied starting schema.
    Initial(String),
}

impl Source<'_> {
    /// Names the source the way the author would recognise it, which is the
    /// object it declared rather than the statement's whole text.
    fn describe(&self) -> String {
        let statement = match self {
            Self::Generated(label) => return (*label).to_string(),
            Self::Initial(label) => return label.clone(),
            Self::Input(statement) => statement,
        };
        match statement {
            Statement::CreateTable(create) => create.name.to_string(),
            Statement::CreateView(view) => view.name.to_string(),
            Statement::CreateVirtualTable { name, .. } => name.to_string(),
            Statement::CreateIndex(index) => {
                index.name.as_ref().map_or_else(
                    || format!("an index on {}", index.table_name),
                    ObjectName::to_string,
                )
            }
            Statement::CreateTrigger(trigger) => {
                format!("{} on {}", trigger.name, trigger.table_name)
            }
            Statement::AlterTable(alter) => alter.name.to_string(),
            other => other.to_string(),
        }
    }
}

/// Refuses the first emitted definition that lands on a name the script already
/// holds.
pub(crate) fn reject_name_collisions<'a>(
    initial_schema: Option<&ParserDB>,
    emitted: impl IntoIterator<Item = (Source<'a>, &'a Statement)>,
) -> Result<(), Error> {
    let mut namespaces = Databases::default();
    if let Some(schema) = initial_schema {
        namespaces.seed(schema)?;
    }
    for (source, statement) in emitted {
        namespaces.apply(statement, source)?;
    }
    Ok(())
}

#[derive(Default)]
struct Databases<'a> {
    main: Namespaces<'a>,
    temporary: Namespaces<'a>,
}

impl<'a> Databases<'a> {
    fn seed(&mut self, schema: &ParserDB) -> Result<(), Error> {
        for table in schema.tables() {
            let name = table.stored_table_name();
            self.namespace(table.is_temporary()).define_object_key(
                sqlite_key(&name),
                None,
                Source::Initial(initial_description(
                    "table",
                    table.table_schema(),
                    table.table_name(),
                )),
            )?;
        }
        for view in schema.views() {
            let name = view.stored_view_name();
            self.namespace(view.is_temporary()).define_object_key(
                sqlite_key(&name),
                None,
                Source::Initial(initial_description("view", view.view_schema(), view.view_name())),
            )?;
        }
        for view in schema.materialized_views() {
            let name = view.stored_view_name();
            self.namespace(view.is_temporary()).define_object_key(
                sqlite_key(&name),
                None,
                Source::Initial(initial_description(
                    "materialized view",
                    view.view_schema(),
                    view.view_name(),
                )),
            )?;
        }
        for index in schema.indexes() {
            let Some(name) = index.name() else { continue };
            let owner = sqlite_key(&index.table().stored_table_name());
            self.namespace(index.table().is_temporary()).define_object_key(
                sqlite_key(name),
                Some(owner),
                Source::Initial(initial_description("index", index.schema(), name)),
            )?;
        }
        let slots = super::translator_impls::trigger_slots::TriggerSlots::seeded(Some(schema))?;
        for trigger in schema.triggers() {
            let relation = trigger_relation(trigger, schema)?;
            let relation_is_temporary = match relation {
                TriggerRelation::Table(table) => table.is_temporary(),
                TriggerRelation::View(view) => view.is_temporary(),
            };
            let namespace = self.namespace(relation_is_temporary);
            let owner = sqlite_key(&relation.stored_name());
            let description = initial_description("trigger", None, trigger.name());
            for name in slots.frame_names_of(trigger) {
                let name_key = key(name);
                if !namespace.triggers.contains_key(&name_key) {
                    namespace.define_trigger_key(
                        name_key,
                        owner.clone(),
                        Source::Initial(description.clone()),
                    )?;
                }
            }
            // A plain row trigger lives under the names its slots were emitted
            // with.
            if slots.holds(trigger) {
                for name in slots.names_of(trigger) {
                    let name_key = key(name);
                    if !namespace.triggers.contains_key(&name_key) {
                        namespace.define_trigger_key(
                            name_key,
                            owner.clone(),
                            Source::Initial(description.clone()),
                        )?;
                    }
                }
                continue;
            }
            namespace.define_trigger_key(
                sqlite_key(trigger.name()),
                owner,
                Source::Initial(description),
            )?;
        }
        Ok(())
    }

    fn holds_temporary(&self, name: &ObjectName, trigger: bool) -> bool {
        if name.0.len() > 1 {
            return name.0[0]
                .as_ident()
                .is_some_and(|identifier| identifier.value.eq_ignore_ascii_case("temp"));
        }
        if trigger {
            self.temporary.triggers.contains_key(&key(name))
        } else {
            self.temporary.objects.contains_key(&key(name))
        }
    }

    fn namespace(&mut self, temporary: bool) -> &mut Namespaces<'a> {
        if temporary { &mut self.temporary } else { &mut self.main }
    }

    fn apply(&mut self, statement: &'a Statement, source: Source<'a>) -> Result<(), Error> {
        let temporary = match statement {
            Statement::CreateTable(create) => create.temporary,
            Statement::CreateView(view) => view.temporary,
            Statement::CreateIndex(index) => {
                index.name.as_ref().filter(|name| name.0.len() > 1).map_or_else(
                    || self.holds_temporary(&index.table_name, false),
                    |name| self.holds_temporary(name, false),
                )
            }
            Statement::CreateTrigger(trigger) => self.holds_temporary(&trigger.table_name, false),
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View | ObjectType::Index,
                names,
                ..
            } => {
                for name in names {
                    let temporary = self.holds_temporary(name, false);
                    self.namespace(temporary).drop_object(name);
                }
                return Ok(());
            }
            Statement::DropTrigger(drop) => self.holds_temporary(&drop.trigger_name, true),
            Statement::AlterTable(alter) => self.holds_temporary(&alter.name, false),
            Statement::CreateVirtualTable { name, .. } => {
                name.0.len() > 1 && self.holds_temporary(name, false)
            }
            _ => false,
        };
        self.namespace(temporary).apply(statement, source)
    }
}

/// One live name, and what would take it away.
struct Entry<'a> {
    source: Source<'a>,
    /// The table or view this hangs off, when dropping that drops this too.
    owner: Option<String>,
}

#[derive(Default)]
struct Namespaces<'a> {
    /// Tables, views and indexes, which SQLite keeps together.
    objects: BTreeMap<String, Entry<'a>>,
    /// Triggers, which SQLite keeps apart but names database-wide.
    triggers: BTreeMap<String, Entry<'a>>,
}

impl<'a> Namespaces<'a> {
    fn apply(&mut self, statement: &'a Statement, source: Source<'a>) -> Result<(), Error> {
        match statement {
            Statement::CreateTable(create) => self.define_object(&create.name, None, source),
            Statement::CreateView(view) => self.define_object(&view.name, None, source),
            Statement::CreateVirtualTable { name, .. } => self.define_object(name, None, source),
            Statement::CreateIndex(index) => {
                match &index.name {
                    Some(name) => self.define_object(name, Some(&index.table_name), source),
                    None => Ok(()),
                }
            }
            Statement::CreateTrigger(trigger) => {
                self.define_trigger(&trigger.name, &trigger.table_name, source)
            }
            Statement::DropTrigger(drop) => {
                self.triggers.remove(&key(&drop.trigger_name));
                Ok(())
            }
            Statement::AlterTable(alter) => {
                for operation in &alter.operations {
                    if let AlterTableOperation::RenameTable { table_name } = operation {
                        let (RenameTableNameKind::As(to) | RenameTableNameKind::To(to)) =
                            table_name;
                        self.rename_object(&alter.name, to, &source)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn define_object(
        &mut self,
        name: &ObjectName,
        owner: Option<&ObjectName>,
        source: Source<'a>,
    ) -> Result<(), Error> {
        self.define_object_key(key(name), owner.map(self::key), source)
    }

    fn define_object_key(
        &mut self,
        key: String,
        owner: Option<String>,
        source: Source<'a>,
    ) -> Result<(), Error> {
        if let Some(held) = self.objects.get(&key) {
            return Err(object_collision(&key, &held.source, &source));
        }
        self.objects.insert(key, Entry { source, owner });
        Ok(())
    }

    fn define_trigger(
        &mut self,
        name: &ObjectName,
        owner: &ObjectName,
        source: Source<'a>,
    ) -> Result<(), Error> {
        self.define_trigger_key(key(name), self::key(owner), source)
    }

    fn define_trigger_key(
        &mut self,
        key: String,
        owner: String,
        source: Source<'a>,
    ) -> Result<(), Error> {
        if let Some(held) = self.triggers.get(&key) {
            return Err(trigger_collision(&key, &held.source, &source));
        }
        self.triggers.insert(key, Entry { source, owner: Some(owner) });
        Ok(())
    }

    /// Drops `name` and everything SQLite would drop with it.
    fn drop_object(&mut self, name: &ObjectName) {
        let key = key(name);
        self.objects.remove(&key);
        self.objects.retain(|_, entry| entry.owner.as_ref() != Some(&key));
        self.triggers.retain(|_, entry| entry.owner.as_ref() != Some(&key));
    }

    fn rename_object(
        &mut self,
        from: &ObjectName,
        to: &ObjectName,
        source: &Source<'a>,
    ) -> Result<(), Error> {
        let (from, to) = (key(from), key(to));
        if let Some(held) = self.objects.get(&to) {
            return Err(object_collision(&to, &held.source, source));
        }
        if let Some(entry) = self.objects.remove(&from) {
            self.objects.insert(to.clone(), entry);
        }
        for entry in self.objects.values_mut().chain(self.triggers.values_mut()) {
            if entry.owner.as_ref() == Some(&from) {
                entry.owner = Some(to.clone());
            }
        }
        Ok(())
    }
}

/// The name SQLite would file an object under, which folds ASCII case.
pub(crate) fn key(name: &ObjectName) -> String {
    last_ident_value_or_display(name).to_ascii_lowercase()
}

fn sqlite_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn initial_description(kind: &str, schema: Option<&str>, name: &str) -> String {
    schema.map_or_else(
        || format!("the initial {kind} {name}"),
        |schema| format!("the initial {kind} {schema}.{name}"),
    )
}

fn object_collision(name: &str, first: &Source<'_>, second: &Source<'_>) -> Error {
    Error::EmittedNameCollision {
        kind: "objects".to_string(),
        name: name.to_string(),
        first: first.describe(),
        second: second.describe(),
        reason: "SQLite holds tables, views and indexes in one namespace where PostgreSQL holds \
                 one per schema"
            .to_string(),
    }
}

fn trigger_collision(name: &str, first: &Source<'_>, second: &Source<'_>) -> Error {
    Error::EmittedNameCollision {
        kind: "triggers".to_string(),
        name: name.to_string(),
        first: first.describe(),
        second: second.describe(),
        reason: "SQLite names a trigger across the whole database where PostgreSQL names it \
                 within its table"
            .to_string(),
    }
}

/// Pairs each emitted statement with the input statement it came from.
///
/// A `CREATE TABLE` input can produce extra statements beyond the translated
/// table itself when RLS or a virtual extension is active. These extra
/// statements (views, triggers, virtual tables) are generated by the translator
/// and carry no direct author: they get `Source::Generated` with a label that
/// names the artifact kind rather than a fixed string.
pub(crate) fn sourced<'a>(
    inputs: &'a [Statement],
    translated: &'a [Vec<Statement>],
) -> impl Iterator<Item = (Source<'a>, &'a Statement)> {
    inputs.iter().zip(translated).flat_map(|(input, emitted)| {
        let is_create_table = matches!(input, Statement::CreateTable(_));
        emitted.iter().enumerate().map(move |(i, s)| {
            // The first statement is always the direct translation of the
            // input. Subsequent statements from a CREATE TABLE are
            // generated by the translator; label them by their
            // actual statement kind.
            let source = if is_create_table && i > 0 {
                Source::Generated(generated_artifact_label(s))
            } else {
                Source::Input(input)
            };
            (source, s)
        })
    })
}

/// A static label describing the kind of generated artifact, used in collision
/// error messages so the caller does not hunt for a statement they never wrote.
fn generated_artifact_label(statement: &Statement) -> &'static str {
    match statement {
        Statement::CreateVirtualTable { module_name, .. }
            if module_name.value.eq_ignore_ascii_case("vec0") =>
        {
            "a generated vec0 virtual table"
        }
        Statement::CreateVirtualTable { .. } => "a generated virtual table",
        Statement::CreateView(_) => "a generated RLS view",
        Statement::CreateTrigger(_) => "a generated trigger",
        Statement::CreateTable(create)
            if super::translator_impls::reentry::is_marker_table(&create.name) =>
        {
            "a generated marker table"
        }
        Statement::CreateTable(_) => "a generated backing table",
        Statement::CreateIndex(_) => "a generated NULLS NOT DISTINCT index",
        _ => "a generated statement",
    }
}
