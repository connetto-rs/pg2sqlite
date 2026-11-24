//! Submodule defining a struct providing options for the translation.

use alloc::borrow::Cow;

use sql_traits::{
    structs::{
        ColumnDefinition, ColumnDefinitionScope, ColumnQueryScope, ColumnScope, Deployment,
        IdentifierCase, ParserDB,
    },
    traits::DatabaseLike,
};
use sqlparser::ast::{Expr, ObjectName};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    impls::{
        request_zone::{RequestZone, ZoneUnavailable},
        translator_impls::{
            array::WholeRow,
            drop_column_rebuild::StatementHistory,
            trigger_row::{TriggerRow, TriggerRows},
            trigger_slots::{TableKey, table_key},
        },
    },
    traits::{
        ArrayRepresentation, CaseFoldFunctions, JsonbFunctions, SessionVariableMapping,
        TimeZoneFunctions, UuidRepresentation,
    },
    warnings::DeclaredObject,
};

type Role = <ParserDB as DatabaseLike>::Role;

/// Tables whose storage lives in a database attached to the replica's
/// connection, set with [`Pg2SqliteOptions::with_attached_tables`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct AttachedTables {
    schema: String,
    tables: Vec<String>,
}

impl AttachedTables {
    /// The name the database is attached under, as in `ATTACH ... AS <schema>`.
    #[must_use]
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// The source names of the tables placed in the attached database.
    #[must_use]
    pub fn tables(&self) -> &[String] {
        &self.tables
    }

    /// Whether `table` is listed, comparing as SQLite compares names, without
    /// regard to ASCII case.
    #[must_use]
    pub fn contains(&self, table: &str) -> bool {
        self.tables.iter().any(|listed| listed.eq_ignore_ascii_case(table))
    }
}

/// Struct to hold options for the translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pg2SqliteOptions {
    /// Whether to drop check constraints containing unsupported functions.
    remove_unsupported_check_constraints: bool,
    /// The representation of UUIDs in `SQLite`.
    uuid_representation: Option<UuidRepresentation>,
    /// The name of the function to use for random UUID generation.
    /// Its runtime return type must match `uuid_representation`.
    uuid_function_name: String,
    /// The name of the destination's version 7 UUID generator. `None` means
    /// the destination has none, and `uuidv7()` is refused rather than
    /// answered with a random value. Set via `with_uuid_v7_function_name`.
    uuid_v7_function_name: Option<String>,
    /// Optional UDF name for converting text UUID literals to 16-byte
    /// BLOBs at INSERT/UPDATE time. `None` means "emit
    /// `unhex(replace(literal, '-', ''))` inline" (pure SQLite, no UDF
    /// setup). Set via `with_uuid_text_to_blob_function_name`.
    uuid_text_to_blob_function_name: Option<String>,
    /// The host-registered SQLite functions that read a zone whose UTC offset
    /// changes. `None` refuses what needs one. Set via
    /// `with_time_zone_functions`.
    time_zone_functions: Option<TimeZoneFunctions>,
    /// The host-registered SQLite functions and collation that canonicalise,
    /// read as text and compare `jsonb`. `None` refuses what needs any of
    /// them. Set via `with_jsonb_functions`.
    jsonb_functions: Option<JsonbFunctions>,
    /// The host-registered SQLite functions that fold case as the server
    /// does. `None` refuses a fold SQLite's own `lower` and `upper` cannot
    /// answer. Set via `with_case_fold_functions`.
    case_fold_functions: Option<CaseFoldFunctions>,
    /// Names of host-registered functions the destination SQLite provides,
    /// stored lower-cased because SQLite resolves function names without
    /// regard to case. A name here passes through instead of being refused as
    /// unrecognised. Set via `with_user_defined_functions`.
    user_defined_functions: Vec<String>,
    /// The representation of PostgreSQL arrays in `SQLite`. `None` rejects
    /// every array construct instead of downgrading it silently.
    array_representation: Option<ArrayRepresentation>,
    /// The suffix to append to table names when renaming them for RLS views.
    rls_table_suffix: String,
    /// The reserved marker used to name deny triggers for read-only non-RLS
    /// tables (`<table><marker>_insert` and so on).
    readonly_deny_trigger_suffix: String,
    /// The deployment acting role this translation runs as.
    acting_role: Option<String>,
    /// The role the schema's statements run as, owning what they create.
    schema_owner_role: Option<String>,
    /// The SQLite functions answering the deployment's session settings.
    session_variables: Vec<SessionVariableMapping>,
    /// The roles and session settings the deployment's requests run with.
    deployment: Option<Deployment>,
    /// The name of the audit table for RLS validation monitoring.
    rls_audit_table_name: Option<String>,
    /// The caller-registered function that lifts row-level security and the
    /// generated write guards.
    rls_exemption_function: Option<String>,
    /// Whether to enable strict RLS validation (abort on violations).
    strict_rls_validation: bool,
    /// Whether a write denied by a policy `USING` clause raises instead of
    /// affecting zero rows as PostgreSQL does.
    strict_rls_write_deny: bool,
    /// Whether to enable SQLiteGIS-backed PostGIS translation (passthrough of
    /// `ST_*` scalar functions in SQLiteGIS's catalog).
    sqlitegis_enabled: bool,
    /// Whether the destination SQLite is declared to provide the math
    /// functions, which ship only under `SQLITE_ENABLE_MATH_FUNCTIONS`.
    math_functions_available: bool,
    /// Tables placed in an attached database. Set via `with_attached_tables`.
    attached_tables: Option<AttachedTables>,
    /// The function each write a trigger derives runs between calls of. Set
    /// via `with_server_derived_write_function`.
    server_derived_write_function: Option<String>,
    /// Whether the columns stored as text for a type PostgreSQL bounds go
    /// without the `CHECK` holding them to the text the translation writes.
    /// Set via `with_stored_text_checks_omitted`.
    stored_text_checks_omitted: bool,
    /// Whether every foreign key is left to the server, read for the
    /// translation's analysis and emitted nowhere. Set via
    /// `with_foreign_keys_omitted`.
    foreign_keys_omitted: bool,
    /// The major version of the PostgreSQL server the replica stands in for.
    /// `None` refuses what answers differently across majors. Set via
    /// `with_postgres_major`.
    postgres_major: Option<u16>,
}

#[cfg(feature = "arbitrary")]
impl<'a> arbitrary::Arbitrary<'a> for Pg2SqliteOptions {
    /// Randomises user-facing configuration only.
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self {
            remove_unsupported_check_constraints: bool::arbitrary(u)?,
            uuid_representation: Option::<UuidRepresentation>::arbitrary(u)?,
            uuid_function_name: String::arbitrary(u)?,
            uuid_v7_function_name: Option::<String>::arbitrary(u)?,
            uuid_text_to_blob_function_name: Option::<String>::arbitrary(u)?,
            time_zone_functions: Option::<TimeZoneFunctions>::arbitrary(u)?,
            jsonb_functions: Option::<JsonbFunctions>::arbitrary(u)?,
            case_fold_functions: Option::<CaseFoldFunctions>::arbitrary(u)?,
            array_representation: Option::<ArrayRepresentation>::arbitrary(u)?,
            rls_table_suffix: String::arbitrary(u)?,
            readonly_deny_trigger_suffix: String::arbitrary(u)?,
            acting_role: Option::<String>::arbitrary(u)?,
            schema_owner_role: Option::<String>::arbitrary(u)?,
            session_variables: Vec::<SessionVariableMapping>::arbitrary(u)?,
            deployment: None,
            rls_audit_table_name: Option::<String>::arbitrary(u)?,
            rls_exemption_function: Option::<String>::arbitrary(u)?,
            strict_rls_validation: bool::arbitrary(u)?,
            strict_rls_write_deny: bool::arbitrary(u)?,
            sqlitegis_enabled: bool::arbitrary(u)?,
            math_functions_available: bool::arbitrary(u)?,
            user_defined_functions: Vec::<String>::arbitrary(u)?,
            attached_tables: Option::<AttachedTables>::arbitrary(u)?,
            server_derived_write_function: Option::<String>::arbitrary(u)?,
            stored_text_checks_omitted: bool::arbitrary(u)?,
            foreign_keys_omitted: bool::arbitrary(u)?,
            postgres_major: Option::<u16>::arbitrary(u)?,
        })
    }
}

impl Default for Pg2SqliteOptions {
    fn default() -> Self {
        Self {
            remove_unsupported_check_constraints: false,
            uuid_representation: None,
            uuid_function_name: "uuid".to_string(),
            uuid_v7_function_name: None,
            uuid_text_to_blob_function_name: None,
            time_zone_functions: None,
            jsonb_functions: None,
            case_fold_functions: None,
            array_representation: None,
            rls_table_suffix: "_rls".to_string(),
            readonly_deny_trigger_suffix: "__readonly".to_string(),
            acting_role: None,
            schema_owner_role: None,
            session_variables: Vec::new(),
            deployment: None,
            rls_audit_table_name: None,
            rls_exemption_function: None,
            strict_rls_validation: false,
            strict_rls_write_deny: false,
            sqlitegis_enabled: false,
            math_functions_available: false,
            user_defined_functions: Vec::new(),
            attached_tables: None,
            server_derived_write_function: None,
            stored_text_checks_omitted: false,
            foreign_keys_omitted: false,
            postgres_major: None,
        }
    }
}

/// The script-wide facts a pass before translation collects, shared by every
/// [`TranslationContext`] derived for a query so a child costs no copying.
#[derive(Default, Clone)]
pub(crate) struct PrewalkCatalogs {
    spatial_indexes: Vec<(String, String)>,
    fts_indexes: Vec<(String, String)>,
    declared_object_names: Vec<String>,
    trigger_function_names: Vec<String>,
    /// The folded table and trigger names of the row triggers sharing a
    /// table, timing and event with another, whose `WHEN` moves into the body
    /// so the slot can run them as one SQLite trigger.
    shared_slot_triggers: alloc::collections::BTreeSet<(TableKey, String)>,
    /// The table identity, function name and event of every
    /// `BEFORE INSERT` or `BEFORE UPDATE` trigger, one entry per event, which
    /// the RLS backing-table update check reads to let the crate's own
    /// maintenance write through. Only those fields are kept, since holding
    /// whole trigger nodes here costs link-time drop glue out of proportion
    /// to what the check needs.
    before_row_trigger_fns: Vec<(
        TableKey,
        sqlparser::ast::ObjectName,
        crate::impls::translator_impls::reentry::MaintainedEvent,
    )>,
    /// The triggers each table carries after the statements translated so
    /// far, which a write left off the replica with its table reads to find
    /// one PostgreSQL fires for it.
    table_triggers: Vec<TableTrigger>,
    /// The deferred constraint triggers the translation unit creates.
    deferred_triggers: Vec<crate::impls::translator_impls::deferred_constraint::DeferredTrigger>,
    /// The request's zone, resolved the first time an operation needs it.
    request_zone: core::cell::OnceCell<Result<RequestZone, ZoneUnavailable>>,
    /// What the batch's translation works out once, kept for the schemas it
    /// holds alive, boxed so every context stays as small.
    batch_cache: Box<core::cell::RefCell<crate::impls::batch_cache::BatchCache>>,
    /// The helpers the translation calls as table-valued functions, in the
    /// order first called.
    helper_functions: core::cell::RefCell<Vec<crate::script::HelperFunction>>,
}

/// A trigger on a table or a view.
#[derive(Clone)]
pub(crate) struct TableTrigger {
    /// The trigger's name as written.
    pub(crate) name: String,
    /// The table's or view's identity.
    table: TableKey,
    /// The events the trigger fires on.
    pub(crate) events: Vec<sqlparser::ast::TriggerEvent>,
    pub(crate) period: Option<sqlparser::ast::TriggerPeriod>,
    pub(crate) row_level: bool,
}

#[derive(Clone, Copy)]
enum TranslationScope<'a> {
    Query {
        scope: &'a ColumnScope<'a, 'a, ParserDB>,
        pseudo_row: bool,
        original_query: Option<&'a sqlparser::ast::Query>,
        /// The set-returning JSON functions among the query's relations, whose
        /// alias a bare name may read as a whole row.
        whole_rows: &'a [WholeRow<'a>],
    },
    Definition(ColumnDefinitionScope<'a, 'a, 'a, ParserDB>),
}

/// A PL/pgSQL variable bound in the body being translated, with the
/// declaration a reference to it reads its type from.
#[derive(Debug, Clone)]
pub(crate) struct BoundVariable {
    name: String,
    declaration: Option<sqlparser::ast::ColumnDef>,
}

impl BoundVariable {
    /// The variable `name` declared as `data_type`, or without a declaration
    /// when its type is written as `%TYPE` or `%ROWTYPE`.
    pub(crate) fn new(name: String, data_type: Option<sqlparser::ast::DataType>) -> Self {
        let declaration = data_type.map(|data_type| {
            sqlparser::ast::ColumnDef {
                name: sqlparser::ast::Ident::new(name.clone()),
                data_type,
                options: Vec::new(),
            }
        });
        Self::declared(name, declaration)
    }

    /// The variable `name` whose references read `declaration`.
    pub(crate) const fn declared(
        name: String,
        declaration: Option<sqlparser::ast::ColumnDef>,
    ) -> Self {
        Self { name, declaration }
    }
}

/// The options a translation runs under, together with what it has learned
/// about the statements around the one it is translating.
///
/// Built from [`Pg2SqliteOptions`], which it dereferences to, so every setting
/// reads through it unchanged. Both directions carry it, because both resolve a
/// column reference against the relations in scope before reading a declared
/// type.
pub struct TranslationContext<'a> {
    options: Cow<'a, Pg2SqliteOptions>,
    catalogs: Cow<'a, PrewalkCatalogs>,
    /// The relations whose columns a reference in this context can name. A
    /// query attaches its own `FROM` clause and a definition attaches the table
    /// it belongs to, so a type-dependent rewrite reads the column it actually
    /// names rather than any column of that name in the schema. `None` means
    /// nothing is in scope, which refuses rather than guessing.
    scope: Option<TranslationScope<'a>>,
    /// The context this one was derived from, whose scope answers a reference
    /// the inner one cannot. PostgreSQL resolves an outer reference in a
    /// correlated subquery against the enclosing query, and a trigger body
    /// reads the guarded table the same way, so resolution walks outward rather
    /// than stopping at the innermost `FROM`.
    outer: Option<&'a TranslationContext<'a>>,
    /// The `WITH` clause of the query being translated, so a scope built for
    /// one arm of a set operation still reads a CTE reference as opaque rather
    /// than matching a base table of the same name.
    with: Option<&'a sqlparser::ast::With>,
    /// The PL/pgSQL variables bound in the body being translated. A variable
    /// is not a column, so a reference naming one reads the variable's
    /// declaration, or nothing for a variable without one, and is never
    /// resolved against the relations in scope.
    variables: &'a [BoundVariable],
    /// Whether every relation available to the input is present in the schema.
    schema_is_complete: bool,
    subqueries_allowed: bool,
    /// The schema as the statement being translated was written, which names
    /// a wildcard's columns before a later column change in its epoch.
    written_schema: Option<&'a ParserDB>,
    /// What the batch emitted before the statement being translated, which a
    /// table rebuild recreates indexes and triggers from.
    history: Option<StatementHistory<'a>>,
    /// The role the body being translated runs as, the owner of a `SECURITY
    /// DEFINER` trigger function, which PostgreSQL checks a call's privileges
    /// against in place of the acting role.
    running_as: Option<&'a Role>,
    /// The innermost `LANGUAGE sql` function whose body is being translated
    /// in place of a call, linked to the ones enclosing it.
    inlined_body: Option<&'a InlinedBody<'a>>,
    /// The table of the row trigger being translated and the rows its events
    /// give it, which `NEW` and `OLD` read whole.
    trigger_rows: Option<TriggerRows<'a>>,
    trigger_catalog: Option<&'a [TableTrigger]>,
    /// The addresses of the `FROM` items of the query being translated that
    /// a row-locking clause covers, which PostgreSQL filters by the `UPDATE`
    /// policies as well. Only a node of that query matches one, so a context
    /// a subquery of an expression inherits matches nothing.
    locked: &'a [usize],
    /// Whether the query about to be translated sits in a locked `FROM` item,
    /// which locks every item of its own `FROM`.
    locks_whole_query: bool,
    /// The declaration whose expressions a warning names, `None` for a query.
    declaration: Option<Declaration<'a>>,
    /// The key of the table the `INSERT` whose source is being translated
    /// writes.
    inserting_into: Option<&'a str>,
}

/// A declaration whose expressions a translation reads.
#[derive(Clone, Copy)]
pub(crate) enum Declaration<'a> {
    /// A statement, which names an object when it declares or alters one or
    /// writes a table or view, and the schema it was written against, which
    /// tells a written view from a table.
    Statement {
        /// The statement.
        statement: &'a sqlparser::ast::Statement,
        /// The schema it was written against.
        schema: &'a ParserDB,
    },
    /// A row level security policy, whose expressions are translated under
    /// whichever statement builds its table's view and triggers.
    Policy(&'a sqlparser::ast::CreatePolicy),
}

impl Declaration<'_> {
    /// The object it declares or writes, `None` for a statement that names
    /// none.
    fn object(self) -> Option<DeclaredObject> {
        match self {
            Self::Statement { statement, schema } => {
                DeclaredObject::of_statement(statement, schema)
            }
            Self::Policy(policy) => Some(DeclaredObject::of_policy(policy)),
        }
    }
}

/// A `LANGUAGE sql` function whose body is being translated in place of a
/// call, linked to the function whose body holds that call, if any.
pub(crate) struct InlinedBody<'a> {
    pub(crate) function: &'a sqlparser::ast::CreateFunction,
    pub(crate) outer: Option<&'a InlinedBody<'a>>,
}

impl InlinedBody<'_> {
    /// The functions being translated in place of a call, innermost first.
    pub(crate) fn chain(&self) -> impl Iterator<Item = &sqlparser::ast::CreateFunction> {
        core::iter::successors(Some(self), |body| body.outer).map(|body| body.function)
    }
}

/// The fields a derived [`TranslationContext`] copies from an existing one,
/// the options and catalogs borrowed rather than cloned.
struct ContextFields<'a> {
    options: Cow<'a, Pg2SqliteOptions>,
    catalogs: Cow<'a, PrewalkCatalogs>,
    with: Option<&'a sqlparser::ast::With>,
    variables: &'a [BoundVariable],
    schema_is_complete: bool,
    subqueries_allowed: bool,
    written_schema: Option<&'a ParserDB>,
    history: Option<StatementHistory<'a>>,
    running_as: Option<&'a Role>,
    inlined_body: Option<&'a InlinedBody<'a>>,
    trigger_rows: Option<TriggerRows<'a>>,
    trigger_catalog: Option<&'a [TableTrigger]>,
    locked: &'a [usize],
    locks_whole_query: bool,
    declaration: Option<Declaration<'a>>,
    inserting_into: Option<&'a str>,
}

impl<'a> ContextFields<'a> {
    fn borrowed(context: &'a TranslationContext<'a>) -> Self {
        Self {
            options: Cow::Borrowed(context.options.as_ref()),
            catalogs: Cow::Borrowed(context.catalogs.as_ref()),
            with: context.with,
            variables: context.variables,
            schema_is_complete: context.schema_is_complete,
            subqueries_allowed: context.subqueries_allowed,
            written_schema: context.written_schema,
            history: context.history,
            running_as: context.running_as,
            inlined_body: context.inlined_body,
            trigger_rows: context.trigger_rows,
            trigger_catalog: context.trigger_catalog,
            locked: context.locked,
            locks_whole_query: context.locks_whole_query,
            declaration: context.declaration,
            inserting_into: context.inserting_into,
        }
    }

    /// The context these fields complete, with the caller's `scope` and
    /// `outer`.
    fn into_context(
        self,
        scope: Option<TranslationScope<'a>>,
        outer: Option<&'a TranslationContext<'a>>,
    ) -> TranslationContext<'a> {
        TranslationContext {
            options: self.options,
            catalogs: self.catalogs,
            scope,
            outer,
            with: self.with,
            variables: self.variables,
            schema_is_complete: self.schema_is_complete,
            subqueries_allowed: self.subqueries_allowed,
            written_schema: self.written_schema,
            history: self.history,
            running_as: self.running_as,
            inlined_body: self.inlined_body,
            trigger_rows: self.trigger_rows,
            trigger_catalog: self.trigger_catalog,
            locked: self.locked,
            locks_whole_query: self.locks_whole_query,
            declaration: self.declaration,
            inserting_into: self.inserting_into,
        }
    }
}

impl<'a> TranslationContext<'a> {
    /// Builds a context that carries `options` and no scope, which is what a
    /// caller starting a translation has.
    #[must_use]
    pub fn new(options: &'a Pg2SqliteOptions) -> Self {
        Self {
            options: Cow::Borrowed(options),
            catalogs: Cow::Owned(PrewalkCatalogs::default()),
            scope: None,
            outer: None,
            with: None,
            variables: &[],
            schema_is_complete: false,
            subqueries_allowed: true,
            written_schema: None,
            history: None,
            running_as: None,
            inlined_body: None,
            trigger_rows: None,
            trigger_catalog: None,
            locked: &[],
            locks_whole_query: false,
            declaration: None,
            inserting_into: None,
        }
    }
    pub(crate) fn with_complete_schema(options: &'a Pg2SqliteOptions) -> Self {
        let mut context = Self::new(options);
        context.schema_is_complete = true;
        context
    }

    #[cfg(test)]
    pub(crate) fn from_owned(options: Pg2SqliteOptions) -> TranslationContext<'static> {
        TranslationContext {
            options: Cow::Owned(options),
            catalogs: Cow::Owned(PrewalkCatalogs::default()),
            scope: None,
            outer: None,
            with: None,
            variables: &[],
            schema_is_complete: false,
            subqueries_allowed: true,
            written_schema: None,
            history: None,
            running_as: None,
            inlined_body: None,
            trigger_rows: None,
            trigger_catalog: None,
            locked: &[],
            locks_whole_query: false,
            declaration: None,
            inserting_into: None,
        }
    }

    /// The same context with `scope` attached, for translating the expressions
    /// of one query, one definition, or one naked expression a caller hands in.
    ///
    /// The script-wide catalogs are borrowed rather than copied, so a child is
    /// two pointers. Building one is the only way a scope enters translation,
    /// which is why forgetting to build one loses the scope rather than leaving
    /// an enclosing query's relations in place. A reference the attached scope
    /// cannot answer is tried against the enclosing one, as PostgreSQL resolves
    /// an outer reference.
    #[must_use]
    pub fn with_scope<'s>(
        &'s self,
        scope: &'s ColumnScope<'s, 's, ParserDB>,
    ) -> TranslationContext<'s> {
        self.query_scoped(scope, false, None, &[])
    }

    /// Retains original query identity alongside column resolution.
    pub(crate) fn with_query_scope<'s>(
        &'s self,
        scope: &'s ColumnScope<'s, 's, ParserDB>,
        query: &'s sqlparser::ast::Query,
        whole_rows: &'s [WholeRow<'s>],
    ) -> TranslationContext<'s> {
        self.query_scoped(scope, false, Some(query), whole_rows)
    }

    /// The same context with the scope of one `SELECT` whose relations
    /// include the set-returning JSON functions `whole_rows` describes.
    pub(crate) fn with_select_scope<'s>(
        &'s self,
        scope: &'s ColumnScope<'s, 's, ParserDB>,
        whole_rows: &'s [WholeRow<'s>],
    ) -> TranslationContext<'s> {
        self.query_scoped(scope, false, None, whole_rows)
    }

    pub(crate) fn with_pseudo_row_scope<'s>(
        &'s self,
        scope: &'s ColumnScope<'s, 's, ParserDB>,
    ) -> TranslationContext<'s> {
        self.query_scoped(scope, true, None, &[])
    }

    /// The same context inside a query whose columns `scope` resolves, with
    /// this context as the enclosing one.
    fn query_scoped<'s>(
        &'s self,
        scope: &'s ColumnScope<'s, 's, ParserDB>,
        pseudo_row: bool,
        original_query: Option<&'s sqlparser::ast::Query>,
        whole_rows: &'s [WholeRow<'s>],
    ) -> TranslationContext<'s> {
        ContextFields::borrowed(self).into_context(
            Some(TranslationScope::Query { scope, pseudo_row, original_query, whole_rows }),
            Some(self),
        )
    }

    pub(crate) fn with_definition_scope<'s>(
        &'s self,
        scope: ColumnDefinitionScope<'s, 's, 's, ParserDB>,
    ) -> TranslationContext<'s> {
        ContextFields::borrowed(self)
            .into_context(Some(TranslationScope::Definition(scope)), Some(self))
    }

    /// The same context for the body of a derived table that is not
    /// `LATERAL`, in the `FROM` clause of the query whose scope this context
    /// carries.
    ///
    /// PostgreSQL resolves that body against the queries enclosing the one
    /// holding it and never against that query's own `FROM` items, measured
    /// on 18.6, so the query's level is left out and the levels above it
    /// stay.
    ///
    /// When a row-locking clause covers `item`, the query of that body locks
    /// every item of its own `FROM`.
    pub(crate) fn derived_table_body(
        &self,
        item: &sqlparser::ast::TableFactor,
    ) -> TranslationContext<'_> {
        debug_assert!(
            matches!(self.scope, Some(TranslationScope::Query { .. })),
            "a FROM item is translated under the scope of the query holding it"
        );
        let (scope, outer) = self.outer.map_or((None, None), |outer| (outer.scope, outer.outer));
        let mut fields = ContextFields::borrowed(self);
        fields.locks_whole_query = self.locks(item);
        fields.into_context(scope, outer)
    }

    /// The same context noting `with` as the enclosing query's CTE clause.
    pub(crate) fn with_cte_clause<'s>(
        &'s self,
        with: Option<&'s sqlparser::ast::With>,
    ) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.with = with;
        fields.into_context(self.scope, self.outer)
    }

    /// The same context translating the expressions of `declaration`.
    pub(crate) fn with_declaration<'s>(
        &'s self,
        declaration: Declaration<'s>,
    ) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.declaration = Some(declaration);
        fields.into_context(self.scope, self.outer)
    }

    /// The object whose expressions are being translated or the table or
    /// view written, `None` for a query.
    pub(crate) fn declared_object(&self) -> Option<DeclaredObject> {
        self.declaration.and_then(Declaration::object)
    }

    /// The same context noting the PL/pgSQL variables in scope.
    pub(crate) fn with_variables<'s>(
        &'s self,
        variables: &'s [BoundVariable],
    ) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.variables = variables;
        fields.into_context(self.scope, self.outer)
    }

    /// The same context noting `schema` as the one the statement was written
    /// against.
    pub(crate) fn with_written_schema<'s>(
        &'s self,
        schema: &'s ParserDB,
    ) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.written_schema = Some(schema);
        fields.into_context(self.scope, self.outer)
    }

    /// The schema the statement being translated was written against.
    pub(crate) const fn written_schema(&self) -> Option<&ParserDB> {
        self.written_schema
    }

    /// Derives a context translating a statement of the batch after the
    /// statements `history` follows.
    pub(crate) fn with_history<'s>(
        &'s self,
        history: StatementHistory<'s>,
    ) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.history = Some(history);
        fields.into_context(self.scope, self.outer)
    }

    /// What the batch emitted before the statement being translated, absent
    /// when the statement is translated alone.
    pub(crate) const fn history(&self) -> Option<StatementHistory<'a>> {
        self.history
    }

    /// The same context translating a body that runs as `role`.
    pub(crate) fn with_running_as<'s>(&'s self, role: &'s Role) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.running_as = Some(role);
        fields.into_context(self.scope, self.outer)
    }

    /// The role the body being translated runs as, when it runs as other than
    /// the acting role.
    pub(crate) const fn running_as(&self) -> Option<&'a Role> {
        self.running_as
    }

    /// The same context translating `body` in place of a call, running as
    /// `running_as` when that is set.
    pub(crate) fn with_inlined_body<'s>(
        &'s self,
        body: &'s InlinedBody<'s>,
        running_as: Option<&'s Role>,
    ) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.inlined_body = Some(body);
        if running_as.is_some() {
            fields.running_as = running_as;
        }
        fields.into_context(self.scope, self.outer)
    }

    /// The innermost function whose body is being translated in place of a
    /// call.
    pub(crate) const fn inlined_body(&self) -> Option<&InlinedBody<'_>> {
        self.inlined_body
    }

    /// The context the set a helper call returns translates under. A function
    /// body sees none of the query calling it, so its scope, `WITH` clause,
    /// variables, trigger rows, trigger catalog, row locks and declaration
    /// stay out, and the set reads the schema as it stands. The table an
    /// enclosing `INSERT` writes stays in, since the body reads the
    /// statement's snapshot of it too. Boxed so the recursion through a
    /// helper's set holds no copy of it.
    pub(crate) fn helper_body(&self) -> Box<TranslationContext<'_>> {
        Box::new(TranslationContext {
            options: Cow::Borrowed(self.options.as_ref()),
            catalogs: Cow::Borrowed(self.catalogs.as_ref()),
            scope: None,
            outer: None,
            with: None,
            variables: &[],
            schema_is_complete: self.schema_is_complete,
            subqueries_allowed: self.subqueries_allowed,
            written_schema: None,
            history: None,
            running_as: self.running_as,
            inlined_body: self.inlined_body,
            trigger_rows: None,
            trigger_catalog: None,
            locked: &[],
            locks_whole_query: false,
            declaration: None,
            inserting_into: self.inserting_into,
        })
    }

    /// What the batch's translation worked out so far, kept for the schemas
    /// it holds alive.
    pub(crate) fn batch_cache(
        &self,
    ) -> &core::cell::RefCell<crate::impls::batch_cache::BatchCache> {
        &self.catalogs.batch_cache
    }

    /// The same context translating a row trigger whose table and events
    /// `rows` describes.
    pub(crate) fn with_trigger_rows<'s>(&'s self, rows: TriggerRows<'s>) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.trigger_rows = Some(rows);
        fields.into_context(self.scope, self.outer)
    }

    /// The same context translating a query whose locking clauses cover the
    /// `FROM` items at `locked`.
    pub(crate) fn with_locked<'s>(&'s self, locked: &'s [usize]) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.locked = locked;
        fields.locks_whole_query = false;
        fields.into_context(self.scope, self.outer)
    }

    /// Whether the query about to be translated sits in a locked `FROM` item.
    pub(crate) const fn locks_whole_query(&self) -> bool {
        self.locks_whole_query
    }

    /// Whether a row-locking clause covers `factor`.
    pub(crate) fn locks(&self, factor: &sqlparser::ast::TableFactor) -> bool {
        self.locked.contains(&core::ptr::from_ref(factor).addr())
    }

    /// The same context translating the source of an `INSERT` writing the
    /// table whose key is `table`.
    pub(crate) fn with_inserting_into<'s>(&'s self, table: &'s str) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.inserting_into = Some(table);
        fields.into_context(self.scope, self.outer)
    }

    /// The key of the table the `INSERT` whose source is being translated
    /// writes.
    pub(crate) const fn inserting_into(&self) -> Option<&str> {
        self.inserting_into
    }

    /// The same context translating the part of a row trigger that fires for
    /// `events` alone.
    pub(crate) fn with_trigger_events<'s>(
        &'s self,
        events: &[sqlparser::ast::TriggerEvent],
    ) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.trigger_rows = self.trigger_rows.map(|rows| rows.for_events(events));
        fields.into_context(self.scope, self.outer)
    }

    /// The rows of the row trigger being translated, if any.
    pub(crate) const fn trigger_rows(&self) -> Option<TriggerRows<'a>> {
        self.trigger_rows
    }

    /// The whole trigger row `expr` reads, when it is a bare `NEW` or `OLD`
    /// inside a row trigger.
    pub(crate) fn trigger_row(&self, expr: &Expr) -> Option<TriggerRow<'a>> {
        self.trigger_rows.and_then(|rows| rows.row(expr))
    }

    /// Restricts expression translation to a position that forbids subqueries.
    pub(crate) fn without_subqueries(&self) -> TranslationContext<'_> {
        let mut fields = ContextFields::borrowed(self);
        fields.subqueries_allowed = false;
        fields.into_context(self.scope, self.outer)
    }

    #[must_use]
    pub(crate) const fn allows_subqueries(&self) -> bool {
        self.subqueries_allowed
    }

    /// True when `name`, unquoted, is a PL/pgSQL variable rather than a
    /// column.
    #[must_use]
    pub(crate) fn is_variable(&self, name: &str) -> bool {
        self.variables.iter().any(|variable| variable.name.eq_ignore_ascii_case(name))
    }

    /// The declaration of the PL/pgSQL variable `name`, unquoted, or [`None`]
    /// when no variable of that name is bound or its type is not declared in
    /// a form this reads.
    #[must_use]
    pub(crate) fn variable_declaration(&self, name: &str) -> Option<&sqlparser::ast::ColumnDef> {
        self.variables
            .iter()
            .find(|variable| variable.name.eq_ignore_ascii_case(name))
            .and_then(|variable| variable.declaration.as_ref())
    }

    #[must_use]
    pub(crate) const fn schema_is_complete(&self) -> bool {
        self.schema_is_complete
    }

    /// The enclosing query's `WITH` clause, if it has one.
    #[must_use]
    pub(crate) fn cte_clause(&self) -> Option<&sqlparser::ast::With> {
        self.with
    }

    /// The `WITH` clauses of this context and the ones it was derived from,
    /// innermost first.
    pub(crate) fn cte_clauses(&self) -> impl Iterator<Item = &sqlparser::ast::With> {
        core::iter::successors(Some(self), |context| context.outer)
            .filter_map(|context| context.with)
    }

    /// The scopes a reference may resolve against, innermost first.
    ///
    /// A bare name that reads the whole row of a one-column set-returning
    /// function resolves as that column, so a reader of its type sees the
    /// value the translation reads.
    pub(crate) fn column_definitions<'s>(
        &'s self,
        reference: &'s Expr,
        pseudo_row_reference: Option<&'s Expr>,
    ) -> impl Iterator<
        Item = Result<
            Option<ColumnDefinition<'s, 's, 's, ParserDB>>,
            sql_traits::errors::LookupError,
        >,
    > + 's {
        let whole_row = self.whole_row(reference).and_then(WholeRow::column_reference);
        let mut levels = self.levels();
        core::iter::from_fn(move || {
            let reference = whole_row.as_ref().unwrap_or(reference);
            levels.next().map(|scope| Self::resolve_at(scope, reference, pseudo_row_reference))
        })
    }

    /// The scopes of this context and the ones it was derived from, innermost
    /// first.
    fn levels(&self) -> impl Iterator<Item = TranslationScope<'_>> {
        core::iter::successors(Some(self), |context| context.outer)
            .filter_map(|context| context.scope)
    }

    fn resolve_at<'s>(
        scope: TranslationScope<'s>,
        reference: &Expr,
        pseudo_row_reference: Option<&Expr>,
    ) -> Result<Option<ColumnDefinition<'s, 's, 's, ParserDB>>, sql_traits::errors::LookupError>
    {
        match scope {
            TranslationScope::Query { scope, pseudo_row, .. } => {
                scope.resolve_column_definition(
                    pseudo_row_reference.filter(|_| pseudo_row).unwrap_or(reference),
                )
            }
            TranslationScope::Definition(scope) => scope.resolve_column_definition(reference),
        }
    }

    /// The whole row a bare `reference` reads, when it names the relation of a
    /// set-returning JSON function and no column in scope answers it.
    ///
    /// PostgreSQL tries a bare name as a column at every level before it tries
    /// it as a relation, so a column of that name anywhere in scope wins,
    /// measured on 18.6. The innermost relation of that name then decides. An
    /// opaque relation answers every name and so decides nothing, and an
    /// ambiguous name is left to the refusal that reports it.
    pub(crate) fn whole_row(&self, reference: &Expr) -> Option<&WholeRow<'_>> {
        let Expr::Identifier(name) = reference else { return None };
        let named = |rows: &[WholeRow<'_>]| rows.iter().any(|row| row.is_named(name));
        let candidate = self.levels().any(|scope| {
            matches!(scope, TranslationScope::Query { whole_rows, .. } if named(whole_rows))
        });
        if !candidate || (name.quote_style.is_none() && self.is_variable(&name.value)) {
            return None;
        }
        for scope in self.levels() {
            match Self::resolve_at(scope, reference, None) {
                Ok(None | Some(ColumnDefinition::Opaque)) => {}
                Ok(Some(_)) | Err(_) => return None,
            }
        }
        let qualifier = ObjectName::from(vec![name.clone()]);
        self.levels().find_map(|scope| {
            let TranslationScope::Query { scope, whole_rows, .. } = scope else { return None };
            scope.resolve_relation_binding(&qualifier)?;
            Some(whole_rows.iter().find(|row| row.is_named(name)))
        })?
    }

    /// Borrows the recorded output scope for an original query node.
    pub(crate) fn query_scope<'s>(
        &'s self,
        query: &sqlparser::ast::Query,
    ) -> Option<ColumnQueryScope<'s, 's, 's, ParserDB>> {
        let mut context = Some(self);
        while let Some(current) = context {
            if let Some(TranslationScope::Query { scope, .. }) = current.scope
                && let Some(query_scope) = scope.scope_for_query(query)
            {
                return Some(query_scope);
            }
            context = current.outer;
        }
        None
    }

    /// Borrows the nearest original query enclosing an output inspection.
    pub(crate) fn original_query(&self) -> Option<&sqlparser::ast::Query> {
        let mut context = Some(self);
        while let Some(current) = context {
            if let Some(TranslationScope::Query { original_query: Some(query), .. }) = current.scope
            {
                return Some(query);
            }
            context = current.outer;
        }
        None
    }

    /// Records `(table, column)` as having a translated spatial index. Both
    /// are lowercased on insert so later lookups via
    /// [`Self::has_spatial_index`] can match the parser's case-folded
    /// identifiers without re-normalizing. Idempotent: duplicate entries
    /// are filtered out.
    pub(crate) fn add_spatial_index(
        &mut self,
        table: impl Into<String>,
        column: impl Into<String>,
    ) {
        let entry = (table.into().to_ascii_lowercase(), column.into().to_ascii_lowercase());
        if !self.catalogs.spatial_indexes.contains(&entry) {
            self.catalogs.to_mut().spatial_indexes.push(entry);
        }
    }

    /// Returns whether the given `(table, column)` pair was registered as a
    /// spatial index in the same translation unit. Case-insensitive on both
    /// inputs.
    #[must_use]
    pub(crate) fn has_spatial_index(&self, table: &str, column: &str) -> bool {
        let table = table.to_ascii_lowercase();
        let column = column.to_ascii_lowercase();
        self.catalogs.spatial_indexes.iter().any(|(t, c)| *t == table && *c == column)
    }

    /// Records `(table, column)` as having a translated FTS5 index. Same
    /// case-folded, idempotent shape as [`Self::add_spatial_index`].
    pub(crate) fn add_fts_index(&mut self, table: impl Into<String>, column: impl Into<String>) {
        let entry = (table.into().to_ascii_lowercase(), column.into().to_ascii_lowercase());
        if !self.catalogs.fts_indexes.contains(&entry) {
            self.catalogs.to_mut().fts_indexes.push(entry);
        }
    }

    /// Returns whether the given `(table, column)` pair was registered as an
    /// FTS5 index in the same translation unit. Case-insensitive on both
    /// inputs. Drives the `to_tsvector(...) @@ to_tsquery(...)` rewrite gate
    /// in `translate_fts_expression`.
    #[must_use]
    pub(crate) fn has_fts_index(&self, table: &str, column: &str) -> bool {
        let table = table.to_ascii_lowercase();
        let column = column.to_ascii_lowercase();
        self.catalogs.fts_indexes.iter().any(|(t, c)| *t == table && *c == column)
    }

    /// Records `name` as a declared SQLite object name, lowercased and
    /// idempotent so [`Self::has_declared_object_name`] matches
    /// case-insensitively as SQLite does.
    pub(crate) fn add_declared_object_name(&mut self, name: impl Into<String>) {
        let name = name.into().to_ascii_lowercase();
        if !self.catalogs.declared_object_names.contains(&name) {
            self.catalogs.to_mut().declared_object_names.push(name);
        }
    }

    /// Returns whether `name` collides with an object already declared in the
    /// translation unit. Case-insensitive, matching SQLite name resolution.
    #[must_use]
    pub(crate) fn has_declared_object_name(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.catalogs.declared_object_names.contains(&name)
    }

    /// Records `name` as a function executed by a trigger in the translation
    /// unit, lowercased and idempotent to match
    /// [`Self::has_trigger_function_name`].
    pub(crate) fn add_trigger_function_name(&mut self, name: impl Into<String>) {
        let name = name.into().to_ascii_lowercase();
        if !self.catalogs.trigger_function_names.contains(&name) {
            self.catalogs.to_mut().trigger_function_names.push(name);
        }
    }

    /// Returns whether a trigger in the translation unit executes `name`, in
    /// which case the function's body reaches the output inlined in that
    /// trigger and its definition is not lost.
    #[must_use]
    pub(crate) fn has_trigger_function_name(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.catalogs.trigger_function_names.contains(&name)
    }

    /// Records the row triggers sharing a slot with another, by folded table
    /// and trigger name.
    pub(crate) fn set_shared_slot_triggers(
        &mut self,
        triggers: alloc::collections::BTreeSet<(TableKey, String)>,
    ) {
        self.catalogs.to_mut().shared_slot_triggers = triggers;
    }

    /// Returns whether the row trigger keyed by folded `table` and `name`
    /// shares its table, timing and event with another.
    #[must_use]
    pub(crate) fn shares_trigger_slot(&self, key: &(TableKey, String)) -> bool {
        self.catalogs.shared_slot_triggers.contains(key)
    }

    pub(crate) fn add_before_row_trigger_fn(
        &mut self,
        table_name: TableKey,
        fn_name: sqlparser::ast::ObjectName,
        event: crate::impls::translator_impls::reentry::MaintainedEvent,
    ) {
        self.catalogs.to_mut().before_row_trigger_fns.push((table_name, fn_name, event));
    }

    pub(crate) fn before_row_trigger_fns(
        &self,
    ) -> &[(
        TableKey,
        sqlparser::ast::ObjectName,
        crate::impls::translator_impls::reentry::MaintainedEvent,
    )] {
        &self.catalogs.before_row_trigger_fns
    }

    /// Records each trigger function `statement` moves under its new name as
    /// well, since the triggers keep running it.
    pub(crate) fn carry_trigger_functions(&mut self, statement: &sqlparser::ast::Statement) {
        use sqlparser::ast::{AlterFunction, AlterFunctionOperation, AlterFunctionSignature};

        use crate::impls::{
            object_name::last_ident_value_or_display, translator_impls::function_moves::moved,
        };

        let carried: Vec<_> = self
            .catalogs
            .before_row_trigger_fns
            .iter()
            .filter_map(|(table, function, event)| {
                Some((table.clone(), moved(statement, function)?, *event))
            })
            .collect();
        self.catalogs.to_mut().before_row_trigger_fns.extend(carried);
        if let sqlparser::ast::Statement::AlterFunction(AlterFunction {
            signature: AlterFunctionSignature::Function(function),
            operation: AlterFunctionOperation::RenameTo { new_name },
        }) = statement
            && self.has_trigger_function_name(&last_ident_value_or_display(&function.name))
        {
            self.add_trigger_function_name(new_name.value.clone());
        }
    }

    /// Follows the triggers `statement` creates, drops, or carries to a
    /// renamed table, and forgets those of a dropped table.
    pub(crate) fn follow_triggers(&mut self, statement: &sqlparser::ast::Statement) {
        use sqlparser::ast::{AlterTableOperation, ObjectType, RenameTableNameKind, Statement};

        use crate::impls::object_name::last_ident_value_or_display;

        match statement {
            Statement::CreateTrigger(trigger) => self.register_table_trigger(trigger),
            Statement::DropTrigger(drop) => {
                let name = last_ident_value_or_display(&drop.trigger_name);
                let table = drop.table_name.as_ref().map(table_key);
                self.catalogs.to_mut().table_triggers.retain(|kept| {
                    !kept.name.eq_ignore_ascii_case(&name)
                        || table.as_ref().is_some_and(|table| *table != kept.table)
                });
            }
            Statement::Drop { object_type: ObjectType::Table, names, .. } => {
                let dropped: Vec<TableKey> = names.iter().map(table_key).collect();
                self.catalogs.to_mut().table_triggers.retain(|kept| !dropped.contains(&kept.table));
            }
            Statement::AlterTable(alter_table) => {
                for operation in &alter_table.operations {
                    let AlterTableOperation::RenameTable {
                        table_name:
                            RenameTableNameKind::As(renamed) | RenameTableNameKind::To(renamed),
                    } = operation
                    else {
                        continue;
                    };
                    let from = table_key(&alter_table.name);
                    let mut to = table_key(renamed);
                    if renamed.0.len() == 1 {
                        to.temporary = from.temporary;
                    }
                    for kept in &mut self.catalogs.to_mut().table_triggers {
                        if kept.table == from {
                            kept.table.clone_from(&to);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn register_table_trigger(&mut self, trigger: &sqlparser::ast::CreateTrigger) {
        use crate::impls::object_name::last_ident_value_or_display;
        let name = last_ident_value_or_display(&trigger.name);
        let table = table_key(&trigger.table_name);
        let triggers = &mut self.catalogs.to_mut().table_triggers;
        triggers.retain(|kept| kept.table != table || !kept.name.eq_ignore_ascii_case(&name));
        triggers.push(TableTrigger {
            name,
            table,
            events: trigger.events.clone(),
            period: trigger.period,
            row_level: matches!(
                trigger.trigger_object,
                Some(
                    sqlparser::ast::TriggerObjectKind::For(sqlparser::ast::TriggerObject::Row)
                        | sqlparser::ast::TriggerObjectKind::ForEach(
                            sqlparser::ast::TriggerObject::Row
                        )
                )
            ),
        });
    }

    pub(crate) fn seed_table_triggers(&mut self, schema: Option<&ParserDB>) -> Vec<TableTrigger> {
        use sql_traits::traits::DatabaseLike;
        if let Some(schema) = schema {
            for trigger in schema.triggers() {
                self.register_table_trigger(trigger);
            }
        }
        self.catalogs.table_triggers.clone()
    }

    pub(crate) fn with_table_triggers<'s>(
        &'s self,
        triggers: &'s [TableTrigger],
    ) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.trigger_catalog = Some(triggers);
        fields.into_context(self.scope, self.outer)
    }

    pub(crate) fn table_triggers<'s>(
        &'s self,
        table: &'s sqlparser::ast::CreateTable,
    ) -> impl Iterator<Item = &'s TableTrigger> {
        let key = TableKey::from(table);
        self.trigger_catalog
            .unwrap_or(&self.catalogs.table_triggers)
            .iter()
            .filter(move |trigger| trigger.table == key)
    }

    /// The name of a trigger `table` carries that fires on an event `fires`
    /// accepts.
    pub(crate) fn trigger_firing(
        &self,
        table: &TableKey,
        fires: impl Fn(&sqlparser::ast::TriggerEvent) -> bool,
    ) -> Option<&str> {
        self.catalogs
            .table_triggers
            .iter()
            .find(|trigger| trigger.table == *table && trigger.events.iter().any(&fires))
            .map(|trigger| trigger.name.as_str())
    }

    /// Whether an `INSTEAD OF` trigger on the view stored as `view` takes a
    /// write of an event `fires` accepts.
    pub(crate) fn instead_of_trigger_takes(
        &self,
        view: &str,
        fires: impl Fn(&sqlparser::ast::TriggerEvent) -> bool,
    ) -> bool {
        self.catalogs.table_triggers.iter().any(|trigger| {
            trigger.period == Some(sqlparser::ast::TriggerPeriod::InsteadOf)
                && trigger.table.name == view
                && trigger.events.iter().any(&fires)
        })
    }

    pub(crate) fn add_deferred_trigger(
        &mut self,
        trigger: crate::impls::translator_impls::deferred_constraint::DeferredTrigger,
    ) {
        self.catalogs.to_mut().deferred_triggers.push(trigger);
    }

    /// The deferred constraint triggers the translation unit creates, whose
    /// queue every translated `COMMIT` drains first.
    pub(crate) fn deferred_triggers(
        &self,
    ) -> &[crate::impls::translator_impls::deferred_constraint::DeferredTrigger] {
        &self.catalogs.deferred_triggers
    }

    /// The request's zone, resolved against `schema` once per translation,
    /// or why it does not resolve.
    pub(crate) fn request_zone(&self, schema: &ParserDB) -> Result<&RequestZone, &ZoneUnavailable> {
        self.catalogs
            .request_zone
            .get_or_init(|| crate::impls::request_zone::resolve(schema, self.options.as_ref()))
            .as_ref()
    }

    /// The name a call site reads `body` by, a function answering a call
    /// to `helper` with `parameters` under `columns`, recorded the first
    /// time a call needs it.
    ///
    /// The name is `<helper>_<hash>`, the hash that of the function's SQL,
    /// so every translation names the same function alike and a statement
    /// translated on its own registers no other function under a name the
    /// script uses.
    pub(crate) fn record_helper_function(
        &self,
        helper: &str,
        parameters: Vec<String>,
        columns: Vec<String>,
        body: sqlparser::ast::Query,
    ) -> String {
        use core::hash::Hasher as _;

        let body = sqlparser::ast::Statement::Query(Box::new(body));
        let mut hasher = crate::impls::helper_functions::Fnv1a::new();
        hasher.write(body.to_string().as_bytes());
        for name in parameters.iter().chain(&columns) {
            hasher.write(&[0]);
            hasher.write(name.as_bytes());
        }
        let name = format!("{helper}_{:016x}", hasher.finish());
        let mut functions = self.catalogs.helper_functions.borrow_mut();
        if !functions.iter().any(|function| function.name == name) {
            functions.push(crate::script::HelperFunction {
                name: name.clone(),
                parameters,
                columns,
                body,
            });
        }
        name
    }

    /// The helper functions recorded so far.
    pub(crate) fn helper_functions(
        &self,
    ) -> core::cell::Ref<'_, Vec<crate::script::HelperFunction>> {
        self.catalogs.helper_functions.borrow()
    }

    /// The helpers the translation calls as table-valued functions, taken
    /// out of the catalog once the translation is done.
    pub(crate) fn take_helper_functions(&self) -> Vec<crate::script::HelperFunction> {
        core::mem::take(&mut *self.catalogs.helper_functions.borrow_mut())
    }
}

impl core::ops::Deref for TranslationContext<'_> {
    type Target = Pg2SqliteOptions;

    fn deref(&self) -> &Self::Target {
        self.options.as_ref()
    }
}

/// The scope over one declared table's columns, `None` when the table does
/// not resolve.
#[must_use]
pub(crate) fn table_column_scope<'db>(
    table: Option<&'db <ParserDB as DatabaseLike>::Table>,
    schema: &'db ParserDB,
) -> Option<ColumnScope<'db, 'db, ParserDB>> {
    table.map(|table| ColumnScope::for_table(table, schema, IdentifierCase::AsWritten))
}

/// The scope over the relations `query` reads.
pub(crate) fn query_column_scope<'db>(
    query: &'db sqlparser::ast::Query,
    schema: &'db ParserDB,
) -> Result<ColumnScope<'db, 'db, ParserDB>, sql_traits::errors::LookupError> {
    ColumnScope::from_query(query, schema, IdentifierCase::AsWritten)
}

impl Pg2SqliteOptions {
    #[must_use]
    /// Drops check constraints that call unsupported functions.
    pub fn with_remove_unsupported_check_constraints(mut self) -> Self {
        self.remove_unsupported_check_constraints = true;
        self
    }

    #[must_use]
    /// Returns whether unsupported check constraints are dropped.
    pub fn is_remove_unsupported_check_constraints_enabled(&self) -> bool {
        self.remove_unsupported_check_constraints
    }

    #[must_use]
    /// Sets how translated UUID columns are stored.
    pub fn with_uuid_representation(mut self, representation: UuidRepresentation) -> Self {
        self.uuid_representation = Some(representation);
        self
    }

    #[must_use]
    /// Returns the configured UUID storage representation.
    pub fn get_uuid_representation(&self) -> Option<UuidRepresentation> {
        self.uuid_representation
    }

    #[must_use]
    /// Sets the destination random UUID function, whose return type must match
    /// the storage representation.
    pub fn with_uuid_function_name(mut self, name: impl Into<String>) -> Self {
        self.uuid_function_name = name.into();
        self
    }

    #[must_use]
    /// Returns the destination random UUID function name.
    pub fn get_uuid_function_name(&self) -> &str {
        &self.uuid_function_name
    }

    #[must_use]
    /// Sets the destination version 7 UUID function.
    pub fn with_uuid_v7_function_name(mut self, name: impl Into<String>) -> Self {
        self.uuid_v7_function_name = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured version 7 UUID function name.
    pub fn get_uuid_v7_function_name(&self) -> Option<&str> {
        self.uuid_v7_function_name.as_deref()
    }

    #[must_use]
    /// Sets the function that converts UUID text to a 16 byte blob.
    pub fn with_uuid_text_to_blob_function_name(mut self, name: impl Into<String>) -> Self {
        self.uuid_text_to_blob_function_name = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured UUID text to blob function name.
    pub fn get_uuid_text_to_blob_function_name(&self) -> Option<&str> {
        self.uuid_text_to_blob_function_name.as_deref()
    }

    #[must_use]
    /// Sets the functions that read a zone whose UTC offset changes.
    ///
    /// PostgreSQL turns an instant into a wall clock and back, and adds the
    /// months and days of an interval to a `timestamptz`, in the request's
    /// `TimeZone`, which the deployment passed with
    /// [`with_deployment`](Self::with_deployment) resolves, for the acting
    /// role named with [`with_acting_role`](Self::with_acting_role) or,
    /// without one, as the connection has it before any `SET ROLE`. In UTC
    /// and in a zone of one fixed offset the translation computes all of it
    /// with SQLite's own date functions. A zone whose offset changes, such as
    /// `Europe/Rome`, needs these two functions, and what reads that zone is
    /// refused without them. [`TimeZoneFunctions`] states what each one
    /// answers.
    pub fn with_time_zone_functions(mut self, functions: TimeZoneFunctions) -> Self {
        self.time_zone_functions = Some(functions);
        self
    }

    #[must_use]
    /// Returns the configured zone functions.
    pub fn get_time_zone_functions(&self) -> Option<&TimeZoneFunctions> {
        self.time_zone_functions.as_ref()
    }

    #[must_use]
    /// Names the SQLite functions the replica registers to fold case as the
    /// server does.
    ///
    /// A `lower` or `upper` call, an `ILIKE`, and a policy reading a setting
    /// through `lower`, over anything but ASCII text or a text collated `C`
    /// or `POSIX`, folds by the server's `LC_CTYPE`, which SQLite's own
    /// `lower` and `upper` answer only for ASCII letters. Each such fold calls
    /// these functions, and is refused without them. [`CaseFoldFunctions`]
    /// states what each one answers.
    pub fn with_case_fold_functions(mut self, functions: CaseFoldFunctions) -> Self {
        self.case_fold_functions = Some(functions);
        self
    }

    #[must_use]
    /// Returns the configured case fold functions.
    pub fn get_case_fold_functions(&self) -> Option<&CaseFoldFunctions> {
        self.case_fold_functions.as_ref()
    }

    #[must_use]
    /// Names the SQLite functions and collation the replica registers for
    /// `jsonb`.
    ///
    /// The replica stores every `jsonb` value as the text PostgreSQL prints
    /// for it. A literal is brought to that text during translation, while a
    /// bound parameter, a cast from `text` or `json`, and a value computed by
    /// a `jsonb` function or operator go through the canonicalising function
    /// at run time. `->>`, `#>>`, `json_extract_path_text` and
    /// `jsonb_extract_path_text`, over a column or a field of a JSON session
    /// setting, read the value through the text function, which answers the
    /// text PostgreSQL extracts. PostgreSQL compares `jsonb` by value, so
    /// `'1.0'::jsonb = '1'::jsonb`, and every comparison, `UNIQUE`
    /// constraint, index, `DISTINCT`, `GROUP BY` and `ORDER BY` over `jsonb`
    /// names the collation. A `jsonb` column, and anything else needing any
    /// of them, is refused without them. [`JsonbFunctions`] states what each
    /// one answers.
    pub fn with_jsonb_functions(mut self, functions: JsonbFunctions) -> Self {
        self.jsonb_functions = Some(functions);
        self
    }

    #[must_use]
    /// Returns the configured `jsonb` functions and collation.
    pub fn get_jsonb_functions(&self) -> Option<&JsonbFunctions> {
        self.jsonb_functions.as_ref()
    }

    #[must_use]
    /// Sets how PostgreSQL arrays are represented in SQLite.
    pub fn with_array_representation(mut self, representation: ArrayRepresentation) -> Self {
        self.array_representation = Some(representation);
        self
    }

    #[must_use]
    /// Returns the configured array representation.
    pub fn get_array_representation(&self) -> Option<ArrayRepresentation> {
        self.array_representation
    }

    #[must_use]
    /// Sets the suffix of the backing table behind a view, which row-level
    /// security and the acting role's grants put there.
    pub fn with_rls_table_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.rls_table_suffix = suffix.into();
        self
    }

    #[must_use]
    /// Returns the suffix of the backing table behind a view.
    pub fn get_rls_table_suffix(&self) -> &str {
        &self.rls_table_suffix
    }

    #[must_use]
    /// Sets the marker used in the names of the triggers denying a plain
    /// table's writes to the acting role.
    pub fn with_readonly_deny_trigger_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.readonly_deny_trigger_suffix = suffix.into();
        self
    }

    #[must_use]
    /// Returns the marker used in the names of the triggers denying a plain
    /// table's writes to the acting role.
    pub fn get_readonly_deny_trigger_suffix(&self) -> &str {
        &self.readonly_deny_trigger_suffix
    }

    #[must_use]
    /// Names the deployment acting role this translation runs as, whose grants
    /// decide which tables, columns and writes reach the replica, which
    /// policies apply, and which `current_user`, `current_role` and `user`
    /// answer.
    ///
    /// The role has to be one of the acting roles of the deployment passed
    /// with [`with_deployment`](Self::with_deployment), declared in the
    /// schema.
    pub fn with_acting_role(mut self, role: impl Into<String>) -> Self {
        self.acting_role = Some(role.into());
        self
    }

    #[must_use]
    /// Returns the acting role this translation runs as.
    pub fn get_acting_role(&self) -> Option<&str> {
        self.acting_role.as_deref()
    }

    #[must_use]
    /// Names the role the schema's statements run as, as a migration runs as
    /// one role no statement names. That role owns every table and view
    /// created before a `SET ROLE` moves away from it, so row-level security
    /// exempts it from those tables' policies and reads a plain view as it,
    /// and an `ALTER DEFAULT PRIVILEGES` without `FOR ROLE` gives its defaults
    /// to what it creates afterwards.
    pub fn with_schema_owner_role(mut self, role: impl Into<String>) -> Self {
        self.schema_owner_role = Some(role.into());
        self
    }

    #[must_use]
    /// Returns the role that ran the schema, if one is named.
    pub fn get_schema_owner_role(&self) -> Option<&str> {
        self.schema_owner_role.as_deref()
    }

    #[must_use]
    /// Pairs a session setting the deployment declares with the SQLite
    /// function the replica answers it with.
    pub fn with_session_variable(mut self, mapping: SessionVariableMapping) -> Self {
        self.session_variables.push(mapping);
        self
    }

    #[must_use]
    /// Returns the configured session variable mappings.
    pub fn get_session_variables(&self) -> &[SessionVariableMapping] {
        &self.session_variables
    }

    #[must_use]
    /// Returns the last mapping for `setting`, compared without case as
    /// PostgreSQL compares setting names.
    pub fn find_session_variable(&self, setting: &str) -> Option<&SessionVariableMapping> {
        self.session_variables.iter().rev().find(|m| m.setting.eq_ignore_ascii_case(setting))
    }

    #[must_use]
    /// Sets the deployment whose requests the translation serves.
    ///
    /// The deployment declares the login role `session_user` answers, the
    /// acting roles [`with_acting_role`](Self::with_acting_role) chooses
    /// from, and every session setting a statement reads: its type, how a set
    /// is packed, the fields of a JSON document and whether a request may
    /// leave it unset. A statement reading a setting or a role is refused
    /// without one. Its server zone, from
    /// [`Deployment::with_default_time_zone`], is the zone the session running
    /// the schema starts in, which reads a `timestamptz` literal of a policy.
    pub fn with_deployment(mut self, deployment: Deployment) -> Self {
        self.deployment = Some(deployment);
        self
    }

    #[must_use]
    /// Returns the deployment the translation serves.
    pub fn get_deployment(&self) -> Option<&Deployment> {
        self.deployment.as_ref()
    }

    #[must_use]
    /// Sets the table that records RLS validation results.
    pub fn with_rls_audit_table_name(mut self, name: impl Into<String>) -> Self {
        self.rls_audit_table_name = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured RLS audit table name.
    pub fn get_rls_audit_table_name(&self) -> Option<&str> {
        self.rls_audit_table_name.as_deref()
    }

    #[must_use]
    /// Sets the boolean function marking a statement as one applying the
    /// server's rows.
    ///
    /// While it answers true, row-level security binds neither the
    /// statement's writes nor its reads, as it binds neither for PostgreSQL's
    /// table owner. The generated write guards and grant gates let the write
    /// through, and a row-secured view shows every row of its backing table,
    /// still projecting only the columns the acting role may read. No `BEFORE`
    /// or `AFTER` trigger translated from a `CREATE TRIGGER` fires, constraint
    /// triggers and deferred ones included, as none fires where a PostgreSQL
    /// subscriber applies replicated rows with `session_replication_role =
    /// replica`, so a delivered row is stored as the server sent it.
    pub fn with_rls_exemption_function(mut self, name: impl Into<String>) -> Self {
        self.rls_exemption_function = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured RLS exemption function name.
    pub fn get_rls_exemption_function(&self) -> Option<&str> {
        self.rls_exemption_function.as_deref()
    }

    #[must_use]
    /// Makes RLS validation errors fatal and redirects supported returning
    /// inserts.
    pub fn with_strict_rls_validation(mut self) -> Self {
        self.strict_rls_validation = true;
        self
    }

    #[must_use]
    /// Returns whether strict RLS validation is enabled.
    pub fn is_strict_rls_validation(&self) -> bool {
        self.strict_rls_validation
    }

    #[must_use]
    /// Makes policy denied updates and deletes raise instead of affecting no
    /// rows.
    pub fn with_strict_rls_write_deny(mut self) -> Self {
        self.strict_rls_write_deny = true;
        self
    }

    #[must_use]
    /// Returns whether policy denied updates and deletes raise.
    pub fn is_strict_rls_write_deny(&self) -> bool {
        self.strict_rls_write_deny
    }

    #[must_use]
    /// Enables functions supplied by the SQLiteGIS extension.
    pub fn with_sqlitegis_enabled(mut self) -> Self {
        self.sqlitegis_enabled = true;
        self
    }

    #[must_use]
    /// Returns whether SQLiteGIS function translation is enabled.
    pub fn is_sqlitegis_enabled(&self) -> bool {
        self.sqlitegis_enabled
    }

    #[must_use]
    /// Declares that the destination provides the optional SQLite math
    /// functions.
    pub fn with_math_functions_available(mut self) -> Self {
        self.math_functions_available = true;
        self
    }

    #[must_use]
    /// Returns whether the destination provides SQLite math functions.
    pub fn is_math_functions_available(&self) -> bool {
        self.math_functions_available
    }

    #[must_use]
    /// Declares function names that the destination provides.
    pub fn with_user_defined_functions<S: Into<String>>(
        mut self,
        names: impl IntoIterator<Item = S>,
    ) -> Self {
        self.user_defined_functions
            .extend(names.into_iter().map(|name| name.into().to_ascii_lowercase()));
        self
    }

    #[must_use]
    /// Returns whether the destination declares the function name.
    pub fn declares_user_defined_function(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.user_defined_functions.contains(&name)
    }

    #[must_use]
    /// Places `tables` in `schema` and emits cross-database views and triggers
    /// as `TEMP`.
    ///
    /// ```
    /// # use pg2sqlite::prelude::*;
    /// let options = Pg2SqliteOptions::default().with_attached_tables("shared", ["notes"]);
    /// let script = Pg2Sqlite::default()
    ///     .sql(
    ///         "CREATE TABLE owners (id INT PRIMARY KEY);
    ///          CREATE TABLE notes (id INT PRIMARY KEY, owner INT NOT NULL);
    ///          CREATE VIEW owned AS SELECT n.id FROM notes n JOIN owners o ON o.id = n.owner;",
    ///     )?
    ///     .translate_to_sql(&options)?;
    /// for statement in script.initialize(FreshFiles::Both)? {
    ///     println!("{statement}");
    /// }
    /// # Ok::<(), Error>(())
    /// ```
    pub fn with_attached_tables<S: Into<String>>(
        mut self,
        schema: impl Into<String>,
        tables: impl IntoIterator<Item = S>,
    ) -> Self {
        self.attached_tables = Some(AttachedTables {
            schema: schema.into(),
            tables: tables.into_iter().map(Into::into).collect(),
        });
        self
    }

    #[must_use]
    /// Returns the tables placed in an attached database, if any.
    pub fn get_attached_tables(&self) -> Option<&AttachedTables> {
        self.attached_tables.as_ref()
    }

    #[must_use]
    /// Marks the writes the server derives, for a replica that uploads the
    /// rows its transactions change to a PostgreSQL that runs its own
    /// triggers and cascades when it applies them.
    ///
    /// Each run of statements a `BEFORE` or `AFTER` trigger runs that write
    /// rows, directly or in a function it calls, runs between `SELECT name(1)`
    /// and `SELECT name(-1)`. The consumer registers `name` as a
    /// non-deterministic SQLite function, so SQLite calls it at each mark. The
    /// replica writes those rows, so its policies, checks and triggers judge
    /// later writes on the rows PostgreSQL's would. The server derives them
    /// again from the uploaded rows, so the consumer keeps them out of the
    /// upload. A consumer recording with a SQLite session suspends the
    /// recording while the arguments it was called with add up to more than
    /// zero. A statement that fails between the two calls never makes the
    /// second, so the consumer sets the sum back to zero once each statement
    /// ends. Rows written this way are not undone by inverting the uploaded
    /// changes, and the consumer drops them itself when the server refuses the
    /// upload. An `INSTEAD OF` trigger writes the view's own rows, which stay
    /// unmarked. A trigger SQLite cannot run is refused as without this
    /// option.
    ///
    /// A foreign key whose `ON DELETE` or `ON UPDATE` action is `CASCADE`,
    /// `SET NULL` or `SET DEFAULT` is left out whole, since SQLite runs the
    /// action itself where no call can mark it, and kept as `NO ACTION` it
    /// would refuse an offline delete of a parent whose children the server
    /// removes.
    /// [`Pg2Sqlite::translate_with_report`](crate::pg2sqlite::Pg2Sqlite::translate_with_report)
    /// names each such key in a
    /// [`TranslationWarning::LeftToServer`](crate::warnings::TranslationWarning::LeftToServer),
    /// and each policy, `CHECK`, trigger, unique key or index and foreign key
    /// the replica keeps that reads what its action changes, which it judges
    /// on the rows as they were before the action until the server's rows
    /// arrive, in a
    /// [`TranslationWarning::ReadsLeftToServer`](crate::warnings::TranslationWarning::ReadsLeftToServer).
    ///
    /// ```
    /// # use pg2sqlite::prelude::*;
    /// let options = Pg2SqliteOptions::default().with_server_derived_write_function("derived");
    /// let script = Pg2Sqlite::default()
    ///     .sql(
    ///         "CREATE TABLE notes (id INT PRIMARY KEY);
    ///          CREATE TABLE note_events (note INT NOT NULL);
    ///          CREATE FUNCTION log_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
    ///          BEGIN
    ///            INSERT INTO note_events VALUES (NEW.id);
    ///            RETURN NULL;
    ///          END $$;
    ///          CREATE TRIGGER notes_logged AFTER INSERT ON notes
    ///            FOR EACH ROW EXECUTE FUNCTION log_note();",
    ///     )?
    ///     .translate_to_sql(&options)?;
    /// let trigger = script.iter().find(|statement| statement.contains("CREATE TRIGGER")).unwrap();
    /// assert!(trigger.contains("derived(1)") && trigger.contains("derived(-1)"));
    /// # Ok::<(), Error>(())
    /// ```
    pub fn with_server_derived_write_function(mut self, name: impl Into<String>) -> Self {
        self.server_derived_write_function = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the function the writes the server derives run between calls
    /// of.
    pub fn get_server_derived_write_function(&self) -> Option<&str> {
        self.server_derived_write_function.as_deref()
    }

    #[must_use]
    /// Leaves out the `CHECK` that holds each column stored as text for a
    /// type PostgreSQL bounds to the text the translation writes for it: a
    /// `uuid` under [`UuidRepresentation::Text`], a `date`, `time`, `timetz`,
    /// `timestamp` or `timestamptz`, and a `json` or `jsonb` document.
    ///
    /// Such a column then takes any text, so a bound parameter or a row
    /// applied directly may hold a value PostgreSQL refuses for the type,
    /// such as `not a uuid`, `yesterday-ish` or `{not json`, and a replica
    /// that uploads it has it refused by the server. Every other declared
    /// bound stays.
    ///
    /// ```
    /// # use pg2sqlite::prelude::*;
    /// let options = Pg2SqliteOptions::default().with_stored_text_checks_omitted();
    /// let script = Pg2Sqlite::default()
    ///     .sql("CREATE TABLE events (id BIGINT PRIMARY KEY, at TIMESTAMPTZ, doc JSON);")?
    ///     .translate_to_sql(&options)?;
    /// assert!(script.iter().all(|statement| !statement.contains("CHECK")));
    /// # Ok::<(), Error>(())
    /// ```
    pub fn with_stored_text_checks_omitted(mut self) -> Self {
        self.stored_text_checks_omitted = true;
        self
    }

    #[must_use]
    /// Returns whether the columns stored as text go without the `CHECK`
    /// holding them to the text the translation writes.
    pub fn is_stored_text_checks_omitted(&self) -> bool {
        self.stored_text_checks_omitted
    }

    #[must_use]
    /// Leaves every foreign key to the server, for a replica holding a
    /// partial copy of its rows, where a child may arrive without its parent
    /// and a SQLite key would refuse writes PostgreSQL accepted.
    ///
    /// The translation still reads each key the schema declares, which is
    /// what tells the arms of a helper's grant walk apart. The script then
    /// declares no foreign key, keeps no trigger standing in for the
    /// referenced side of one, and runs none of their `ON DELETE` or
    /// `ON UPDATE` actions, which the server runs when it applies the
    /// upload.
    ///
    /// [`Pg2Sqlite::translate_with_report`](crate::pg2sqlite::Pg2Sqlite::translate_with_report)
    /// names each one in a
    /// [`TranslationWarning::LeftToServer`](crate::warnings::TranslationWarning::LeftToServer).
    ///
    /// ```
    /// # use pg2sqlite::prelude::*;
    /// let options = Pg2SqliteOptions::default().with_foreign_keys_omitted();
    /// let script = Pg2Sqlite::default()
    ///     .sql(
    ///         "CREATE TABLE folders (id INT PRIMARY KEY);
    ///          CREATE TABLE files (id INT PRIMARY KEY, folder INT REFERENCES folders (id));",
    ///     )?
    ///     .translate_to_sql(&options)?;
    /// assert!(script.iter().all(|statement| !statement.contains("REFERENCES")));
    /// # Ok::<(), Error>(())
    /// ```
    pub fn with_foreign_keys_omitted(mut self) -> Self {
        self.foreign_keys_omitted = true;
        self
    }

    #[must_use]
    /// Returns whether every foreign key is left to the server.
    pub fn is_foreign_keys_omitted(&self) -> bool {
        self.foreign_keys_omitted
    }

    #[must_use]
    /// Names the major version of the PostgreSQL server the replica stands
    /// in for, such as `18`.
    ///
    /// What the server answers differently across majors follows it, and a
    /// statement whose translation depends on the major is refused without
    /// it. A `RESTRICT` foreign key into rows a policy may hide from the
    /// acting role refuses a delete or rekey with `restrict_violation`
    /// (`23001`) from PostgreSQL 18 on, and with `foreign_key_violation`
    /// (`23503`) before it.
    pub fn with_postgres_major(mut self, major: u16) -> Self {
        self.postgres_major = Some(major);
        self
    }

    #[must_use]
    /// Returns the configured PostgreSQL major version.
    pub fn get_postgres_major(&self) -> Option<u16> {
        self.postgres_major
    }
}

/// Options acting as `role`, a deployment that logs in and acts as it alone.
#[cfg(all(test, feature = "std"))]
pub(crate) fn acting_as(role: &str) -> Pg2SqliteOptions {
    let deployment = Deployment::new(role, [role], []).expect("one acting role");
    Pg2SqliteOptions::default().with_deployment(deployment).with_acting_role(role)
}

#[cfg(all(test, feature = "arbitrary"))]
mod tests {
    use arbitrary::{Arbitrary, Unstructured};

    use super::Pg2SqliteOptions;

    /// Fuzzed options never carry a deployment, so a fuzzed acting role is
    /// refused rather than trusted.
    #[test]
    fn a_fuzzed_acting_role_meets_the_deployment_refusal() {
        let options = (0..=u8::MAX)
            .filter_map(|seed| {
                Pg2SqliteOptions::arbitrary(&mut Unstructured::new(&[seed; 512])).ok()
            })
            .find(|options| options.get_acting_role().is_some())
            .expect("a seed naming an acting role");
        let schema = sql_traits::structs::ParserDB::from_statements(Vec::new(), "fuzz".to_string())
            .expect("an empty schema");
        let error = crate::impls::session_variable::acting_role(&schema, &options)
            .expect_err("no deployment declares the role");
        assert!(matches!(error, crate::errors::Error::DeploymentRequired { .. }), "{error}");
    }
}
