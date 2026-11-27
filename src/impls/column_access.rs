//! Resolves the columns a statement of the acting role, or a statement or
//! expression of a PL/pgSQL body, names before SQLite, which reads a
//! double-quoted name no column in scope holds as a string, sees it.

use core::{ops::ControlFlow, ptr};

use pg2sqlite_runtime::write_guard::permission_denied;
use sql_traits::{
    errors::LookupError,
    structs::{
        ColumnBinding, ColumnDefinition, ExpressionContext, ExpressionScope, IdentifierCase,
        ParserDB, TargetName,
    },
    traits::{ColumnLike, DatabaseLike, TableLike},
    utils::identifier_resolution::{identifiers_match, stored_ident_name},
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, ConflictTarget, Expr, FromTable, FunctionArg, FunctionArguments,
    Ident, Insert, ObjectName, ObjectNamePart, OnConflictAction, OnInsert, Select, Statement,
    TableAlias, TableFactor, TableObject, TableWithJoins, Visit, Visitor, visit_expressions,
};
use sqlparser_plpgsql::{
    embedded,
    namespace::{NameKind, Namespace, Sql, TriggerVariable},
};
use sqlstate::{SqlState, pg};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        object_name::{last_ident, resolve_translation_table},
        privileges::RoleTable,
        scoped_relations::relation_occurrences,
        translator_impls::{
            insert::row_width,
            off_replica::{self, OffReplica},
            one_shot,
            rls::write_guard_condition_expr,
            view_writes::unheld_view_privilege,
        },
    },
    options::TranslationContext,
    sqlstate::message_prefix,
};

pub(crate) type Table = <ParserDB as DatabaseLike>::Table;

/// SQL keywords PostgreSQL reads as functions, which the parser hands over as
/// bare names.
const NAMED_FUNCTIONS: [&str; 6] =
    ["current_catalog", "current_role", "current_schema", "current_user", "session_user", "user"];

/// What [`resolve_columns`] leaves to a statement's translation.
#[derive(Default)]
pub(crate) struct Resolved {
    /// The first ambiguous name, PostgreSQL's `42702`, raised only where the
    /// translation has no refusal of its own, as [`with_ambiguity`] does.
    ambiguous: Option<Error>,
    /// PostgreSQL's `42501` message for a write the acting role lacks a
    /// privilege for, raised only where the translation has no refusal of its
    /// own, since PostgreSQL checks privileges after parsing and rewriting.
    unheld: Option<String>,
}

/// The temporary view whose insert raises a guarded write's refusal.
const REFUSAL_VIEW: &str = "pg2sqlite_refusal";

/// The temporary trigger raising a guarded write's refusal.
const REFUSAL_TRIGGER: &str = "pg2sqlite_refuse";

impl Resolved {
    /// `translated`, the statement's translation, refused as the statement
    /// is.
    ///
    /// A write the acting role lacks a privilege for is refused before any
    /// row, as PostgreSQL refuses it, since the replica's write guards fire
    /// only for a row the statement reaches. Without an RLS exemption
    /// function nothing lifts the refusal, so the translation refuses the
    /// write. With one, the write runs after a guard raising the refusal
    /// unless the exemption holds, as it holds for a schema's seed rows.
    ///
    /// # Errors
    ///
    /// Returns the translation's error, then the ambiguity and the refusal
    /// of a write the acting role lacks a privilege for.
    pub(crate) fn translated(
        self,
        translated: Result<Vec<Statement>, Error>,
        options: &TranslationContext<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let mut translated = with_ambiguity(self.ambiguous, translated)?;
        let Some(message) = self.unheld else { return Ok(translated) };
        let Some(exemption) = write_guard_condition_expr(None, options) else {
            return Err(postgres(pg::INSUFFICIENT_PRIVILEGE, message));
        };
        let raise = ast_builder::raise_statement(
            "ABORT",
            Some(&format!("{}{message}", message_prefix(pg::INSUFFICIENT_PRIVILEGE))),
            Some(exemption),
        );
        let mut guarded = one_shot::run_once(REFUSAL_VIEW, REFUSAL_TRIGGER, vec![raise]);
        guarded.append(&mut translated);
        Ok(guarded)
    }
}

/// Refuses a top-level query, `INSERT`, `UPDATE` or `DELETE` naming a column
/// PostgreSQL refuses, and leaves to [`Resolved::translated`] a first
/// ambiguous name and a write the acting role lacks a privilege for.
///
/// # Errors
///
/// Returns [`Error::Postgres`] for a column PostgreSQL refuses, a translation
/// refusal for a write-only column, and the lookup errors resolution meets.
pub(crate) fn resolve_columns(
    statement: &Statement,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Resolved, Error> {
    if !matches!(
        statement,
        Statement::Query(_) | Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)
    ) {
        return Ok(Resolved::default());
    }
    if let Some((table, write)) = off_replica::row_write(statement)
        && OffReplica::leaves_with(table, &write, schema, options)?
    {
        return Ok(Resolved::default());
    }

    let context = ExpressionContext::for_statement(
        statement,
        &ExpressionScope::Empty,
        schema,
        IdentifierCase::AsWritten,
    )?;
    let relations = Relations::of(statement);
    let arbiter = Arbiter::of(statement, schema)?;
    let mut written = insert_targets(statement, schema)?;
    let Reads { columns: read, ambiguous } =
        read_names(statement, &context, &relations, &arbiter, schema, None)?;
    if ambiguous.is_some() {
        return Ok(Resolved { ambiguous, unheld: None });
    }
    written.extend(set_targets(statement, schema)?);

    let mut grants = Grants { held: Vec::new(), schema, options };
    for (table, column) in read {
        if !grants.of(table)?.reads(column) {
            return Err(denied(table));
        }
    }
    let unheld = unheld_write(statement, &written, &mut grants)?;
    for target in &written {
        if grants.of(target.table)?.reads(target.column) {
            continue;
        }
        return Err(match unheld {
            Some(message) => postgres(pg::INSUFFICIENT_PRIVILEGE, message),
            None => {
                Error::forward_refusal(format!(
                    "the acting role may {} column {} of {} but not read it, and the replica \
                     holds only the columns the role reads, so the value has nowhere to go",
                    target.write.verb(),
                    target.column,
                    target.table.table_name(),
                ))
            }
        });
    }
    Ok(Resolved { ambiguous: None, unheld })
}

/// PostgreSQL's `42501` message refusing `statement`, whose written columns
/// are `written`, for a privilege the acting role lacks on the table or view
/// it names, a column it fills or, through a view, on the relation the view
/// writes.
///
/// # Errors
///
/// Returns the lookup errors resolving the relations meets.
fn unheld_write<'s>(
    statement: &Statement,
    written: &[Target<'s>],
    grants: &mut Grants<'s, '_>,
) -> Result<Option<String>, Error> {
    for target in written {
        if !target.write.held(grants.of(target.table)?, target.column) {
            return Ok(Some(permission_denied(target.table.table_name())));
        }
    }
    if let Some(table) = unheld_command(statement, written, grants)? {
        return Ok(Some(permission_denied(table.table_name())));
    }
    unheld_view_privilege(statement, grants.schema, grants.options)
}

/// The table `statement` deletes from, or inserts into without naming a
/// column it fills, when the acting role holds no `DELETE` on it, or no
/// `INSERT` on any column for such an insert.
///
/// # Errors
///
/// Returns the lookup errors resolving the table meets.
fn unheld_command<'s>(
    statement: &Statement,
    written: &[Target<'s>],
    grants: &mut Grants<'s, '_>,
) -> Result<Option<&'s Table>, Error> {
    let (name, holds): (_, fn(&RoleTable) -> bool) = match statement {
        Statement::Delete(delete) => {
            let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) =
                &delete.from;
            let [TableWithJoins { relation: TableFactor::Table { name, .. }, .. }] =
                targets.as_slice()
            else {
                return Ok(None);
            };
            (name, RoleTable::deletes)
        }
        Statement::Insert(Insert { table: TableObject::TableName(name), .. })
            if !written.iter().any(|target| matches!(target.write, Write::Insert)) =>
        {
            (name, RoleTable::inserts_any)
        }
        _ => return Ok(None),
    };
    let Some(table) = resolve_translation_table(grants.schema, name)? else { return Ok(None) };
    Ok((!holds(grants.of(table)?)).then_some(table))
}

/// The SQLSTATE and message PostgreSQL raises where a PL/pgSQL body runs
/// `sql`, for a relation it names that `schema` lacks, a name no relation in
/// scope holds and no variable reaches, a field of `NEW` or `OLD` the
/// trigger's table lacks, or a column an `INSERT` or `UPDATE` writes that
/// its table does not declare.
///
/// `None` when every name resolves, for SQL other than an expression, a
/// query, an `INSERT`, an `UPDATE` or a `DELETE`, and where `schema` cannot
/// settle a name, such as one of a schema it does not model, which the
/// translation of the SQL then answers for.
pub(crate) fn unresolved_in_body(
    sql: Sql<'_>,
    variables: &Variables<'_>,
    schema: &ParserDB,
) -> Option<(SqlState, String)> {
    let resolved = match sql {
        Sql::Statement(
            statement @ (Statement::Query(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)),
        ) => statement_in_body(statement, variables, schema),
        Sql::Expression(query) => {
            expression_in_body(&embedded::expression(query), variables, schema)
        }
        Sql::Statement(_) | Sql::Dynamic(_) => return None,
    };
    match resolved {
        Err(Error::Postgres { sqlstate, message }) => Some((sqlstate, message)),
        Ok(()) | Err(_) => None,
    }
}

/// Refuses the first relation `statement`, a statement of a PL/pgSQL body,
/// names that `schema` lacks, then its first target, read or written name
/// that does not resolve, in the order PostgreSQL meets them.
fn statement_in_body(
    statement: &Statement,
    variables: &Variables<'_>,
    schema: &ParserDB,
) -> Result<(), Error> {
    missing_relation(statement, schema)?;
    let context = ExpressionContext::for_statement(
        statement,
        &ExpressionScope::Empty,
        schema,
        IdentifierCase::AsWritten,
    )?;
    let arbiter = Arbiter::of(statement, schema)?;
    let relations = Relations::of(statement);
    insert_targets(statement, schema)?;
    read_names(statement, &context, &relations, &arbiter, schema, Some(variables))?;
    set_targets(statement, schema).map(drop)
}

/// Refuses the first relation `expression`, an expression of a PL/pgSQL
/// body, names that `schema` lacks, then its first name that does not
/// resolve.
fn expression_in_body(
    expression: &Expr,
    variables: &Variables<'_>,
    schema: &ParserDB,
) -> Result<(), Error> {
    missing_relation(expression, schema)?;
    let context = ExpressionContext::for_expression(
        expression,
        &ExpressionScope::Empty,
        schema,
        IdentifierCase::AsWritten,
    )?;
    let relations = Relations::default().read(expression);
    read_names(expression, &context, &relations, &Arbiter::default(), schema, Some(variables))
        .map(drop)
}

/// Refuses the first stored relation `node` names that `schema` holds
/// neither as a table nor as a view, as PostgreSQL does.
///
/// A name in a schema other than `public` and `pg_temp`, which `schema`
/// does not model, and a set-returning function called in `FROM` are left
/// alone.
fn missing_relation<V: Visit>(node: &V, schema: &ParserDB) -> Result<(), Error> {
    let mut functions = FunctionFactors(Vec::new());
    let _ = node.visit(&mut functions);
    let FunctionFactors(functions) = functions;
    for name in relation_occurrences(node) {
        if functions.contains(&name) || relation_exists(&name, schema) != Some(false) {
            continue;
        }
        let written: Vec<_> =
            name.0.iter().filter_map(ObjectNamePart::as_ident).map(stored_ident_name).collect();
        return Err(postgres(
            pg::UNDEFINED_TABLE,
            format!("relation \"{}\" does not exist", written.join(".")),
        ));
    }
    Ok(())
}

/// The names of the set-returning functions a node calls in `FROM`.
struct FunctionFactors(Vec<ObjectName>);

impl Visitor for FunctionFactors {
    type Break = ();

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        if let TableFactor::Table { name, args: Some(_), .. } = factor {
            self.0.push(name.clone());
        }
        ControlFlow::Continue(())
    }
}

/// Whether `schema` holds `name` as a table or a view, searching `pg_temp`
/// first for an unqualified name as PostgreSQL does, or `None` for a name
/// `schema` cannot settle.
fn relation_exists(name: &ObjectName, schema: &ParserDB) -> Option<bool> {
    let idents: Vec<&Ident> = name.0.iter().filter_map(ObjectNamePart::as_ident).collect();
    let (last, qualifier) = match idents.as_slice() {
        [last] => (*last, None),
        [qualifier, last] => (*last, Some(*qualifier)),
        _ => return None,
    };
    let target = TargetName::new(&last.value, last.quote_style.is_some());
    let candidates = match qualifier.map(|qualifier| (stored_ident_name(qualifier), qualifier)) {
        None => vec![target.clone().with_schema("pg_temp", false), target],
        Some((stored, qualifier)) if stored == "public" || stored == "pg_temp" => {
            vec![target.with_schema(&qualifier.value, qualifier.quote_style.is_some())]
        }
        Some(_) => return None,
    };
    for candidate in candidates {
        let table = schema.table_by_target(candidate.clone(), IdentifierCase::AsWritten);
        let view = schema.view_by_target(candidate, IdentifierCase::AsWritten);
        match (table, view) {
            (Ok(Some(_)), _) | (_, Ok(Some(_))) => return Some(true),
            (Err(_), _) | (_, Err(_)) => return None,
            (Ok(None), Ok(None)) => {}
        }
    }
    Some(false)
}

/// The stored columns a statement reads and its first ambiguous name.
struct Reads<'c> {
    columns: Vec<(&'c Table, &'c str)>,
    ambiguous: Option<Error>,
}

/// What the names of `root` read, its `FROM` items being `relations`, a name
/// a variable of `variables` reaches reading no column.
///
/// # Errors
///
/// Refuses the first name no relation in scope holds, as PostgreSQL does,
/// and returns the lookup errors resolution meets.
fn read_names<'c, N: Visit>(
    root: &N,
    context: &'c ExpressionContext<'_, '_, ParserDB>,
    relations: &Relations,
    arbiter: &Arbiter<'_>,
    schema: &ParserDB,
    variables: Option<&Variables<'_>>,
) -> Result<Reads<'c>, Error> {
    let mut reads = Reads { columns: Vec::new(), ambiguous: None };
    let mut failure = None;
    let _ = visit_expressions(root, |expression| {
        let address = ptr::from_ref(expression).addr();
        let resolved = if relations.argument_names.contains(&address) {
            Ok(Reference::Other)
        } else if arbiter.predicate.contains(&address) {
            arbiter.resolve(expression, schema).map(|()| Reference::Other)
        } else {
            resolve(expression, context, relations, schema, variables)
        };
        match resolved {
            Ok(Reference::Column(table, column)) => reads.columns.push((table, column)),
            Ok(Reference::Ambiguous(name)) => {
                reads.ambiguous.get_or_insert_with(|| ambiguous_column(&name));
            }
            Ok(Reference::Other) => {}
            Err(error) => {
                failure = Some(error);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
    failure.map_or(Ok(reads), Err)
}

/// What a statement answers given the ambiguity [`resolve_columns`] found and
/// its translation, PostgreSQL's `42702` unless the translation refuses the
/// statement for another reason.
fn with_ambiguity<T>(ambiguous: Option<Error>, translated: Result<T, Error>) -> Result<T, Error> {
    match (ambiguous, translated) {
        (
            Some(ambiguity),
            Ok(_) | Err(Error::LookupError(LookupError::AmbiguousTableLookup { .. })),
        ) => Err(ambiguity),
        (_, translated) => translated,
    }
}

/// Refuses a name of the `WHEN` condition of a trigger on `table` that
/// PostgreSQL refuses, where the row is in reach only as `NEW` and `OLD`,
/// both carrying every column of `table`.
///
/// # Errors
///
/// Returns [`Error::Postgres`] for a name reaching no column, a bare column,
/// which reaches both rows, and a relation other than `NEW` and `OLD`, and the
/// lookup errors resolution meets.
pub(crate) fn resolve_trigger_condition(
    condition: &Expr,
    table: &Table,
    schema: &ParserDB,
) -> Result<(), Error> {
    let mut failure = None;
    let _ = visit_expressions(condition, |expression| {
        let Some(parts) = name_parts(expression) else { return ControlFlow::Continue(()) };
        match condition_name(parts, table, schema) {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => {
                failure = Some(error);
                ControlFlow::Break(())
            }
        }
    });
    failure.map_or(Ok(()), Err)
}

/// What PostgreSQL answers for the name `parts` in the `WHEN` condition of a
/// trigger on `table`.
fn condition_name(parts: &[Ident], table: &Table, schema: &ParserDB) -> Result<(), Error> {
    let row = |ident: &Ident| matches!(stored_ident_name(ident).as_ref(), "new" | "old");
    match parts {
        [ident] if row(ident) || names_function(ident) => Ok(()),
        [ident] if declared(table, ident, schema)?.is_some() => {
            Err(ambiguous_column(&stored_ident_name(ident)))
        }
        [ident] => Err(undefined_column(ident)),
        [qualifier, column] if row(qualifier) => {
            if declared(table, column, schema)?.is_some() {
                Ok(())
            } else {
                Err(undefined_qualified_column(parts))
            }
        }
        [.., qualifier, _]
            if row(qualifier) || stored_ident_name(qualifier) == table.table_name() =>
        {
            Err(invalid_reference(qualifier))
        }
        [.., qualifier, _] => Err(missing_from_entry(&stored_ident_name(qualifier))),
        [] => Ok(()),
    }
}

/// The parts of the name `expression` writes, when it is a name.
fn name_parts(expression: &Expr) -> Option<&[Ident]> {
    match expression {
        Expr::Identifier(ident) => Some(core::slice::from_ref(ident)),
        Expr::CompoundIdentifier(parts) => Some(parts),
        _ => None,
    }
}

/// Whether the bare name `ident` is a keyword PostgreSQL reads as a function
/// or as `DEFAULT`, never as a column.
fn names_function(ident: &Ident) -> bool {
    ident.quote_style.is_none()
        && (ident.value.eq_ignore_ascii_case("default")
            || NAMED_FUNCTIONS.iter().any(|name| ident.value.eq_ignore_ascii_case(name)))
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

/// What `expression` reads, a name a variable of `variables` reaches reading
/// no column.
///
/// # Errors
///
/// Refuses a name no relation in scope holds, as PostgreSQL does.
fn resolve<'c>(
    expression: &Expr,
    context: &'c ExpressionContext<'_, '_, ParserDB>,
    relations: &Relations,
    schema: &ParserDB,
    variables: Option<&Variables<'_>>,
) -> Result<Reference<'c>, Error> {
    let Some(parts) = name_parts(expression) else { return Ok(Reference::Other) };
    match context.reference(expression) {
        Ok(Some((ColumnDefinition::Base { table, column, .. }, binding)))
            if relations.binds_directly(&binding, table, schema)? =>
        {
            Ok(Reference::Column(table, column.column_name()))
        }
        Ok(Some(_)) => Ok(Reference::Other),
        Err(LookupError::AmbiguousTableLookup { .. }) => {
            Ok(Reference::Ambiguous(
                parts
                    .last()
                    .map_or_else(String::new, |ident| stored_ident_name(ident).into_owned()),
            ))
        }
        Ok(None) => {
            unbound(expression, parts, relations, variables, schema).map(|()| Reference::Other)
        }
        Err(error) => Err(error.into()),
    }
}

/// What PostgreSQL answers for the name `parts` of `expression`, which no
/// column in scope holds and which reads a variable of `variables` when one
/// reaches it.
fn unbound(
    expression: &Expr,
    parts: &[Ident],
    relations: &Relations,
    variables: Option<&Variables<'_>>,
    schema: &ParserDB,
) -> Result<(), Error> {
    if let Some(variables) = variables {
        if let Some(error) = variables.nested_row_read(parts) {
            return Err(error);
        }
        if variables.reach(parts, schema)? {
            return Ok(());
        }
    }
    match parts {
        [ident] if names_function(ident) => Ok(()),
        // A whole-row reference.
        [ident] if relations.exposes(ident) => Ok(()),
        [ident]
            if matches!(stored_ident_name(ident).as_ref(), "old" | "new")
                && relations.returning.contains(&ptr::from_ref(expression).addr()) =>
        {
            Ok(())
        }
        [ident] => Err(undefined_column(ident)),
        [.., qualifier, _] if relations.hidden.contains(&ptr::from_ref(expression).addr()) => {
            Err(invalid_reference(qualifier))
        }
        [.., qualifier, _] if !relations.exposes(qualifier) => {
            Err(missing_from_entry(&stored_ident_name(qualifier)))
        }
        _ => Err(undefined_qualified_column(parts)),
    }
}

/// The names in reach at one point of a trigger body, and the table the
/// trigger fires on, whose row `NEW` and `OLD` hold.
pub(crate) struct Variables<'a> {
    pub(crate) namespace: &'a Namespace,
    pub(crate) rows: Option<&'a Table>,
}

impl Variables<'_> {
    /// Whether the name `parts` reaches a variable, a variable of any type
    /// taken to hold a row whose fields the parts after its name read.
    ///
    /// # Errors
    ///
    /// Refuses a field of `NEW` or `OLD` the trigger's table does not
    /// declare, as PostgreSQL does, and returns the lookup errors that
    /// meets.
    fn reach(&self, parts: &[Ident], schema: &ParserDB) -> Result<bool, Error> {
        let stored: Vec<_> = parts.iter().map(stored_ident_name).collect();
        let Some((name, consumed)) = self.namespace.lookup(&stored, |_| true) else {
            return Ok(false);
        };
        let record = match name.kind() {
            NameKind::Trigger(TriggerVariable::New) => "new",
            NameKind::Trigger(TriggerVariable::Old) => "old",
            _ => return Ok(true),
        };
        if let (Some(table), Some(field)) = (self.rows, parts.get(consumed))
            && declared(table, field, schema)?.is_none()
        {
            return Err(postgres(
                pg::UNDEFINED_COLUMN,
                format!("record \"{record}\" has no field \"{}\"", stored_ident_name(field)),
            ));
        }
        Ok(true)
    }

    /// PostgreSQL's error for a read naming more than one part past `NEW` or
    /// `OLD`, which it takes for a column of a relation the query lists.
    fn nested_row_read(&self, parts: &[Ident]) -> Option<Error> {
        let stored: Vec<_> = parts.iter().map(stored_ident_name).collect();
        let (name, consumed) = self.namespace.lookup(stored.get(..1)?, |_| true)?;
        if !matches!(name.kind(), NameKind::Trigger(TriggerVariable::New | TriggerVariable::Old)) {
            return None;
        }
        match parts.len().checked_sub(consumed)? {
            0 | 1 => None,
            2 => Some(missing_from_entry(&stored[consumed])),
            _ => {
                Some(postgres(
                    pg::FEATURE_NOT_SUPPORTED,
                    format!("cross-database references are not implemented: {}", stored.join(".")),
                ))
            }
        }
    }
}

/// The SQLSTATE and message PostgreSQL raises where a PL/pgSQL body assigns
/// the target `parts`, for a field of `NEW` or `OLD` the trigger's table
/// does not declare.
pub(crate) fn unresolved_target(
    parts: &[Ident],
    variables: &Variables<'_>,
    schema: &ParserDB,
) -> Option<(SqlState, String)> {
    match variables.reach(parts, schema) {
        Err(Error::Postgres { sqlstate, message }) => Some((sqlstate, message)),
        Ok(_) | Err(_) => None,
    }
}

fn postgres(sqlstate: SqlState, message: String) -> Error {
    Error::Postgres { sqlstate, message }
}

fn undefined_column(name: &Ident) -> Error {
    postgres(pg::UNDEFINED_COLUMN, format!("column \"{}\" does not exist", stored_ident_name(name)))
}

fn undefined_qualified_column(parts: &[Ident]) -> Error {
    let stored: Vec<_> = parts.iter().map(stored_ident_name).collect();
    postgres(pg::UNDEFINED_COLUMN, format!("column {} does not exist", stored.join(".")))
}

/// The refusal of `name`, as PostgreSQL stores it, reaching more than one
/// column.
fn ambiguous_column(name: &str) -> Error {
    postgres(pg::AMBIGUOUS_COLUMN, format!("column reference \"{name}\" is ambiguous"))
}

fn invalid_reference(qualifier: &Ident) -> Error {
    postgres(
        pg::UNDEFINED_TABLE,
        format!(
            "invalid reference to FROM-clause entry for table \"{}\"",
            stored_ident_name(qualifier)
        ),
    )
}

/// The refusal of a qualifier `table`, as PostgreSQL stores it, that no
/// `FROM` entry exposes.
fn missing_from_entry(table: &str) -> Error {
    postgres(pg::UNDEFINED_TABLE, format!("missing FROM-clause entry for table \"{table}\""))
}

/// PostgreSQL's refusal of a statement needing a privilege the acting role
/// lacks on `table`.
pub(crate) fn denied(table: &Table) -> Error {
    postgres(pg::INSUFFICIENT_PRIVILEGE, permission_denied(table.table_name()))
}

/// The `WHERE` of an `INSERT`'s `ON CONFLICT` target, which reads the
/// target table's columns.
#[derive(Default)]
struct Arbiter<'s> {
    table: Option<&'s Table>,
    /// The address of every node of the predicate.
    predicate: Vec<usize>,
}

impl<'s> Arbiter<'s> {
    fn of(statement: &Statement, schema: &'s ParserDB) -> Result<Self, Error> {
        let mut arbiter = Self::default();
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
            Err(undefined_column(ident))
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

    /// Whether `grants` hold this write of `column`.
    fn held(self, grants: &RoleTable, column: &str) -> bool {
        match self {
            Self::Insert => grants.inserts(column),
            Self::Update => grants.updates(column),
        }
    }
}

/// One column a statement writes.
struct Target<'s> {
    table: &'s Table,
    column: &'s str,
    write: Write,
}

/// The columns the rows `statement` inserts fill, an `INSERT` naming none
/// filling the leading columns its rows fill, and none known behind a
/// wildcard.
///
/// # Errors
///
/// Refuses a target the table does not declare, as PostgreSQL does before
/// reading any name of the statement.
fn insert_targets<'s>(
    statement: &Statement,
    schema: &'s ParserDB,
) -> Result<Vec<Target<'s>>, Error> {
    let mut targets = Vec::new();
    let Statement::Insert(insert) = statement else { return Ok(targets) };
    let TableObject::TableName(name) = &insert.table else { return Ok(targets) };
    let Some(table) = resolve_translation_table(schema, name)? else { return Ok(targets) };
    if insert.columns.is_empty() {
        let width = insert.source.as_deref().map_or(Some(0), |source| row_width(&source.body));
        for column in table.columns(schema)?.take(width.unwrap_or(0)) {
            targets.push(Target { table, column: column.column_name(), write: Write::Insert });
        }
    } else {
        for column in insert.columns.iter().filter_map(last_ident) {
            targets.push(target(table, column, Write::Insert, schema)?);
        }
    }
    Ok(targets)
}

/// The columns the `SET` of an `UPDATE` or of an `INSERT`'s `ON CONFLICT DO
/// UPDATE` writes.
///
/// # Errors
///
/// Refuses a target the table does not declare, as PostgreSQL does after
/// reading the names of the statement.
fn set_targets<'s>(statement: &Statement, schema: &'s ParserDB) -> Result<Vec<Target<'s>>, Error> {
    let mut targets = Vec::new();
    let (name, assignments) = match statement {
        Statement::Insert(insert) => {
            let (TableObject::TableName(name), Some(OnInsert::OnConflict(conflict))) =
                (&insert.table, &insert.on)
            else {
                return Ok(targets);
            };
            let OnConflictAction::DoUpdate(update) = &conflict.action else { return Ok(targets) };
            (name, &update.assignments)
        }
        Statement::Update(update) => {
            let TableFactor::Table { name, .. } = &update.table.relation else {
                return Ok(targets);
            };
            (name, &update.assignments)
        }
        _ => return Ok(targets),
    };
    let Some(table) = resolve_translation_table(schema, name)? else { return Ok(targets) };
    assigned(assignments, table, schema, &mut targets)?;
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
                stored_ident_name(written),
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
#[derive(Default)]
struct Relations {
    factors: Vec<usize>,
    /// Copied, since the visitor lends each node to its callback alone.
    names: Vec<Ident>,
    argument_names: Vec<usize>,
    returning: Vec<usize>,
    /// The `FROM` items of each enclosing query level, innermost last, with
    /// how many non-`LATERAL` derived tables of that level are being visited.
    levels: Vec<(Vec<Ident>, usize)>,
    /// Qualified references whose qualifier names a `FROM` item of an
    /// enclosing level that a non-`LATERAL` derived table cannot see.
    hidden: Vec<usize>,
}

impl Relations {
    fn of(statement: &Statement) -> Self {
        let mut relations = Self::default();
        if let Statement::Insert(Insert {
            table: TableObject::TableName(name),
            table_alias,
            on,
            ..
        }) = statement
        {
            let exposed = table_alias.as_ref().map(|alias| &alias.alias).or(last_ident(name));
            relations.names.extend(exposed.cloned());
            if let Some(OnInsert::OnConflict(conflict)) = on
                && let OnConflictAction::DoUpdate(_) = conflict.action
            {
                relations.names.push(Ident::new("excluded"));
            }
        }
        let returning = match statement {
            Statement::Insert(insert) => insert.returning.as_ref(),
            Statement::Update(update) => update.returning.as_ref(),
            Statement::Delete(delete) => delete.returning.as_ref(),
            _ => None,
        };
        if let Some(returning) = returning {
            let _ = visit_expressions(returning, |expression| {
                if let Expr::Identifier(ident) = expression
                    && matches!(stored_ident_name(ident).as_ref(), "old" | "new")
                {
                    relations.returning.push(ptr::from_ref(expression).addr());
                }
                ControlFlow::<()>::Continue(())
            });
        }
        relations.read(statement)
    }

    /// These relations with the `FROM` items `node` holds.
    fn read<N: Visit>(mut self, node: &N) -> Self {
        let _ = node.visit(&mut self);
        self
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
pub(crate) fn holds(names: &[Ident], name: &Ident) -> bool {
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
pub(crate) fn from_names(from: &TableWithJoins, names: &mut Vec<Ident>) {
    for factor in
        core::iter::once(&from.relation).chain(from.joins.iter().map(|join| &join.relation))
    {
        names.extend(exposed_name(factor).cloned());
        if let TableFactor::NestedJoin { table_with_joins, .. } = factor {
            from_names(table_with_joins, names);
        }
    }
}
