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
        },
    },
    traits::{
        ArrayRepresentation, JsonbFunctions, SessionVariableMapping, TimeZoneFunctions,
        UuidRepresentation,
    },
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
    /// The host-registered SQLite function and collation that canonicalise
    /// and compare `jsonb`. `None` refuses what needs either. Set via
    /// `with_jsonb_functions`.
    jsonb_functions: Option<JsonbFunctions>,
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
    /// The caller-registered function that exempts generated write guards.
    write_exemption_function: Option<String>,
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
    /// The name of the SQLite UDF to use for ILIKE case folding. When Some,
    /// both sides of an ILIKE expression are wrapped in this function rather
    /// than `lower()`. This enables Unicode-aware case folding for non-ASCII
    /// patterns. Set via `with_ilike_fold_function`.
    ilike_fold_function: Option<String>,
    /// Tables placed in an attached database. Set via `with_attached_tables`.
    attached_tables: Option<AttachedTables>,
    /// Whether the triggers that write rows and the foreign keys that change
    /// child rows are left to the server. Set via
    /// `with_server_derived_writes_omitted`.
    server_derived_writes_omitted: bool,
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
            array_representation: Option::<ArrayRepresentation>::arbitrary(u)?,
            rls_table_suffix: String::arbitrary(u)?,
            readonly_deny_trigger_suffix: String::arbitrary(u)?,
            acting_role: Option::<String>::arbitrary(u)?,
            schema_owner_role: Option::<String>::arbitrary(u)?,
            session_variables: Vec::<SessionVariableMapping>::arbitrary(u)?,
            deployment: None,
            rls_audit_table_name: Option::<String>::arbitrary(u)?,
            write_exemption_function: Option::<String>::arbitrary(u)?,
            strict_rls_validation: bool::arbitrary(u)?,
            strict_rls_write_deny: bool::arbitrary(u)?,
            sqlitegis_enabled: bool::arbitrary(u)?,
            math_functions_available: bool::arbitrary(u)?,
            user_defined_functions: Vec::<String>::arbitrary(u)?,
            ilike_fold_function: Option::<String>::arbitrary(u)?,
            attached_tables: Option::<AttachedTables>::arbitrary(u)?,
            server_derived_writes_omitted: bool::arbitrary(u)?,
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
            array_representation: None,
            rls_table_suffix: "_rls".to_string(),
            readonly_deny_trigger_suffix: "__readonly".to_string(),
            acting_role: None,
            schema_owner_role: None,
            session_variables: Vec::new(),
            deployment: None,
            rls_audit_table_name: None,
            write_exemption_function: None,
            strict_rls_validation: false,
            strict_rls_write_deny: false,
            sqlitegis_enabled: false,
            math_functions_available: false,
            user_defined_functions: Vec::new(),
            ilike_fold_function: None,
            attached_tables: None,
            server_derived_writes_omitted: false,
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
    /// The names of the triggers left to the server, whose `DROP TRIGGER`
    /// finds nothing on the replica.
    server_derived_trigger_names: Vec<String>,
    /// The triggers the schema holds once the translation unit has run,
    /// which the server fires around the writes it derives.
    firing_triggers: Vec<crate::impls::derived_writes::FiringTrigger>,
    /// The folded table and trigger names of the row triggers sharing a
    /// table, timing and event with another, whose `WHEN` moves into the body
    /// so the slot can run them as one SQLite trigger.
    shared_slot_triggers: alloc::collections::BTreeSet<(String, String)>,
    /// The table name, lower-cased, and the function name of every `BEFORE
    /// INSERT` trigger, which the RLS backing-table update check reads to
    /// exempt the crate's own maintenance write. Only those two strings are
    /// kept, since holding whole trigger nodes here costs link-time drop glue
    /// out of proportion to the two fields the check needs.
    before_insert_trigger_fns: Vec<(String, sqlparser::ast::ObjectName)>,
    /// The triggers each table carries after the statements translated so
    /// far, which a write left off the replica with its table reads to find
    /// one PostgreSQL fires for it.
    table_triggers: Vec<TableTrigger>,
    /// The deferred constraint triggers the translation unit creates.
    deferred_triggers: Vec<crate::impls::translator_impls::deferred_constraint::DeferredTrigger>,
    /// The request's zone, resolved the first time an operation needs it.
    request_zone: core::cell::OnceCell<Result<RequestZone, ZoneUnavailable>>,
}

/// A trigger on a table.
#[derive(Clone)]
struct TableTrigger {
    /// The trigger's name as written.
    name: String,
    /// The table's name, lower-cased.
    table: String,
    /// The events the trigger fires on.
    events: Vec<sqlparser::ast::TriggerEvent>,
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
    /// The body a trigger translated here executes in place of its function's
    /// stored one, set when a replay renames what the body references.
    rebound_body: Option<&'a sqlparser_plpgsql::embedded::Block>,
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
    rebound_body: Option<&'a sqlparser_plpgsql::embedded::Block>,
    written_schema: Option<&'a ParserDB>,
    history: Option<StatementHistory<'a>>,
    running_as: Option<&'a Role>,
    inlined_body: Option<&'a InlinedBody<'a>>,
    trigger_rows: Option<TriggerRows<'a>>,
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
            rebound_body: context.rebound_body,
            written_schema: context.written_schema,
            history: context.history,
            running_as: context.running_as,
            inlined_body: context.inlined_body,
            trigger_rows: context.trigger_rows,
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
            rebound_body: self.rebound_body,
            written_schema: self.written_schema,
            history: self.history,
            running_as: self.running_as,
            inlined_body: self.inlined_body,
            trigger_rows: self.trigger_rows,
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
            rebound_body: None,
            written_schema: None,
            history: None,
            running_as: None,
            inlined_body: None,
            trigger_rows: None,
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
            rebound_body: None,
            written_schema: None,
            history: None,
            running_as: None,
            inlined_body: None,
            trigger_rows: None,
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

    /// The same context noting `with` as the enclosing query's CTE clause.
    pub(crate) fn with_cte_clause<'s>(
        &'s self,
        with: Option<&'s sqlparser::ast::With>,
    ) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.with = with;
        fields.into_context(self.scope, self.outer)
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

    /// The same context translating a trigger with `body` in place of its
    /// function's stored body.
    pub(crate) fn with_rebound_body<'s>(
        &'s self,
        body: &'s sqlparser_plpgsql::embedded::Block,
    ) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.rebound_body = Some(body);
        fields.into_context(self.scope, self.outer)
    }

    /// The body a trigger translated here executes in place of its
    /// function's stored one.
    pub(crate) const fn rebound_body(&self) -> Option<&sqlparser_plpgsql::embedded::Block> {
        self.rebound_body
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

    /// The same context translating a row trigger whose table and events
    /// `rows` describes.
    pub(crate) fn with_trigger_rows<'s>(&'s self, rows: TriggerRows<'s>) -> TranslationContext<'s> {
        let mut fields = ContextFields::borrowed(self);
        fields.trigger_rows = Some(rows);
        fields.into_context(self.scope, self.outer)
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
        triggers: alloc::collections::BTreeSet<(String, String)>,
    ) {
        self.catalogs.to_mut().shared_slot_triggers = triggers;
    }

    /// Returns whether the row trigger keyed by folded `table` and `name`
    /// shares its table, timing and event with another.
    #[must_use]
    pub(crate) fn shares_trigger_slot(&self, key: &(String, String)) -> bool {
        self.catalogs.shared_slot_triggers.contains(key)
    }

    /// Registers `name` as a trigger the replica leaves to the server.
    pub(crate) fn add_server_derived_trigger_name(&mut self, name: &str) {
        self.catalogs.to_mut().server_derived_trigger_names.push(name.to_owned());
    }

    /// Returns whether a trigger named `name` was left to the server.
    #[must_use]
    pub(crate) fn is_server_derived_trigger_name(&self, name: &str) -> bool {
        self.catalogs.server_derived_trigger_names.iter().any(|left| left == name)
    }

    /// Records the triggers the schema holds once the translation unit has
    /// run.
    pub(crate) fn set_firing_triggers(
        &mut self,
        triggers: Vec<crate::impls::derived_writes::FiringTrigger>,
    ) {
        self.catalogs.to_mut().firing_triggers = triggers;
    }

    /// The triggers the schema holds once the translation unit has run.
    #[must_use]
    pub(crate) fn firing_triggers(&self) -> &[crate::impls::derived_writes::FiringTrigger] {
        &self.catalogs.firing_triggers
    }

    pub(crate) fn add_before_insert_trigger_fn(
        &mut self,
        table_name: String,
        fn_name: sqlparser::ast::ObjectName,
    ) {
        self.catalogs.to_mut().before_insert_trigger_fns.push((table_name, fn_name));
    }

    pub(crate) fn before_insert_trigger_fns(&self) -> &[(String, sqlparser::ast::ObjectName)] {
        &self.catalogs.before_insert_trigger_fns
    }

    /// Follows the triggers `statement` creates, drops, or carries to a
    /// renamed table, and forgets those of a dropped table.
    pub(crate) fn follow_triggers(&mut self, statement: &sqlparser::ast::Statement) {
        use sqlparser::ast::{AlterTableOperation, ObjectType, RenameTableNameKind, Statement};

        use crate::impls::object_name::last_ident_value_or_display;

        let table_key = |name: &ObjectName| last_ident_value_or_display(name).to_lowercase();
        match statement {
            Statement::CreateTrigger(trigger) => {
                let name = last_ident_value_or_display(&trigger.name);
                let table = table_key(&trigger.table_name);
                let triggers = &mut self.catalogs.to_mut().table_triggers;
                triggers
                    .retain(|kept| kept.table != table || !kept.name.eq_ignore_ascii_case(&name));
                triggers.push(TableTrigger { name, table, events: trigger.events.clone() });
            }
            Statement::DropTrigger(drop) => {
                let name = last_ident_value_or_display(&drop.trigger_name);
                let table = drop.table_name.as_ref().map(table_key);
                self.catalogs.to_mut().table_triggers.retain(|kept| {
                    !kept.name.eq_ignore_ascii_case(&name)
                        || table.as_ref().is_some_and(|table| *table != kept.table)
                });
            }
            Statement::Drop { object_type: ObjectType::Table, names, .. } => {
                let dropped: Vec<String> = names.iter().map(table_key).collect();
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
                    let (from, to) = (table_key(&alter_table.name), table_key(renamed));
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

    /// The name of a trigger `table` carries that fires on an event `fires`
    /// accepts, among those the replica does not leave to the server.
    pub(crate) fn trigger_firing(
        &self,
        table: &str,
        fires: impl Fn(&sqlparser::ast::TriggerEvent) -> bool,
    ) -> Option<&str> {
        let table = table.to_lowercase();
        self.catalogs
            .table_triggers
            .iter()
            .find(|trigger| {
                trigger.table == table
                    && trigger.events.iter().any(&fires)
                    && !self.is_server_derived_trigger_name(&trigger.name)
            })
            .map(|trigger| trigger.name.as_str())
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
    /// Names the SQLite function and collation the replica registers for
    /// `jsonb`.
    ///
    /// The replica stores every `jsonb` value as the text PostgreSQL prints
    /// for it. A literal is brought to that text during translation, while a
    /// bound parameter, a cast from `text` or `json`, and a value computed by
    /// a `jsonb` function or operator go through the function at run time.
    /// PostgreSQL compares `jsonb` by value, so `'1.0'::jsonb = '1'::jsonb`,
    /// and every comparison, `UNIQUE` constraint, index, `DISTINCT`, `GROUP
    /// BY` and `ORDER BY` over `jsonb` names the collation. A `jsonb` column,
    /// and anything else needing either one, is refused without them.
    /// [`JsonbFunctions`] states what each one answers.
    pub fn with_jsonb_functions(mut self, functions: JsonbFunctions) -> Self {
        self.jsonb_functions = Some(functions);
        self
    }

    #[must_use]
    /// Returns the configured `jsonb` function and collation.
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
    /// Sets the boolean function that exempts generated write guards.
    ///
    /// Constraint triggers skip an exempt write too, deferred ones included.
    pub fn with_write_exemption_function(mut self, name: impl Into<String>) -> Self {
        self.write_exemption_function = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured write exemption function name.
    pub fn get_write_exemption_function(&self) -> Option<&str> {
        self.write_exemption_function.as_deref()
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
    /// Sets the Unicode aware case folding function used for ILIKE.
    pub fn with_ilike_fold_function(mut self, name: impl Into<String>) -> Self {
        self.ilike_fold_function = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured ILIKE case folding function name.
    pub fn get_ilike_fold_function(&self) -> Option<&str> {
        self.ilike_fold_function.as_deref()
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
    /// Leaves out the writes the server derives, for a replica that uploads
    /// every row its transactions change to a PostgreSQL that runs its own
    /// triggers and cascades when it applies them.
    ///
    /// A `BEFORE` or `AFTER` trigger whose function runs `INSERT`, `UPDATE`,
    /// `DELETE`, `MERGE` or `TRUNCATE`, directly or in a function it calls,
    /// stays without those statements, since the server derives those rows
    /// again from the uploaded one, and keeps its refusals and its reads. It
    /// is left out whole when nothing else is left of it, when it assigns to
    /// `NEW` beside them, and when what it keeps reads what a write the
    /// server derives produces before it within the same write: a later
    /// statement of its own, `FOUND` or the row such a write returns, or a
    /// table another trigger or a foreign key action writes before it fires,
    /// a deferred one after every other. A trigger that only refuses a write,
    /// reads, or sets columns of its own `NEW` row stays as it is unless it
    /// reads such a table, and so does every `INSTEAD OF` trigger, whose
    /// writes are the view's own. A foreign key whose `ON DELETE` or
    /// `ON UPDATE` action is `CASCADE`, `SET NULL` or `SET DEFAULT` is left
    /// out whole, since kept as `NO ACTION` it would refuse an offline delete
    /// of a parent whose children the server removes.
    ///
    /// [`Pg2Sqlite::translate_with_report`](crate::pg2sqlite::Pg2Sqlite::translate_with_report)
    /// names each one in a
    /// [`TranslationWarning::LeftToServer`](crate::warnings::TranslationWarning::LeftToServer).
    ///
    /// ```
    /// # use pg2sqlite::prelude::*;
    /// let options = Pg2SqliteOptions::default().with_server_derived_writes_omitted();
    /// let script = Pg2Sqlite::default()
    ///     .sql(
    ///         "CREATE TABLE folders (id INT PRIMARY KEY);
    ///          CREATE TABLE files (id INT PRIMARY KEY,
    ///              folder INT REFERENCES folders (id) ON DELETE CASCADE);",
    ///     )?
    ///     .translate_to_sql(&options)?;
    /// assert!(script.iter().all(|statement| !statement.contains("REFERENCES")));
    /// # Ok::<(), Error>(())
    /// ```
    pub fn with_server_derived_writes_omitted(mut self) -> Self {
        self.server_derived_writes_omitted = true;
        self
    }

    #[must_use]
    /// Returns whether the writes the server derives are left out.
    pub fn is_server_derived_writes_omitted(&self) -> bool {
        self.server_derived_writes_omitted
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
