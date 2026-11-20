//! Resolves the columns a statement of the acting role names before SQLite,
//! which reads a double-quoted name no column in scope holds as a string,
//! sees it.

use core::{ops::ControlFlow, ptr};

use sql_traits::{
    errors::LookupError,
    structs::{
        ColumnBinding, ColumnDefinition, ExpressionContext, ExpressionScope, IdentifierCase,
        ParserDB,
    },
    traits::{ColumnLike, DatabaseLike, TableLike},
    utils::identifier_resolution::identifiers_match,
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, ConflictTarget, Expr, FunctionArg, FunctionArguments, Ident,
    Insert, OnConflictAction, OnInsert, Select, Statement, TableAlias, TableFactor, TableObject,
    TableWithJoins, Visit, Visitor, visit_expressions,
};
use sqlstate::{SqlState, pg};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::{last_ident, resolve_translation_table},
        privileges::RoleTable,
        translator_impls::{
            insert::row_width,
            off_replica::{self, OffReplica},
        },
    },
    options::TranslationContext,
    write_guard::permission_denied,
};

type Table = <ParserDB as DatabaseLike>::Table;

/// SQL keywords PostgreSQL reads as functions, which the parser hands over as
/// bare names.
const NAMED_FUNCTIONS: [&str; 6] =
    ["current_catalog", "current_role", "current_schema", "current_user", "session_user", "user"];

/// Refuses a top-level query, `INSERT`, `UPDATE` or `DELETE` naming a column
/// PostgreSQL refuses, answering a first ambiguous name as `Ok(Some(42702))`
/// for the caller to raise only where the translation has no refusal of its
/// own.
///
/// A missing write privilege on a column the replica holds is left to the
/// write guards, which a deployment's write exemption can lift.
///
/// # Errors
///
/// Returns [`Error::Postgres`] for a column PostgreSQL refuses, a translation
/// refusal for a write-only column, and the lookup errors resolution meets.
pub(crate) fn resolve_columns(
    statement: &Statement,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Error>, Error> {
    if !matches!(
        statement,
        Statement::Query(_) | Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)
    ) {
        return Ok(None);
    }
    if let Some((table, write)) = off_replica::row_write(statement)
        && OffReplica::leaves_with(table, &write, schema, options)?
    {
        return Ok(None);
    }

    let context = ExpressionContext::for_statement(
        statement,
        &ExpressionScope::Empty,
        schema,
        IdentifierCase::AsWritten,
    )?;
    let relations = Relations::of(statement);
    let arbiter = Arbiter::of(statement, schema)?;
    let mut read = Vec::new();
    let mut ambiguous = None;
    let mut failure = None;
    let _ = visit_expressions(statement, |expression| {
        let address = ptr::from_ref(expression).addr();
        let resolved = if relations.argument_names.contains(&address) {
            Ok(Reference::Other)
        } else if arbiter.predicate.contains(&address) {
            arbiter.resolve(expression, schema).map(|()| Reference::Other)
        } else {
            resolve(expression, &context, &relations, schema)
        };
        match resolved {
            Ok(Reference::Column(table, column)) => read.push((table, column)),
            Ok(Reference::Ambiguous(name)) => {
                ambiguous.get_or_insert_with(|| {
                    postgres(
                        pg::AMBIGUOUS_COLUMN,
                        format!("column reference \"{name}\" is ambiguous"),
                    )
                });
            }
            Ok(Reference::Other) => {}
            Err(error) => {
                failure = Some(error);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
    if let Some(error) = failure {
        return Err(error);
    }
    if ambiguous.is_some() {
        return Ok(ambiguous);
    }
    let written = write_targets(statement, schema)?;

    let mut grants = Grants { held: Vec::new(), schema, options };
    for (table, column) in read {
        if !grants.of(table)?.reads(column) {
            return Err(denied(table));
        }
    }
    let mut hidden = None;
    for target in &written {
        let held = grants.of(target.table)?;
        if !held.reads(target.column) {
            hidden.get_or_insert(target);
        }
    }
    let Some(hidden) = hidden else { return Ok(None) };
    for target in &written {
        let held = grants.of(target.table)?;
        let writes = match target.write {
            Write::Insert => held.inserts(target.column),
            Write::Update => held.updates(target.column),
        };
        if !writes {
            return Err(denied(target.table));
        }
    }
    Err(Error::forward_refusal(format!(
        "the acting role may {} column {} of {} but not read it, and the replica holds only the \
         columns the role reads, so the value has nowhere to go",
        hidden.write.verb(),
        hidden.column,
        hidden.table.table_name(),
    )))
}

/// What a statement answers given the ambiguity [`resolve_columns`] found and
/// its translation, PostgreSQL's `42702` unless the translation refuses the
/// statement for another reason.
pub(crate) fn with_ambiguity<T>(
    ambiguous: Option<Error>,
    translated: Result<T, Error>,
) -> Result<T, Error> {
    match (ambiguous, translated) {
        (
            Some(ambiguity),
            Ok(_) | Err(Error::LookupError(LookupError::AmbiguousTableLookup { .. })),
        ) => Err(ambiguity),
        (_, translated) => translated,
    }
}

/// What a node of the statement reads.
enum Reference<'c> {
    /// A stored column the acting role's privileges on its own table decide.
    Column(&'c Table, &'c str),
    Ambiguous(String),
    /// Any other node, and a reference reaching its column through a view or
    /// a query.
    Other,
}

/// What `expression` reads.
///
/// # Errors
///
/// Refuses a name no relation in scope holds, as PostgreSQL does.
fn resolve<'c>(
    expression: &Expr,
    context: &'c ExpressionContext<'_, '_, ParserDB>,
    relations: &Relations,
    schema: &ParserDB,
) -> Result<Reference<'c>, Error> {
    let parts = match expression {
        Expr::Identifier(ident) => core::slice::from_ref(ident),
        Expr::CompoundIdentifier(parts) => parts.as_slice(),
        _ => return Ok(Reference::Other),
    };
    match context.reference(expression) {
        Ok(Some((ColumnDefinition::Base { table, column, .. }, binding)))
            if relations.binds_directly(&binding, table, schema)? =>
        {
            Ok(Reference::Column(table, column.column_name()))
        }
        Ok(Some(_)) => Ok(Reference::Other),
        Err(LookupError::AmbiguousTableLookup { .. }) => {
            Ok(Reference::Ambiguous(
                parts.last().map_or_else(String::new, |ident| ident.value.clone()),
            ))
        }
        Ok(None) => unbound(expression, parts, relations).map(|()| Reference::Other),
        Err(error) => Err(error.into()),
    }
}

/// What PostgreSQL answers for the name `parts` of `expression`, which no
/// column in scope holds.
fn unbound(expression: &Expr, parts: &[Ident], relations: &Relations) -> Result<(), Error> {
    match parts {
        [ident]
            if ident.quote_style.is_none()
                && (ident.value.eq_ignore_ascii_case("default")
                    || NAMED_FUNCTIONS
                        .iter()
                        .any(|name| ident.value.eq_ignore_ascii_case(name))) =>
        {
            Ok(())
        }
        // A whole-row reference.
        [ident] if relations.exposes(ident) => Ok(()),
        [ident] => Err(undefined_column(&ident.value)),
        [.., qualifier, _] if relations.hidden.contains(&ptr::from_ref(expression).addr()) => {
            Err(postgres(
                pg::UNDEFINED_TABLE,
                format!("invalid reference to FROM-clause entry for table \"{}\"", qualifier.value),
            ))
        }
        [.., qualifier, _] if !relations.exposes(qualifier) => {
            Err(postgres(
                pg::UNDEFINED_TABLE,
                format!("missing FROM-clause entry for table \"{}\"", qualifier.value),
            ))
        }
        _ => {
            let written: Vec<&str> = parts.iter().map(|ident| ident.value.as_str()).collect();
            Err(postgres(
                pg::UNDEFINED_COLUMN,
                format!("column {} does not exist", written.join(".")),
            ))
        }
    }
}

fn postgres(sqlstate: SqlState, message: String) -> Error {
    Error::Postgres { sqlstate, message }
}

fn undefined_column(name: &str) -> Error {
    postgres(pg::UNDEFINED_COLUMN, format!("column \"{name}\" does not exist"))
}

fn denied(table: &Table) -> Error {
    postgres(pg::INSUFFICIENT_PRIVILEGE, permission_denied(table.table_name()))
}

/// The `WHERE` of an `INSERT`'s `ON CONFLICT` target, which reads the
/// target table's columns.
struct Arbiter<'s> {
    table: Option<&'s Table>,
    /// The address of every node of the predicate.
    predicate: Vec<usize>,
}

impl<'s> Arbiter<'s> {
    fn of(statement: &Statement, schema: &'s ParserDB) -> Result<Self, Error> {
        let mut arbiter = Self { table: None, predicate: Vec::new() };
        if let Statement::Insert(insert) = statement
            && let Some(OnInsert::OnConflict(conflict)) = &insert.on
            && let Some(ConflictTarget::Columns { predicate: Some(predicate), .. }) =
                &conflict.conflict_target
            && let TableObject::TableName(name) = &insert.table
        {
            arbiter.table = resolve_translation_table(schema, name)?;
            let _ = visit_expressions(predicate, |expression| {
                arbiter.predicate.push(ptr::from_ref(expression).addr());
                ControlFlow::<()>::Continue(())
            });
        }
        Ok(arbiter)
    }

    /// Refuses a name of the predicate the target table does not declare.
    fn resolve(&self, expression: &Expr, schema: &ParserDB) -> Result<(), Error> {
        let (Some(table), Some(ident)) = (
            self.table,
            match expression {
                Expr::Identifier(ident) => Some(ident),
                Expr::CompoundIdentifier(parts) => parts.last(),
                _ => None,
            },
        ) else {
            return Ok(());
        };
        if declared(table, ident, schema)?.is_some() {
            Ok(())
        } else {
            Err(undefined_column(&ident.value))
        }
    }
}

/// The column of `table` the name `written` reaches.
fn declared<'s>(
    table: &'s Table,
    written: &Ident,
    schema: &'s ParserDB,
) -> Result<Option<&'s str>, Error> {
    Ok(table
        .columns(schema)?
        .find(|column| {
            identifiers_match(
                column.column_name(),
                column.column_name_is_quoted(),
                &written.value,
                written.quote_style.is_some(),
            )
        })
        .map(ColumnLike::column_name))
}

/// A write the acting role needs a privilege on a column for.
#[derive(Clone, Copy)]
enum Write {
    Insert,
    Update,
}

impl Write {
    const fn verb(self) -> &'static str {
        match self {
            Self::Insert => "insert into",
            Self::Update => "update",
        }
    }
}

/// One column a statement writes.
struct Target<'s> {
    table: &'s Table,
    column: &'s str,
    write: Write,
}

/// The columns `statement` writes, an `INSERT` naming none writing the
/// leading columns its rows fill, and none known behind a wildcard.
///
/// # Errors
///
/// Refuses a target the table does not declare, as PostgreSQL does.
fn write_targets<'s>(
    statement: &Statement,
    schema: &'s ParserDB,
) -> Result<Vec<Target<'s>>, Error> {
    let mut targets = Vec::new();
    match statement {
        Statement::Insert(insert) => {
            let TableObject::TableName(name) = &insert.table else { return Ok(targets) };
            let Some(table) = resolve_translation_table(schema, name)? else { return Ok(targets) };
            if insert.columns.is_empty() {
                let width =
                    insert.source.as_deref().map_or(Some(0), |source| row_width(&source.body));
                for column in table.columns(schema)?.take(width.unwrap_or(0)) {
                    targets.push(Target {
                        table,
                        column: column.column_name(),
                        write: Write::Insert,
                    });
                }
            } else {
                for column in insert.columns.iter().filter_map(last_ident) {
                    targets.push(target(table, column, Write::Insert, schema)?);
                }
            }
            if let Some(OnInsert::OnConflict(conflict)) = &insert.on
                && let OnConflictAction::DoUpdate(update) = &conflict.action
            {
                assigned(&update.assignments, table, schema, &mut targets)?;
            }
        }
        Statement::Update(update) => {
            let TableFactor::Table { name, .. } = &update.table.relation else {
                return Ok(targets);
            };
            let Some(table) = resolve_translation_table(schema, name)? else { return Ok(targets) };
            assigned(&update.assignments, table, schema, &mut targets)?;
        }
        _ => {}
    }
    Ok(targets)
}

/// Appends the columns `assignments` set on `table`.
fn assigned<'s>(
    assignments: &[Assignment],
    table: &'s Table,
    schema: &'s ParserDB,
    targets: &mut Vec<Target<'s>>,
) -> Result<(), Error> {
    for assignment in assignments {
        let names = match &assignment.target {
            AssignmentTarget::ColumnName(name) => core::slice::from_ref(name),
            AssignmentTarget::Tuple(names) => names.as_slice(),
        };
        for column in names.iter().filter_map(last_ident) {
            targets.push(target(table, column, Write::Update, schema)?);
        }
    }
    Ok(())
}

/// The column of `table` the target `written` names.
///
/// # Errors
///
/// Refuses a name `table` does not declare, as PostgreSQL does.
fn target<'s>(
    table: &'s Table,
    written: &Ident,
    write: Write,
    schema: &'s ParserDB,
) -> Result<Target<'s>, Error> {
    let Some(column) = declared(table, written, schema)? else {
        return Err(postgres(
            pg::UNDEFINED_COLUMN,
            format!(
                "column \"{}\" of relation \"{}\" does not exist",
                written.value,
                table.table_name()
            ),
        ));
    };
    Ok(Target { table, column, write })
}

/// The acting role's privileges on each table a statement names, read once.
struct Grants<'s, 'o> {
    held: Vec<(&'s Table, RoleTable)>,
    schema: &'s ParserDB,
    options: &'o TranslationContext<'o>,
}

impl<'s> Grants<'s, '_> {
    fn of(&mut self, table: &'s Table) -> Result<&RoleTable, Error> {
        let found = self.held.iter().position(|(held, _)| ptr::eq(*held, table));
        let index = if let Some(index) = found {
            index
        } else {
            self.held.push((table, RoleTable::acting(table, self.schema, self.options)?));
            self.held.len() - 1
        };
        Ok(&self.held[index].1)
    }
}

/// The `FROM` items of one statement, the names its relations expose, and
/// the names of its named function arguments.
struct Relations {
    factors: Vec<usize>,
    /// Copied, since the visitor lends each node to its callback alone.
    names: Vec<Ident>,
    argument_names: Vec<usize>,
    /// The `FROM` items of each enclosing query level, innermost last, with
    /// how many non-`LATERAL` derived tables of that level are being visited.
    levels: Vec<(Vec<Ident>, usize)>,
    /// Qualified references whose qualifier names a `FROM` item of an
    /// enclosing level that a non-`LATERAL` derived table cannot see.
    hidden: Vec<usize>,
}

impl Relations {
    fn of(statement: &Statement) -> Self {
        let mut relations = Self {
            factors: Vec::new(),
            names: Vec::new(),
            argument_names: Vec::new(),
            levels: Vec::new(),
            hidden: Vec::new(),
        };
        if let Statement::Insert(Insert {
            table: TableObject::TableName(name), table_alias, ..
        }) = statement
        {
            let exposed = table_alias.as_ref().map(|alias| &alias.alias).or(last_ident(name));
            relations.names.extend(exposed.cloned());
        }
        let _ = statement.visit(&mut relations);
        relations
    }

    fn exposes(&self, name: &Ident) -> bool {
        holds(&self.names, name)
    }

    /// Whether `binding` reads `table` itself rather than through a view or a
    /// query, a binding with a relation and no `FROM` item reading the row a
    /// write targets.
    fn binds_directly(
        &self,
        binding: &ColumnBinding<'_>,
        table: &Table,
        schema: &ParserDB,
    ) -> Result<bool, Error> {
        let Some(entry) = binding.entry() else { return Ok(binding.relation().is_some()) };
        let factor = entry.factor();
        if !self.factors.contains(&ptr::from_ref(factor).addr()) {
            return Ok(false);
        }
        let TableFactor::Table { name, .. } = factor else { return Ok(false) };
        Ok(resolve_translation_table(schema, name)?.is_some_and(|named| ptr::eq(named, table)))
    }

    /// Whether `qualifier` names a `FROM` item of the nearest level holding
    /// it that a non-`LATERAL` derived table being visited cannot see.
    fn hides(&self, qualifier: &Ident) -> bool {
        self.levels
            .iter()
            .rev()
            .find_map(|(level, hiding)| holds(level, qualifier).then_some(*hiding > 0))
            == Some(true)
    }
}

/// Whether `names` holds a name `name` reaches.
fn holds(names: &[Ident], name: &Ident) -> bool {
    names.iter().any(|held| {
        identifiers_match(
            &held.value,
            held.quote_style.is_some(),
            &name.value,
            name.quote_style.is_some(),
        )
    })
}

impl Visitor for Relations {
    type Break = ();

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        self.factors.push(ptr::from_ref(factor).addr());
        self.names.extend(exposed_name(factor).cloned());
        if let TableFactor::Derived { lateral: false, .. } = factor
            && let Some((_, hiding)) = self.levels.last_mut()
        {
            *hiding += 1;
        }
        ControlFlow::Continue(())
    }

    fn post_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        if let TableFactor::Derived { lateral: false, .. } = factor
            && let Some((_, hiding)) = self.levels.last_mut()
        {
            *hiding -= 1;
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        let mut names = Vec::new();
        for from in &select.from {
            from_names(from, &mut names);
        }
        self.levels.push((names, 0));
        ControlFlow::Continue(())
    }

    fn post_visit_select(&mut self, _select: &Select) -> ControlFlow<Self::Break> {
        self.levels.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &Expr) -> ControlFlow<Self::Break> {
        if let Expr::CompoundIdentifier(parts) = expression
            && let [.., qualifier, _] = parts.as_slice()
            && self.hides(qualifier)
        {
            self.hidden.push(ptr::from_ref(expression).addr());
        }
        if let Expr::Function(function) = expression
            && let FunctionArguments::List(list) = &function.args
        {
            for argument in &list.args {
                if let FunctionArg::ExprNamed { name, .. } = argument {
                    self.argument_names.push(ptr::from_ref(name).addr());
                }
            }
        }
        ControlFlow::Continue(())
    }
}

/// The name `factor` exposes, its alias or its own name.
fn exposed_name(factor: &TableFactor) -> Option<&Ident> {
    let alias = match factor {
        TableFactor::Table { alias, .. }
        | TableFactor::Derived { alias, .. }
        | TableFactor::Function { alias, .. }
        | TableFactor::UNNEST { alias, .. }
        | TableFactor::TableFunction { alias, .. }
        | TableFactor::NestedJoin { alias, .. } => alias.as_ref(),
        _ => None,
    };
    match (alias, factor) {
        (Some(TableAlias { name, .. }), _) => Some(name),
        (None, TableFactor::Table { name, .. } | TableFactor::Function { name, .. }) => {
            last_ident(name)
        }
        (None, _) => None,
    }
}

/// Appends the names the items of `from` expose.
fn from_names(from: &TableWithJoins, names: &mut Vec<Ident>) {
    for factor in
        core::iter::once(&from.relation).chain(from.joins.iter().map(|join| &join.relation))
    {
        names.extend(exposed_name(factor).cloned());
        if let TableFactor::NestedJoin { table_with_joins, .. } = factor {
            from_names(table_with_joins, names);
        }
    }
}
