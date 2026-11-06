//! RLS (Row-Level Security) translation from PostgreSQL to SQLite.
//!
//! This module handles the translation of PostgreSQL RLS policies to SQLite
//! by generating:
//! 1. A renamed inner table (e.g., `documents_rls`) containing the actual data
//! 2. A view with the original table name that filters rows based on policies
//! 3. INSTEAD OF triggers on the view for INSERT, UPDATE, DELETE operations
//!
//! The triggers are the only writers left once a table is split, so they carry
//! what a view cannot: a column's `DEFAULT`, which the INSERT trigger applies
//! itself, and a computed column, which it refuses to be told. A view gives
//! SQLite no way to tell an omitted column from one written NULL, so a default
//! answers for both.

use core::ops::ControlFlow;

use sql_traits::{
    errors::{LookupError, RowSecurityError},
    structs::{
        ActingRole, GatedCommand, IdentifierCase, ParserDB, RowGate, RowSecurity, RowStatement,
        RowVersion, TargetName,
    },
    traits::{ColumnLike, DatabaseLike, PolicyLike, TableLike, TriggerLike},
};
use sqlparser::{
    ast::{
        Assignment, AssignmentTarget, BinaryOperator, ColumnDef, ColumnOption, ColumnOptionDef,
        CreatePolicy, CreateTable, CreateTrigger, DataType, Expr, Function, FunctionArg,
        FunctionArgExpr, FunctionArgumentClause, FunctionArgumentList, FunctionArguments,
        FunctionDesc, HavingBound, Ident, InTarget, JoinConstraint, JoinOperator,
        ListAggOnOverflow, ObjectName, ObjectNamePart, SelectItem, SetExpr, Statement, TableAlias,
        TableFactor, TriggerEvent, TriggerExecBody, TriggerExecBodyType, TriggerObject,
        TriggerObjectKind, TriggerPeriod, UnaryOperator, Value, ValueWithSpan, Visit, VisitMut,
        Visitor, VisitorMut, WindowType,
    },
    tokenizer::{Token, Word},
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        expr_helpers::{concat, for_each_child_expr, map_expr_children},
        function_helpers::{integer_literal, simple_function_expr, string_literal},
        object_name::{append_suffix, last_ident, quote_identifier, quoted_ident},
        privileges::{RoleTable, has_backing_table},
        query_builder::{
            from_relation, make_query, make_simple_select, plain_table_factor, single_expr_query,
        },
        replay::{is_replayable, reject_duplicated_operand},
        session_variable,
        session_variable::SessionRead,
        shared_helpers::{
            join_constraint_mut, join_constraint_ref, minor_unit_scale, relations_scope_query,
        },
        translator_impls::column::declared_default,
    },
    options::Pg2SqliteOptions,
    traits::translator::TranslatorWithContext,
    write_guard::{
        RLS_VIOLATION, WriteCommand, no_policy, permission_denied, read_only, unreadable_key,
    },
};

/// A column of a guarded table, as the write triggers have to see it.
///
/// A view is not a table: it carries no defaults, and it cannot compute a
/// generated column. Both facts have to reach the triggers, which are the only
/// writers left once the table is split.
struct TriggerColumn {
    /// The column's name as the table declares it.
    name: String,
    /// The `DEFAULT` in stored minor units (scaled). Used for the forwarding
    /// write.
    default: Option<Expr>,
    /// The `DEFAULT` in original user-facing units. Used for guard
    /// substitutions that pass through `translate_with_warnings`, which
    /// handles NUMERIC scaling.
    guard_default: Option<Expr>,
    /// The expression SQLite computes the column from, for a generated column.
    generated: Option<Expr>,
    /// Whether the view holds the column, so a trigger on it can read `NEW`.
    projected: bool,
}

impl TriggerColumn {
    /// The value the forwarding write hands the backing table.
    ///
    /// A column the caller omitted arrives NULL, so a declared default answers
    /// for both.
    fn forwarded_value(&self) -> Expr {
        let new_value = prefixed_column_expr("NEW", &self.name);
        match &self.default {
            Some(default) => coalesce(new_value, default.clone()),
            None => new_value,
        }
    }

    /// `COALESCE(NEW.col, guard_default)` for the guard substitution path.
    ///
    /// Uses the unscaled default so `translate_with_warnings` scales it once.
    fn guard_coalesce(&self) -> Expr {
        let new_value = prefixed_column_expr("NEW", &self.name);
        match &self.guard_default {
            Some(default) => coalesce(new_value, default.clone()),
            None => new_value,
        }
    }

    const fn is_generated(&self) -> bool {
        self.generated.is_some()
    }

    /// Whether the forwarding write supplies this column.
    const fn is_forwarded(&self) -> bool {
        self.projected && !self.is_generated()
    }
}

/// The raw `DEFAULT` expression from the column definition, without NUMERIC
/// scaling.
fn raw_column_default(column: &ColumnDef) -> Option<Expr> {
    column.options.iter().find_map(|option| {
        if let ColumnOption::Default(expr) = &option.option { Some(expr.clone()) } else { None }
    })
}

/// Reads every column of the guarded table the way the write triggers need it,
/// on the view `grants` projects, or on the backing table when it is `None`.
///
/// # Errors
///
/// Propagates a lookup failure, and a default that cannot be expressed at the
/// column's scale, or a UUID BLOB column whose default is not a valid UUID.
fn trigger_columns(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    grants: Option<&RoleTable>,
) -> Result<Vec<TriggerColumn>, Error> {
    table
        .columns(schema)?
        .map(|column| {
            let attribute = column.attribute();
            let generated = attribute.options.iter().find_map(|option| {
                match &option.option {
                    ColumnOption::Generated { generation_expr: Some(expr), .. } => {
                        Some(expr.clone())
                    }
                    _ => None,
                }
            });
            let declared = declared_default(attribute, options)?;
            // The guard path scales a NUMERIC default itself, so it takes the
            // raw one.
            let guard_default = if minor_unit_scale(&attribute.data_type).is_some() {
                raw_column_default(attribute)
            } else {
                declared.clone()
            };
            let name = column.column_name().to_owned();
            Ok(TriggerColumn {
                projected: grants.is_none_or(|grants| grants.reads(&name)),
                name,
                default: declared,
                guard_default,
                generated,
            })
        })
        .collect()
}

/// `NEW."column"` and friends, as an expression rather than rendered text,
/// quoted exactly where [`quote_identifier`] quotes so the emitted trigger
/// reads the way the rest of it does.
fn prefixed_column_expr(prefix: &str, column: &str) -> Expr {
    let ident = if quote_identifier(column).starts_with('"') {
        Ident::with_quote('"', column)
    } else {
        Ident::new(column)
    };
    Expr::CompoundIdentifier(vec![Ident::new(prefix), ident])
}

fn coalesce(value: Expr, fallback: Expr) -> Expr {
    simple_function_expr("COALESCE", vec![value, fallback], None)
}

/// What a `NEW.<column>` or `OLD.<column>` reference in a write guard stands
/// for, when the value reaching the trigger is not the value the row carries.
type GuardSubstitution = (String, Expr);
#[derive(Clone, Copy)]
struct PolicyBindings<'a> {
    prefix: Option<&'a str>,
    /// Folded before translation, so a default lands as the SQL the backing
    /// table declares.
    substitutions: &'a [GuardSubstitution],
    /// Reads of a column the view does not hold, folded after translation
    /// since they are already in the stored representation.
    stored: &'a [GuardSubstitution],
}

/// Rewrites those references, so a guard judges the row as it will be stored.
struct GuardFolder<'a> {
    prefix: &'a str,
    substitutions: &'a [GuardSubstitution],
}

impl VisitorMut for GuardFolder<'_> {
    type Break = ();
    /// Folded on the way out, so a reference the fold itself writes is not
    /// folded again.
    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        if let Expr::CompoundIdentifier(parts) = expr
            && let [prefix, column] = parts.as_slice()
            && prefix.value.eq_ignore_ascii_case(self.prefix)
            && let Some((_, replacement)) =
                self.substitutions.iter().find(|(name, _)| name.eq_ignore_ascii_case(&column.value))
        {
            *expr = replacement.clone();
        }
        ControlFlow::Continue(())
    }
}

fn fold_guard(expr: &mut Expr, prefix: &str, substitutions: &[GuardSubstitution]) {
    if substitutions.is_empty() {
        return;
    }
    let _: ControlFlow<()> = VisitMut::visit(expr, &mut GuardFolder { prefix, substitutions });
}

/// Which write a guard sits in, which decides what a `NEW.<column>` reference
/// is missing.
#[derive(Clone, Copy, Eq, PartialEq)]
enum GuardKind {
    /// A column the caller left out arrives NULL, so a default answers for it.
    /// A generated column arrives NULL too, and SQLite will compute it.
    Insert,
    /// SQLite fills `NEW` from the row on disk, so only a generated column is
    /// missing: the view holds the value computed before the update.
    Update,
}

/// What each `NEW.<column>` reference in a guard has to be replaced by before
/// translation.
///
/// # Errors
///
/// Propagates a schema lookup failure.
fn guard_substitutions(
    columns: &[TriggerColumn],
    kind: GuardKind,
    options: &crate::options::TranslationContext<'_>,
    table: &CreateTable,
    schema: &ParserDB,
) -> Result<Vec<GuardSubstitution>, Error> {
    let supplied: Vec<GuardSubstitution> = if kind == GuardKind::Insert {
        columns
            .iter()
            .filter(|column| !column.is_generated())
            .filter_map(|column| {
                if column.projected {
                    column
                        .guard_default
                        .is_some()
                        .then(|| (column.name.clone(), column.guard_coalesce()))
                } else {
                    // A column the view does not hold takes its default.
                    let default = column.guard_default.clone().unwrap_or_else(null_literal);
                    Some((column.name.clone(), default))
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    let ctx = RlsTriggerContext::new(table, options);
    let lowercased_columns: Vec<String> =
        table.columns(schema)?.map(|c| c.column_name().to_lowercase()).collect();
    let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

    let mut substitutions = supplied.clone();
    for column in columns.iter().filter(|column| column.is_generated()) {
        let Some(expression) = column.generated.as_ref() else { continue };
        // PostgreSQL forbids a generation expression from reading another
        // generated column, so one pass is enough.
        let mut computed = transform_expr(
            expression,
            options,
            table,
            schema,
            Some("NEW"),
            Some(ctx.as_rename_tuple()),
            facts,
        );
        fold_guard(&mut computed, "NEW", &supplied);
        substitutions.push((column.name.clone(), Expr::Nested(Box::new(computed))));
    }

    Ok(substitutions)
}

/// The stored value of every column the view does not hold, read from the
/// backing row the trigger's `OLD` names, which is also its `NEW`, since such
/// a column cannot be assigned.
///
/// # Errors
///
/// Propagates a schema lookup failure.
fn stored_substitutions(
    columns: &[TriggerColumn],
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<GuardSubstitution>, Error> {
    if columns.iter().all(|column| column.projected) {
        return Ok(Vec::new());
    }
    let ctx = RlsTriggerContext::new(table, options);
    let names = collect_column_names(table, schema)?;
    let identity = build_row_identity_clause(&names, &collect_pk_column_names(table, schema)?);
    Ok(columns
        .iter()
        .filter(|column| !column.projected)
        .map(|column| {
            let read = single_expr_query(
                Expr::Identifier(quoted_ident(&column.name)),
                from_relation(plain_table_factor(generated_object_name(&ctx.inner_table_name))),
                Some(identity.clone()),
            );
            (column.name.clone(), Expr::Subquery(Box::new(read)))
        })
        .collect())
}

fn null_literal() -> Expr {
    Expr::Value(ValueWithSpan { value: Value::Null, span: sqlparser::tokenizer::Span::empty() })
}

fn refuse_computed_write_statements(
    columns: &[TriggerColumn],
    table_name: &str,
    guard: GuardKind,
) -> Vec<Statement> {
    columns
        .iter()
        .filter(|column| column.is_generated() && column.projected)
        .map(|column| {
            let new_value = prefixed_column_expr("NEW", &column.name);
            let supplied = match guard {
                GuardKind::Insert => Expr::IsNotNull(Box::new(new_value)),
                GuardKind::Update => Expr::IsDistinctFrom(
                    Box::new(new_value),
                    Box::new(prefixed_column_expr("OLD", &column.name)),
                ),
            };
            ast_builder::raise_statement("ABORT",
            Some(&format!(
                "cannot write to generated column \"{}\" of \"{table_name}\": SQLite computes it from the other columns",
                column.name
            )),
            Some(supplied),)
        })
        .collect()
}

/// True when `expr` is the boolean literal `true`, possibly wrapped in
/// redundant parentheses. Such a predicate constrains nothing.
#[cfg_attr(feature = "std", recursive::recursive)]
fn is_true_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Value(value) => matches!(value.value, Value::Boolean(true)),
        Expr::Nested(inner) => is_true_literal(inner),
        _ => false,
    }
}

/// The policies one row version passes, as `sql-traits` settles them for the
/// acting role.
type PolicyGate<'s> = RowGate<'s, CreatePolicy>;

/// How a policy set constrains rows once permissive and restrictive policies
/// are combined.
enum PolicyPredicate {
    DenyAll,
    AllowAll,
    Expr(Box<Expr>),
}

/// Combines a gate the way PostgreSQL does:
/// `(PERMISSIVE_1 OR PERMISSIVE_2 OR ...) AND RESTRICTIVE_1 AND RESTRICTIVE_2`,
/// so a gate with no permissive term denies every row.
///
/// `bindings` says what a `NEW.<column>` or `OLD.<column>` reference stands
/// for where the value reaching the trigger is not the value the row carries.
fn combine_policy_predicates(
    gate: Option<&PolicyGate<'_>>,
    bindings: PolicyBindings<'_>,
    options: &crate::options::TranslationContext<'_>,
    table: &CreateTable,
    schema: &ParserDB,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<PolicyPredicate, Error> {
    let Some(gate) = gate else { return Ok(PolicyPredicate::AllowAll) };
    let PolicyBindings { prefix, substitutions, stored } = bindings;
    // The backing table's name is what a policy naming its own table has to
    // resolve to, and it follows from the table and the options, so it is read
    // here rather than passed in and risked disagreeing with the trigger.
    let ctx = RlsTriggerContext::new(table, options);
    let table_rename = Some(ctx.as_rename_tuple());

    // Both sets are resolved once here rather than inside the recursive
    // transformer, which keeps that transformer total and avoids re-querying
    // the schema at every node it visits.
    let lowercased_columns: Vec<String> =
        table.columns(schema)?.map(|c| c.column_name().to_lowercase()).collect();
    let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

    let mut permissive = Vec::new();
    let mut restrictive = Vec::new();
    let has_permissive = !gate.permissive().is_empty();
    // One literally-true permissive term makes the whole permissive OR true.
    let mut permissive_always_true = false;

    let terms = gate
        .permissive()
        .iter()
        .map(|term| (false, term.expression()))
        .chain(gate.restrictive().iter().map(|term| (true, term.expression())));
    for (is_restrictive, expr) in terms {
        let mut transformed =
            transform_expr(expr, options, table, schema, prefix, table_rename, facts);
        // Folded before translation so a substituted default lands as the same
        // SQL the backing table declares.
        if let Some(prefix) = prefix {
            fold_guard(&mut transformed, prefix, substitutions);
        }
        // The transformer above owns the RLS rewrites: session variables, NEW
        // and OLD prefixes, backing table renames. The forward expression
        // translator then owns the PostgreSQL to SQLite semantics, so a policy
        // cannot smuggle an untranslated operator or function into the emitted
        // view or trigger guard, where it would fail at apply time (ILIKE) or
        // lie dormant until the first read (now, date_trunc).
        // The emitted backing-table name is an alias of the declared table for
        // type resolution.
        let scope_query;
        let policy_scope = if let Some((_, renamed)) = table_rename {
            let mut relation = plain_table_factor(table.name.clone());
            let TableFactor::Table { alias, .. } = &mut relation else { unreachable!() };
            *alias = Some(TableAlias {
                explicit: true,
                name: Ident::new(renamed),
                columns: Vec::new(),
                at: None,
            });
            scope_query = relations_scope_query(from_relation(relation));
            sql_traits::structs::ColumnScope::from_query(
                &scope_query,
                schema,
                IdentifierCase::AsWritten,
            )?
        } else {
            sql_traits::structs::ColumnScope::for_table(table, schema, IdentifierCase::AsWritten)
        };
        let scoped = options.with_pseudo_row_scope(&policy_scope);
        let mut transformed = transformed.translate_with_warnings(schema, &scoped, emit)?;
        if let Some(prefix) = prefix {
            fold_guard(&mut transformed, prefix, stored);
        }
        // A literally-true predicate constrains nothing. For a permissive
        // policy this means the permissive OR is satisfied for every row, so
        // we record that and skip adding any conjunct for it. For a restrictive
        // policy a true predicate constrains nothing and can also be dropped.
        if is_true_literal(&transformed) {
            if !is_restrictive {
                permissive_always_true = true;
            }
            continue;
        }
        if is_restrictive {
            restrictive.push(Expr::Nested(Box::new(transformed)));
        } else {
            permissive.push(Expr::Nested(Box::new(transformed)));
        }
    }

    if !has_permissive {
        return Ok(PolicyPredicate::DenyAll);
    }

    // Each condition is already parenthesised, so a grouping paren is only
    // needed when several permissive conditions are ORed together AND a
    // restrictive conjunct follows. Adding it unconditionally would churn every
    // snapshot with semantically identical output.
    let mut conjuncts = Vec::new();
    // When any permissive policy is literally true, the OR of all permissive
    // conditions is true. No permissive conjunct is needed and the collected
    // non-true permissive predicates are vacuous (true OR anything = true).
    if !permissive_always_true && !permissive.is_empty() {
        let mut joined = permissive
            .into_iter()
            .reduce(|left, right| {
                Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOperator::Or,
                    right: Box::new(right),
                }
            })
            .expect("the permissive set is not empty");
        if restrictive.is_empty() {
            conjuncts.push(joined);
        } else {
            joined = Expr::Nested(Box::new(joined));
            conjuncts.push(joined);
        }
    }
    conjuncts.extend(restrictive);

    if conjuncts.is_empty() {
        Ok(PolicyPredicate::AllowAll)
    } else {
        Ok(PolicyPredicate::Expr(Box::new(
            conjuncts
                .into_iter()
                .reduce(|left, right| {
                    Expr::BinaryOp {
                        left: Box::new(left),
                        op: BinaryOperator::And,
                        right: Box::new(right),
                    }
                })
                .expect("the conjunct set is not empty"),
        )))
    }
}

fn collect_column_names(
    table: &CreateTable,
    schema: &ParserDB,
) -> Result<Vec<String>, LookupError> {
    Ok(table.columns(schema)?.map(|c| c.column_name().to_string()).collect())
}

fn collect_pk_column_names(
    table: &CreateTable,
    schema: &ParserDB,
) -> Result<Vec<String>, LookupError> {
    Ok(table.primary_key_columns(schema)?.map(|c| c.column_name().to_string()).collect())
}

/// True when `expr` contains a bare or compound identifier whose last part
/// matches `name`.
fn expr_contains_identifier(expr: &Expr, name: &str) -> bool {
    struct IdentFinder<'a> {
        found: bool,
        name: &'a str,
    }
    impl Visitor for IdentFinder<'_> {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
            let found = match expr {
                Expr::Identifier(ident) => ident.value.eq_ignore_ascii_case(self.name),
                Expr::CompoundIdentifier(parts) => {
                    parts.last().is_some_and(|i| i.value.eq_ignore_ascii_case(self.name))
                }
                _ => false,
            };
            if found {
                self.found = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    let mut finder = IdentFinder { found: false, name };
    let _: ControlFlow<()> = expr.visit(&mut finder);
    finder.found
}

/// Returns `expr` with session-variable calls, subqueries, and caller-declared
/// functions replaced by replayable placeholders, so `is_replayable` focuses
/// on scalar volatile calls rather than flagging these stable constructs.
fn scrub_policy_stable_exprs(
    expr: &Expr,
    options: &crate::options::TranslationContext<'_>,
) -> Expr {
    struct Scrubber<'a, 'o> {
        options: &'a crate::options::TranslationContext<'o>,
    }
    impl VisitorMut for Scrubber<'_, '_> {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
            match expr {
                Expr::Subquery(_)
                | Expr::Exists { .. }
                | Expr::In { target: InTarget::Subquery(_), .. }
                | Expr::Function(Function { args: FunctionArguments::Subquery(_), .. }) => {
                    *expr = Expr::Value(ValueWithSpan::from(Value::Boolean(true)));
                }
                Expr::Function(func)
                    if session_variable::session_read_of_function(func).is_some()
                        || last_ident(&func.name).is_some_and(|n| {
                            self.options.declares_user_defined_function(&n.value)
                                || self
                                    .options
                                    .get_session_variables()
                                    .iter()
                                    .any(|m| m.sqlite_function.eq_ignore_ascii_case(&n.value))
                        }) =>
                {
                    *expr = Expr::Identifier(Ident::new("__sv__"));
                }
                _ => {}
            }
            ControlFlow::Continue(())
        }
    }
    let mut copy = expr.clone();
    let _: ControlFlow<()> = VisitMut::visit(&mut copy, &mut Scrubber { options });
    copy
}

/// The predicates guarding one DML event, each `None` where the event has no
/// such gate.
struct WriteGuard {
    columns: Vec<TriggerColumn>,
    /// `USING` on the existing row, for UPDATE.
    using: Option<PolicyPredicate>,
    /// `WITH CHECK` on the written row.
    check: PolicyPredicate,
    /// The `SELECT` policies an UPDATE reading a column holds its new row to.
    read_back: Option<PolicyPredicate>,
}

/// Builds the write-guard predicates for one DML event from `rows`, the
/// answer for the statement the event stands for, on the view `grants`
/// projects, or on the backing table when `grants` is `None`.
fn build_write_guard(
    rows: &Rows<'_>,
    kind: GuardKind,
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    grants: Option<&RoleTable>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<WriteGuard, Error> {
    let columns = trigger_columns(table, schema, options, grants)?;
    let subs = guard_substitutions(&columns, kind, options, table, schema)?;
    let stored = match kind {
        GuardKind::Update => stored_substitutions(&columns, table, schema, options)?,
        GuardKind::Insert => Vec::new(),
    };
    let table_name = table.table_name();
    let (using_gate, check_gate, read_back_gate) = match kind {
        GuardKind::Insert => (None, rows.gate(GatedCommand::Insert, RowVersion::Inserted), None),
        GuardKind::Update => {
            (
                rows.gate(GatedCommand::Update, RowVersion::Existing),
                rows.gate(GatedCommand::Update, RowVersion::Updated),
                rows.gate(GatedCommand::Select, RowVersion::Updated),
            )
        }
    };
    // A volatile column default reaches both the guard and the forwarding
    // INSERT, two independent draws.
    if kind == GuardKind::Insert {
        for column in &columns {
            let Some(guard_default) = &column.guard_default else { continue };
            let read = check_gate
                .into_iter()
                .flat_map(|gate| gate.permissive().iter().chain(gate.restrictive()));
            if !is_replayable(guard_default, options)
                && read
                    .into_iter()
                    .any(|term| expr_contains_identifier(term.expression(), &column.name))
            {
                return Err(reject_duplicated_operand(
                    &format!(
                        "column \"{}\" DEFAULT in the RLS insert path of table \"{}\"",
                        column.name, table_name
                    ),
                    guard_default,
                ));
            }
        }
    }
    // A volatile policy predicate reaches both the trigger guard and the
    // forwarding WHERE or backing check, two independent draws.
    for gate in [using_gate, check_gate, read_back_gate].into_iter().flatten() {
        for term in gate.permissive().iter().chain(gate.restrictive()) {
            if is_replayable(&scrub_policy_stable_exprs(term.expression(), options), options) {
                continue;
            }
            let clause = match (term.clause(), gate.row()) {
                (sql_traits::structs::PolicyClause::WithCheck, _) => "WITH CHECK",
                (_, RowVersion::Existing) => "USING",
                (_, _) if gate.command() == GatedCommand::Select => "USING (on the new row)",
                (_, _) => "USING (as WITH CHECK)",
            };
            return Err(reject_duplicated_operand(
                &format!(
                    "policy \"{}\" {clause} predicate on table \"{}\"",
                    term.policy().name(),
                    table_name
                ),
                term.expression(),
            ));
        }
    }
    let mut combine = |gate: Option<&PolicyGate<'_>>, prefix, substitutions| {
        combine_policy_predicates(
            gate,
            PolicyBindings { prefix: Some(prefix), substitutions, stored: &stored },
            options,
            table,
            schema,
            emit,
        )
    };
    // An exempt role has no gate, which combines to every row.
    let using = match kind {
        GuardKind::Update => Some(combine(using_gate, "OLD", &[])?),
        GuardKind::Insert => None,
    };
    let check = combine(check_gate, "NEW", &subs)?;
    let read_back = match read_back_gate {
        Some(gate) => Some(combine(Some(gate), "NEW", &subs)?),
        None => None,
    };
    Ok(WriteGuard { columns, using, check, read_back })
}

/// Returns whether the table named `table_name` has a backing table behind a
/// view, as [`has_backing_table`] answers.
///
/// # Errors
///
/// As [`has_backing_table`].
pub(crate) fn table_has_backing(
    table_name: &str,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<bool, Error> {
    match schema.table_by_target(TargetName::new(table_name, false), IdentifierCase::AsWritten)? {
        Some(table) => has_backing_table(table, schema, options),
        None => Ok(false),
    }
}

/// Resolves the name of the table that stores `table`'s rows, where AFTER
/// triggers attach.
///
/// A table with a backing table is split into a view and the backing table
/// (e.g., `table_rls`), and views don't fire AFTER triggers in SQLite.
///
/// # Errors
///
/// As [`has_backing_table`].
pub(crate) fn resolve_trigger_table_name(
    base_name: &str,
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<String, Error> {
    if has_backing_table(table, schema, options)? {
        let suffix = options.get_rls_table_suffix();
        Ok(format!("{base_name}{suffix}"))
    } else {
        Ok(base_name.to_string())
    }
}

/// Refuses a backing table suffix that cannot separate a secured table from the
/// view that replaces it.
///
/// The empty string is the one value that breaks the scheme outright: forward
/// translation would give the backing table and its view one name, and reverse
/// translation would read every name as a backing table, since every name ends
/// with nothing.
///
/// # Errors
///
/// Returns [`Error::EmptyRlsTableSuffix`]
/// when the configured suffix is empty.
pub fn ensure_usable_rls_table_suffix(
    options: &Pg2SqliteOptions,
) -> Result<(), crate::errors::Error> {
    if options.get_rls_table_suffix().is_empty() {
        return Err(crate::errors::Error::EmptyRlsTableSuffix);
    }
    Ok(())
}

fn build_row_identity_clause(columns: &[String], pk_columns: &[String]) -> Expr {
    let use_null_safe = pk_columns.is_empty();
    let identity_columns = if use_null_safe { columns } else { pk_columns };
    identity_columns
        .iter()
        .map(|column| {
            let current = Expr::Identifier(quoted_ident(column));
            let old = prefixed_column_expr("OLD", column);
            if use_null_safe {
                Expr::IsNotDistinctFrom(Box::new(current), Box::new(old))
            } else {
                Expr::BinaryOp {
                    left: Box::new(current),
                    op: BinaryOperator::Eq,
                    right: Box::new(old),
                }
            }
        })
        .reduce(|left, right| {
            Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::And, right: Box::new(right) }
        })
        .expect("a table has at least one identity column")
}

/// What row-level security applies to one statement shape, for the acting
/// role.
enum Rows<'s> {
    /// The role is exempt, so every row passes.
    Exempt,
    /// The gates a row passes.
    Gated(Vec<PolicyGate<'s>>),
}

impl<'s> Rows<'s> {
    /// The gate `command`'s policies put on `row`, or `None` where the role is
    /// exempt or the statement puts no such gate.
    fn gate(&self, command: GatedCommand, row: RowVersion) -> Option<&PolicyGate<'s>> {
        match self {
            Self::Exempt => None,
            Self::Gated(gates) => {
                gates.iter().find(|gate| gate.command() == command && gate.row() == row)
            }
        }
    }

    /// Whether no policy at all gates `command`'s `row`, which PostgreSQL
    /// reports as the command having no policy.
    fn has_no_policy(&self, command: GatedCommand, row: RowVersion) -> bool {
        self.gate(command, row)
            .is_some_and(|gate| gate.permissive().is_empty() && gate.restrictive().is_empty())
    }
}

/// Who this translation runs as: the deployment acting role it names, or a
/// caller holding none.
///
/// The schema owner role owns what the schema creates, and has to be declared
/// too.
fn acting_role<'s>(
    schema: &'s ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<ActingRole<'s, <ParserDB as DatabaseLike>::Role>, Error> {
    if let Some(name) = options.get_schema_owner_role()
        && schema.role(name).is_none()
    {
        return Err(Error::forward_refusal(format!(
            "the schema owner role '{name}' is not declared in the schema. Declare it with \
             CREATE ROLE, as PostgreSQL requires before a role can act or own."
        )));
    }
    Ok(match session_variable::acting_role(schema, options)? {
        Some(role) => ActingRole::Declared(role),
        None => ActingRole::Public,
    })
}

/// What row-level security applies to `statement` on `table`, as `sql-traits`
/// answers it for the acting role.
fn row_security<'s>(
    table: &'s CreateTable,
    schema: &'s ParserDB,
    statement: RowStatement,
    options: &Pg2SqliteOptions,
) -> Result<Rows<'s>, Error> {
    let answer = table.row_security(statement, acting_role(schema, options)?, schema);
    let refusal = |reason: String| Err(Error::forward_refusal(reason));
    match answer {
        Ok(RowSecurity::Exempt(_)) => Ok(Rows::Exempt),
        Ok(RowSecurity::Gated(gates)) => Ok(Rows::Gated(gates)),
        Ok(RowSecurity::InfiniteRecursion { relation }) => {
            refusal(format!(
                "the policies on '{}' expand back into '{relation}' while its own policies are \
                 still being expanded, so PostgreSQL answers `infinite recursion detected in \
                 policy for relation \"{relation}\"`. Rewrite one of the policies to read a \
                 table its expansion does not reach.",
                table.table_name()
            ))
        }
        Err(RowSecurityError::RoleRequired { policy }) => {
            refusal(format!(
                "policy '{policy}' on table '{}' is scoped to a specific role. Call \
                 with_acting_role to specify which role this translation targets.",
                table.table_name()
            ))
        }
        Err(RowSecurityError::UnsettledViewOwner { view }) => {
            refusal(format!(
                "the policies on '{}' read view '{view}', which reads its tables as its owner, \
                 and the schema never names that owner. Name it with ALTER VIEW {view} OWNER \
                 TO, or call with_schema_owner_role with the role that runs the schema.",
                table.table_name()
            ))
        }
        Err(RowSecurityError::Lookup(error)) => Err(error.into()),
        Err(error) => refusal(error.to_string()),
    }
}

struct RlsTriggerContext<'a> {
    table_name: &'a str,
    inner_table_name: String,
}

impl<'a> RlsTriggerContext<'a> {
    fn new(table: &'a CreateTable, options: &Pg2SqliteOptions) -> Self {
        let table_name = table.table_name();
        let inner_table_name = format!("{}{}", table_name, options.get_rls_table_suffix());
        Self { table_name, inner_table_name }
    }

    const fn as_rename_tuple(&self) -> (&str, &str) {
        (self.table_name, self.inner_table_name.as_str())
    }
}

fn push_pattern_unique(patterns: &mut Vec<SessionRead>, pattern: SessionRead) {
    if !patterns.contains(&pattern) {
        patterns.push(pattern);
    }
}

fn collect_patterns_from_function(func: &Function, patterns: &mut Vec<SessionRead>) {
    if let Some(pattern) = session_variable::session_read_of_function(func) {
        push_pattern_unique(patterns, pattern);
    }

    match &func.args {
        FunctionArguments::List(arg_list) => {
            for arg in &arg_list.args {
                match arg {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
                    | FunctionArg::Named { arg: FunctionArgExpr::Expr(expr), .. }
                    | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(expr), .. } => {
                        collect_session_variable_patterns(expr, patterns);
                    }
                    _ => {}
                }
            }
        }
        // `ARRAY(<subquery>)`, the one call sqlparser gives a bare subquery.
        FunctionArguments::Subquery(query) => collect_patterns_from_query(query, patterns),
        FunctionArguments::None => {}
    }

    if let Some(filter) = &func.filter {
        collect_session_variable_patterns(filter, patterns);
    }

    if let Some(over) = &func.over
        && let sqlparser::ast::WindowType::WindowSpec(window_spec) = over
    {
        for expr in &window_spec.partition_by {
            collect_session_variable_patterns(expr, patterns);
        }
        for order_by_expr in &window_spec.order_by {
            collect_session_variable_patterns(&order_by_expr.expr, patterns);
        }
    }

    for order_by_expr in &func.within_group {
        collect_session_variable_patterns(&order_by_expr.expr, patterns);
    }
}

#[cfg_attr(feature = "std", recursive::recursive)]
fn collect_patterns_from_table_factor(factor: &TableFactor, patterns: &mut Vec<SessionRead>) {
    match factor {
        TableFactor::Derived { subquery, .. } => {
            collect_patterns_from_query(subquery, patterns);
        }
        TableFactor::NestedJoin { table_with_joins, .. } => {
            collect_patterns_from_table_factor(&table_with_joins.relation, patterns);
            for join in &table_with_joins.joins {
                collect_patterns_from_table_factor(&join.relation, patterns);
            }
        }
        _ => {}
    }
}

fn collect_patterns_from_select(select: &sqlparser::ast::Select, patterns: &mut Vec<SessionRead>) {
    for table_with_joins in &select.from {
        collect_patterns_from_table_factor(&table_with_joins.relation, patterns);
        for join in &table_with_joins.joins {
            collect_patterns_from_table_factor(&join.relation, patterns);
            if let Some(JoinConstraint::On(expr)) = join_constraint_ref(&join.join_operator) {
                collect_session_variable_patterns(expr, patterns);
            }
            if let JoinOperator::AsOf { match_condition, .. } = &join.join_operator {
                collect_session_variable_patterns(match_condition, patterns);
            }
        }
    }

    if let Some(selection) = &select.selection {
        collect_session_variable_patterns(selection, patterns);
    }
    if let Some(having) = &select.having {
        collect_session_variable_patterns(having, patterns);
    }
    if let Some(qualify) = &select.qualify {
        collect_session_variable_patterns(qualify, patterns);
    }

    for item in &select.projection {
        if let sqlparser::ast::SelectItem::UnnamedExpr(expr)
        | sqlparser::ast::SelectItem::ExprWithAlias { expr, .. } = item
        {
            collect_session_variable_patterns(expr, patterns);
        }
    }

    if let sqlparser::ast::GroupByExpr::Expressions(exprs, _) = &select.group_by {
        for expr in exprs {
            collect_session_variable_patterns(expr, patterns);
        }
    }
}

#[cfg_attr(feature = "std", recursive::recursive)]
fn collect_patterns_from_set_expr(
    set_expr: &sqlparser::ast::SetExpr,
    patterns: &mut Vec<SessionRead>,
) {
    match set_expr {
        sqlparser::ast::SetExpr::Select(select) => collect_patterns_from_select(select, patterns),
        sqlparser::ast::SetExpr::Query(query) => collect_patterns_from_query(query, patterns),
        sqlparser::ast::SetExpr::SetOperation { left, right, .. } => {
            collect_patterns_from_set_expr(left, patterns);
            collect_patterns_from_set_expr(right, patterns);
        }
        sqlparser::ast::SetExpr::Values(values) => {
            for row in &values.rows {
                for expr in &row.content {
                    collect_session_variable_patterns(expr, patterns);
                }
            }
        }
        _ => {}
    }
}

#[cfg_attr(feature = "std", recursive::recursive)]
fn collect_patterns_from_query(query: &sqlparser::ast::Query, patterns: &mut Vec<SessionRead>) {
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            collect_patterns_from_query(&cte.query, patterns);
        }
    }
    collect_patterns_from_set_expr(query.body.as_ref(), patterns);

    if let Some(order_by) = &query.order_by
        && let sqlparser::ast::OrderByKind::Expressions(exprs) = &order_by.kind
    {
        for order_expr in exprs {
            collect_session_variable_patterns(&order_expr.expr, patterns);
        }
    }

    if let Some(limit_clause) = &query.limit_clause {
        match limit_clause {
            sqlparser::ast::LimitClause::LimitOffset { limit, offset, limit_by } => {
                if let Some(limit) = limit {
                    collect_session_variable_patterns(limit, patterns);
                }
                if let Some(offset) = offset {
                    collect_session_variable_patterns(&offset.value, patterns);
                }
                for expr in limit_by {
                    collect_session_variable_patterns(expr, patterns);
                }
            }
            sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                collect_session_variable_patterns(offset, patterns);
                collect_session_variable_patterns(limit, patterns);
            }
        }
    }

    if let Some(fetch) = &query.fetch
        && let Some(quantity) = &fetch.quantity
    {
        collect_session_variable_patterns(quantity, patterns);
    }
}

/// Collect every reading of the session an expression tree makes.
#[cfg_attr(feature = "std", recursive::recursive)]
fn collect_session_variable_patterns(expr: &Expr, patterns: &mut Vec<SessionRead>) {
    match expr {
        Expr::Identifier(_) => {
            if let Some(read) = session_variable::session_read(expr) {
                push_pattern_unique(patterns, read);
            }
        }
        Expr::Function(func) => collect_patterns_from_function(func, patterns),
        Expr::Subquery(query) | Expr::Exists { subquery: query, .. } => {
            collect_patterns_from_query(query, patterns);
        }
        Expr::In { expr: inner, target: InTarget::Subquery(subquery), .. } => {
            collect_session_variable_patterns(inner, patterns);
            collect_patterns_from_query(subquery, patterns);
        }
        _ => {
            for_each_child_expr(expr, &mut |child| {
                collect_session_variable_patterns(child, patterns);
            });
        }
    }
}

/// Validates that everything an expression reads of the session is declared:
/// each setting by the deployment and paired by a mapping, the acting role by
/// `with_acting_role`, and the login role by the deployment.
///
/// # Errors
///
/// Returns the error naming the first missing declaration, the table and the
/// policy.
pub fn validate_session_variables(
    expr: &Expr,
    options: &Pg2SqliteOptions,
    table_name: &str,
    policy_name: &str,
) -> Result<(), Error> {
    let mut patterns = Vec::new();
    collect_session_variable_patterns(expr, &mut patterns);
    let context = format!(" in table '{table_name}', policy '{policy_name}'");
    for pattern in patterns {
        session_variable::check_read(&pattern, options, &context)?;
    }
    Ok(())
}

/// Validates that all policies for a table have required session variable
/// mappings configured.
///
/// # Errors
///
/// Returns `Error::SessionVariableMappingNotFound` if any policy contains
/// a session variable pattern without a corresponding SQLite function mapping.
pub fn validate_table_policies(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    for policy in table.policies(schema)? {
        if let Some(using_expr) = policy.using_expression(schema) {
            validate_session_variables(using_expr, options, table.table_name(), policy.name())?;
        }
        if let Some(check_expr) = policy.check_expression(schema) {
            validate_session_variables(check_expr, options, table.table_name(), policy.name())?;
        }
    }
    Ok(())
}

/// Schema facts the expression transformer needs, resolved once by the caller.
///
/// Grouped so the transformer entry points take one reference rather than two
/// parallel slices that must always travel together.
#[derive(Clone, Copy)]
struct ResolvedSchemaFacts<'a> {
    /// The host table's column names, folded to lower case.
    lowercased_columns: &'a [String],
}

/// How column references are rewritten during expression transformation.
///
/// `prefix` qualifies bare column references, typically with `NEW` or `OLD`
/// depending on which row the surrounding trigger clause constrains. `None`
/// leaves them bare. `table_rename` rewrites a qualified reference to the
/// renamed backing table.
///
/// `lowercased_columns` holds the table's column names, folded to lower case
/// once by the caller. Resolving them up front keeps the recursive transformer
/// infallible and total, and avoids rescanning the table's columns at every
/// identifier node.
struct ColumnRefStrategy<'a> {
    prefix: Option<&'a str>,
    table_rename: Option<(&'a str, &'a str)>,
    lowercased_columns: &'a [String],
    /// Columns the subquery's own FROM relations declare; bare identifiers
    /// matching these belong to the inner scope and must not receive the
    /// prefix.
    inner_scope_columns: &'a [String],
}

impl<'a> ColumnRefStrategy<'a> {
    fn table_rename(&self) -> Option<(&str, &str)> {
        self.table_rename
    }

    fn subquery_prefix(&self) -> Option<&str> {
        self.prefix
    }

    fn facts(&self) -> ResolvedSchemaFacts<'a> {
        ResolvedSchemaFacts { lowercased_columns: self.lowercased_columns }
    }

    fn has_column(&self, lowercased_name: &str) -> bool {
        self.lowercased_columns.iter().any(|column| column == lowercased_name)
    }

    fn is_inner_scope(&self, lowercased_name: &str) -> bool {
        self.inner_scope_columns.iter().any(|col| col == lowercased_name)
    }
}

fn transform_function_argument_clause_rls(
    clause: &FunctionArgumentClause,
    options: &crate::options::TranslationContext<'_>,
    table: &CreateTable,
    schema: &ParserDB,
    strategy: &ColumnRefStrategy<'_>,
) -> FunctionArgumentClause {
    match clause {
        FunctionArgumentClause::OrderBy(order_by_exprs) => {
            FunctionArgumentClause::OrderBy(
                order_by_exprs
                    .iter()
                    .map(|ob| {
                        let mut transformed = ob.clone();
                        transformed.expr =
                            transform_expr_generic(&ob.expr, options, table, schema, strategy);
                        transformed
                    })
                    .collect(),
            )
        }
        FunctionArgumentClause::Limit(e) => {
            FunctionArgumentClause::Limit(transform_expr_generic(
                e, options, table, schema, strategy,
            ))
        }
        FunctionArgumentClause::Having(HavingBound(kind, e)) => {
            FunctionArgumentClause::Having(HavingBound(
                *kind,
                transform_expr_generic(e, options, table, schema, strategy),
            ))
        }
        FunctionArgumentClause::OnOverflow(ListAggOnOverflow::Truncate { filler, with_count }) => {
            FunctionArgumentClause::OnOverflow(ListAggOnOverflow::Truncate {
                filler: filler
                    .as_ref()
                    .map(|e| Box::new(transform_expr_generic(e, options, table, schema, strategy))),
                with_count: *with_count,
            })
        }
        other => other.clone(),
    }
}

fn transform_window_frame_bound_rls(
    bound: &sqlparser::ast::WindowFrameBound,
    options: &crate::options::TranslationContext<'_>,
    table: &CreateTable,
    schema: &ParserDB,
    strategy: &ColumnRefStrategy<'_>,
) -> sqlparser::ast::WindowFrameBound {
    match bound {
        sqlparser::ast::WindowFrameBound::Preceding(Some(e)) => {
            sqlparser::ast::WindowFrameBound::Preceding(Some(Box::new(transform_expr_generic(
                e, options, table, schema, strategy,
            ))))
        }
        sqlparser::ast::WindowFrameBound::Following(Some(e)) => {
            sqlparser::ast::WindowFrameBound::Following(Some(Box::new(transform_expr_generic(
                e, options, table, schema, strategy,
            ))))
        }
        other => other.clone(),
    }
}

#[allow(clippy::too_many_lines)]
#[cfg_attr(feature = "std", recursive::recursive)]
fn transform_expr_generic(
    expr: &Expr,
    options: &crate::options::TranslationContext<'_>,
    table: &CreateTable,
    schema: &ParserDB,
    strategy: &ColumnRefStrategy<'_>,
) -> Expr {
    let recurse = |e: &Expr| transform_expr_generic(e, options, table, schema, strategy);

    match expr {
        // A cast over the caller's identity is left standing here and resolved
        // by the expression translator, which owns the mapping for every
        // statement kind. Doing it twice was how the type check the mapping
        // records came to apply to a query but not to a policy.
        Expr::Cast { expr: inner, data_type, format, kind } => {
            Expr::Cast {
                expr: Box::new(recurse(inner)),
                data_type: data_type.clone(),
                format: format.clone(),
                kind: kind.clone(),
            }
        }

        Expr::Function(func) => {
            let transform_list = |list: &FunctionArgumentList| {
                FunctionArgumentList {
                    duplicate_treatment: list.duplicate_treatment,
                    args: list
                        .args
                        .iter()
                        .map(|arg| transform_function_arg_with_rls(arg, &recurse))
                        .collect(),
                    clauses: list
                        .clauses
                        .iter()
                        .map(|clause| {
                            transform_function_argument_clause_rls(
                                clause, options, table, schema, strategy,
                            )
                        })
                        .collect(),
                }
            };
            let transformed_args = match &func.args {
                FunctionArguments::List(arg_list) => {
                    FunctionArguments::List(transform_list(arg_list))
                }
                FunctionArguments::Subquery(query) => {
                    FunctionArguments::Subquery(Box::new(transform_query(
                        query,
                        options,
                        table,
                        schema,
                        strategy.subquery_prefix(),
                        strategy.table_rename(),
                        strategy.facts(),
                    )))
                }
                FunctionArguments::None => FunctionArguments::None,
            };

            let transformed_filter = func.filter.as_ref().map(|expr| Box::new(recurse(expr)));

            let transformed_over = func.over.as_ref().map(|window| {
                match window {
                    WindowType::WindowSpec(window_spec) => {
                        WindowType::WindowSpec(sqlparser::ast::WindowSpec {
                            window_name: window_spec.window_name.clone(),
                            partition_by: window_spec.partition_by.iter().map(&recurse).collect(),
                            order_by: window_spec
                                .order_by
                                .iter()
                                .map(|order_by_expr| {
                                    let mut transformed = order_by_expr.clone();
                                    transformed.expr = recurse(&order_by_expr.expr);
                                    transformed
                                })
                                .collect(),
                            window_frame: window_spec.window_frame.as_ref().map(|frame| {
                                sqlparser::ast::WindowFrame {
                                    units: frame.units,
                                    start_bound: transform_window_frame_bound_rls(
                                        &frame.start_bound,
                                        options,
                                        table,
                                        schema,
                                        strategy,
                                    ),
                                    end_bound: frame.end_bound.as_ref().map(|b| {
                                        transform_window_frame_bound_rls(
                                            b, options, table, schema, strategy,
                                        )
                                    }),
                                }
                            }),
                        })
                    }
                    WindowType::NamedWindow(named_window) => {
                        WindowType::NamedWindow(named_window.clone())
                    }
                }
            });

            let transformed_within_group = func
                .within_group
                .iter()
                .map(|order_by_expr| {
                    let mut transformed = order_by_expr.clone();
                    transformed.expr = recurse(&order_by_expr.expr);
                    transformed
                })
                .collect();

            let transformed_parameters = match &func.parameters {
                FunctionArguments::List(param_list) => {
                    FunctionArguments::List(transform_list(param_list))
                }
                other => other.clone(),
            };

            Expr::Function(Function {
                name: func.name.clone(),
                uses_odbc_syntax: func.uses_odbc_syntax,
                parameters: transformed_parameters,
                args: transformed_args,
                filter: transformed_filter,
                null_treatment: func.null_treatment,
                over: transformed_over,
                within_group: transformed_within_group,
            })
        }

        // An unquoted `current_role` is the role keyword, never a column.
        Expr::Identifier(ident) => {
            let ident_lower = ident.value.to_lowercase();

            if let Some(pfx) = strategy.prefix
                && crate::impls::session_variable::session_read(expr).is_none()
                && strategy.has_column(&ident_lower)
                && !strategy.is_inner_scope(&ident_lower)
            {
                return Expr::CompoundIdentifier(vec![Ident::new(pfx), ident.clone()]);
            }

            Expr::Identifier(ident.clone())
        }

        // Handle already-qualified identifiers (e.g., table.column)
        Expr::CompoundIdentifier(idents) => {
            if let Some((old_name, new_name)) = strategy.table_rename()
                && idents.len() >= 2
                && idents[0].value.to_lowercase() == old_name.to_lowercase()
            {
                let mut new_idents = idents.clone();
                new_idents[0] = Ident::new(strategy.prefix.unwrap_or(new_name));
                return Expr::CompoundIdentifier(new_idents);
            }
            Expr::CompoundIdentifier(idents.clone())
        }

        // Handle subqueries with transform_query (not just expr recursion)
        Expr::Exists { subquery, negated } => {
            Expr::Exists {
                subquery: Box::new(transform_query(
                    subquery,
                    options,
                    table,
                    schema,
                    strategy.subquery_prefix(),
                    strategy.table_rename(),
                    strategy.facts(),
                )),
                negated: *negated,
            }
        }

        Expr::Subquery(subquery) => {
            Expr::Subquery(Box::new(transform_query(
                subquery,
                options,
                table,
                schema,
                strategy.subquery_prefix(),
                strategy.table_rename(),
                strategy.facts(),
            )))
        }

        Expr::In { expr: inner, target: InTarget::Subquery(subquery), negated, global } => {
            Expr::In {
                expr: Box::new(recurse(inner)),
                target: InTarget::Subquery(Box::new(transform_query(
                    subquery,
                    options,
                    table,
                    schema,
                    strategy.subquery_prefix(),
                    strategy.table_rename(),
                    strategy.facts(),
                ))),
                negated: *negated,
                global: *global,
            }
        }

        // All other variants: delegate structural recursion to
        // map_expr_children
        other => map_expr_children(other, &recurse),
    }
}

fn transform_expr(
    expr: &Expr,
    options: &crate::options::TranslationContext<'_>,
    table: &CreateTable,
    schema: &ParserDB,
    prefix: Option<&str>,
    table_rename: Option<(&str, &str)>,
    facts: ResolvedSchemaFacts<'_>,
) -> Expr {
    transform_expr_generic(
        expr,
        options,
        table,
        schema,
        &ColumnRefStrategy {
            prefix,
            table_rename,
            lowercased_columns: facts.lowercased_columns,
            inner_scope_columns: &[],
        },
    )
}

fn transform_query(
    query: &sqlparser::ast::Query,
    options: &crate::options::TranslationContext<'_>,
    table: &CreateTable,
    schema: &ParserDB,
    prefix: Option<&str>,
    outer_table: Option<(&str, &str)>,
    facts: ResolvedSchemaFacts<'_>,
) -> sqlparser::ast::Query {
    let mut transformed = query.clone();
    let context = SubqueryTransformContext {
        options,
        table,
        schema,
        prefix,
        outer_table,
        lowercased_columns: facts.lowercased_columns,
    };

    *transformed.body = transform_set_expr(&query.body, &context);

    // ORDER BY, LIMIT and FETCH sit at the query level, outside any SELECT's
    // scope, so an outer reference there needs the same rewrite.
    let Some((outer_name, renamed)) = outer_table else { return transformed };
    let rewrite = |expr: &Expr| transform_outer_table_refs(expr, outer_name, prefix, Some(renamed));

    if let Some(order_by) = &mut transformed.order_by
        && let sqlparser::ast::OrderByKind::Expressions(exprs) = &mut order_by.kind
    {
        for order_expr in exprs {
            order_expr.expr = rewrite(&order_expr.expr);
        }
    }

    if let Some(limit_clause) = &mut transformed.limit_clause {
        match limit_clause {
            sqlparser::ast::LimitClause::LimitOffset { limit, offset, limit_by } => {
                if let Some(limit) = limit {
                    *limit = rewrite(limit);
                }
                if let Some(offset) = offset {
                    offset.value = rewrite(&offset.value);
                }
                for key in limit_by {
                    *key = rewrite(key);
                }
            }
            sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                *offset = rewrite(offset);
                *limit = rewrite(limit);
            }
        }
    }

    if let Some(fetch) = &mut transformed.fetch
        && let Some(quantity) = &mut fetch.quantity
    {
        *quantity = rewrite(quantity);
    }

    transformed
}

/// Transforms every position inside a `SetExpr`: the `Select` case handles
/// `from`, `selection`, `projection`, `having`, `qualify`, and `group_by` as
/// before; the `SetOperation` case recurses into both arms so that an outer
/// table reference inside any arm (e.g. a `UNION ALL`) is reached; the `Query`
/// case calls `transform_query` on the nested query.
#[cfg_attr(feature = "std", recursive::recursive)]
fn transform_set_expr(
    set_expr: &sqlparser::ast::SetExpr,
    context: &SubqueryTransformContext<'_>,
) -> sqlparser::ast::SetExpr {
    match set_expr {
        sqlparser::ast::SetExpr::Select(select) => {
            let mut transformed = select.as_ref().clone();
            let mut subquery_table_renames: Vec<(String, String)> = Vec::new();
            for table_with_joins in &mut transformed.from {
                transform_table_with_joins_for_subquery(
                    table_with_joins,
                    context,
                    &mut subquery_table_renames,
                );
            }

            let rewrite_expr =
                |expr: &Expr| transform_subquery_expression(expr, context, &subquery_table_renames);

            if let Some(selection) = &mut transformed.selection {
                *selection = rewrite_expr(selection);
            }

            for item in &mut transformed.projection {
                if let sqlparser::ast::SelectItem::UnnamedExpr(expr)
                | sqlparser::ast::SelectItem::ExprWithAlias { expr, .. } = item
                {
                    *expr = rewrite_expr(expr);
                }
            }

            if let Some(having) = &mut transformed.having {
                *having = rewrite_expr(having);
            }

            if let Some(qualify) = &mut transformed.qualify {
                *qualify = rewrite_expr(qualify);
            }

            if let sqlparser::ast::GroupByExpr::Expressions(group_exprs, _) =
                &mut transformed.group_by
            {
                for group_expr in group_exprs {
                    *group_expr = rewrite_expr(group_expr);
                }
            }

            sqlparser::ast::SetExpr::Select(Box::new(transformed))
        }

        sqlparser::ast::SetExpr::SetOperation { op, set_quantifier, left, right } => {
            sqlparser::ast::SetExpr::SetOperation {
                op: *op,
                set_quantifier: *set_quantifier,
                left: Box::new(transform_set_expr(left, context)),
                right: Box::new(transform_set_expr(right, context)),
            }
        }

        sqlparser::ast::SetExpr::Query(inner_query) => {
            sqlparser::ast::SetExpr::Query(Box::new(transform_query(
                inner_query,
                context.options,
                context.table,
                context.schema,
                context.prefix,
                context.outer_table,
                context.facts(),
            )))
        }

        other => other.clone(),
    }
}

fn transform_subquery_expression(
    expr: &Expr,
    context: &SubqueryTransformContext<'_>,
    subquery_table_renames: &[(String, String)],
) -> Expr {
    let (options, table, schema, prefix) =
        (context.options, context.table, context.schema, context.prefix);
    let facts = context.facts();
    let mut transformed = expr.clone();

    if let Some((outer_table_name, renamed_table_name)) = context.outer_table {
        transformed = transform_outer_table_refs(
            &transformed,
            outer_table_name,
            prefix,
            Some(renamed_table_name),
        );
    }

    // Subquery table renames only rewrite table-name qualifiers; applying the
    // outer prefix here would turn `members.col` into `NEW.col`, which is
    // wrong when `members` is the subquery's own FROM table.
    for (old_name, new_name) in subquery_table_renames {
        transformed = transform_expr(
            &transformed,
            options,
            table,
            schema,
            None,
            Some((old_name.as_str(), new_name.as_str())),
            facts,
        );
    }

    // Collect columns declared by the subquery's own named FROM tables. A bare
    // identifier the inner scope declares must not receive the outer prefix.
    let inner_scope_columns: Vec<String> = subquery_table_renames
        .iter()
        .flat_map(|(table_name, _)| {
            schema
                .tables()
                .find(|t| t.table_name().eq_ignore_ascii_case(table_name))
                .into_iter()
                .flat_map(|t| {
                    t.columns(schema).into_iter().flatten().map(|c| c.column_name().to_lowercase())
                })
        })
        .collect();

    transform_expr_generic(
        &transformed,
        options,
        table,
        schema,
        &ColumnRefStrategy {
            prefix,
            table_rename: None,
            lowercased_columns: facts.lowercased_columns,
            inner_scope_columns: &inner_scope_columns,
        },
    )
}

struct SubqueryTransformContext<'a> {
    options: &'a crate::options::TranslationContext<'a>,
    table: &'a CreateTable,
    schema: &'a ParserDB,
    prefix: Option<&'a str>,
    outer_table: Option<(&'a str, &'a str)>,
    /// The host table's column names, lower-cased once by the caller.
    lowercased_columns: &'a [String],
}

impl<'a> SubqueryTransformContext<'a> {
    fn facts(&self) -> ResolvedSchemaFacts<'a> {
        ResolvedSchemaFacts { lowercased_columns: self.lowercased_columns }
    }
}

fn transform_table_with_joins_for_subquery(
    table_with_joins: &mut sqlparser::ast::TableWithJoins,
    context: &SubqueryTransformContext<'_>,
    subquery_table_renames: &mut Vec<(String, String)>,
) {
    transform_table_factor_for_subquery(
        &mut table_with_joins.relation,
        context,
        subquery_table_renames,
    );

    for join in &mut table_with_joins.joins {
        transform_table_factor_for_subquery(&mut join.relation, context, subquery_table_renames);
        transform_join_operator_for_subquery(
            &mut join.join_operator,
            context,
            subquery_table_renames,
        );
    }
}

fn transform_table_factor_for_subquery(
    factor: &mut TableFactor,
    context: &SubqueryTransformContext<'_>,
    subquery_table_renames: &mut Vec<(String, String)>,
) {
    match factor {
        TableFactor::Table { name, .. } => {
            let old_name =
                last_ident(name).map_or_else(|| name.to_string(), |ident| ident.value.clone());
            // Every reference keeps the name the policy wrote, so a second
            // guarded table is read through its view and its own policy filters
            // what this subquery sees, which is how PostgreSQL evaluates it.
            // Reading the backing table instead would bypass that policy, and
            // whether the reference carries an alias cannot be what decides it.
            // The rename entry is an identity so the qualifier rewrite in
            // `transform_subquery_expression` leaves the reference alone.
            subquery_table_renames.push((old_name.clone(), old_name));
        }
        TableFactor::Derived { subquery, .. } => {
            **subquery = transform_query(
                subquery,
                context.options,
                context.table,
                context.schema,
                context.prefix,
                context.outer_table,
                context.facts(),
            );
        }
        TableFactor::NestedJoin { table_with_joins, .. } => {
            transform_table_with_joins_for_subquery(
                table_with_joins,
                context,
                subquery_table_renames,
            );
        }
        _ => {}
    }
}

fn transform_join_operator_for_subquery(
    join_operator: &mut JoinOperator,
    context: &SubqueryTransformContext<'_>,
    subquery_table_renames: &[(String, String)],
) {
    let rewrite_constraint = |constraint: &mut JoinConstraint| {
        if let JoinConstraint::On(expr) = constraint {
            *expr = transform_subquery_expression(expr, context, subquery_table_renames);
        }
    };

    if let Some(constraint) = join_constraint_mut(join_operator) {
        rewrite_constraint(constraint);
    }

    if let JoinOperator::AsOf { match_condition, .. } = join_operator {
        *match_condition =
            transform_subquery_expression(match_condition, context, subquery_table_renames);
    }
}

fn transform_function_arg_with_rls(
    arg: &FunctionArg,
    transform_fn: &impl Fn(&Expr) -> Expr,
) -> FunctionArg {
    let transform_arg_expr = |arg_expr: &FunctionArgExpr| -> FunctionArgExpr {
        match arg_expr {
            FunctionArgExpr::Expr(e) => FunctionArgExpr::Expr(transform_fn(e)),
            other => other.clone(),
        }
    };
    match arg {
        FunctionArg::Named { name, arg, operator } => {
            FunctionArg::Named {
                name: name.clone(),
                arg: transform_arg_expr(arg),
                operator: operator.clone(),
            }
        }
        FunctionArg::ExprNamed { name, arg, operator } => {
            FunctionArg::ExprNamed {
                name: name.clone(),
                arg: transform_arg_expr(arg),
                operator: operator.clone(),
            }
        }
        FunctionArg::Unnamed(arg) => FunctionArg::Unnamed(transform_arg_expr(arg)),
    }
}

#[cfg_attr(feature = "std", recursive::recursive)]
fn transform_outer_table_refs(
    expr: &Expr,
    outer_table_name: &str,
    prefix: Option<&str>,
    renamed_table: Option<&str>,
) -> Expr {
    let recurse = |e: &Expr| transform_outer_table_refs(e, outer_table_name, prefix, renamed_table);

    match expr {
        Expr::CompoundIdentifier(idents) => {
            // Check if this is a reference to the outer table
            if idents.len() >= 2
                && idents[0].value.to_lowercase() == outer_table_name.to_lowercase()
            {
                let mut new_idents = idents.clone();
                if let Some(pfx) = prefix {
                    // In trigger context: ownables.id -> OLD.id or NEW.id
                    new_idents[0] = Ident::new(pfx);
                } else if let Some(renamed) = renamed_table {
                    // In view context: ownables.id -> ownables_rls.id
                    new_idents[0] = Ident::new(renamed);
                }
                return Expr::CompoundIdentifier(new_idents);
            }
            Expr::CompoundIdentifier(idents.clone())
        }

        // A call falls through: `map_expr_children` walks every expression it
        // carries, arguments, `FILTER`, `WITHIN GROUP` and the `OVER` window
        // alike. The arm that used to stand here rewrote the first two and
        // cloned the rest, so an outer reference inside `PARTITION BY`
        // survived into the trigger.
        other => map_expr_children(other, &recurse),
    }
}

/// Computes the predicate the RLS view applies on the read path.
///
/// Single source of truth for whether a table's view denies every row, shared
/// by the view generator and the decision to emit validation triggers, so the
/// two cannot drift into disagreeing about the same table.
fn rls_read_predicate(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<PolicyPredicate, Error> {
    let rows = row_security(table, schema, RowStatement::Select, options)?;
    combine_policy_predicates(
        rows.gate(GatedCommand::Select, RowVersion::Existing),
        PolicyBindings { prefix: None, substitutions: &[], stored: &[] },
        options,
        table,
        schema,
        emit,
    )
}

fn generated_object_name(name: &str) -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(name))])
}

fn generate_rls_view_statement_with_context(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    grants: &RoleTable,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Statement, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let projection = grants
        .readable()
        .map(|column| SelectItem::UnnamedExpr(Expr::Identifier(quoted_ident(column))))
        .collect();
    let selection = match rls_read_predicate(table, schema, options, emit)? {
        PolicyPredicate::DenyAll => {
            Some(Expr::Value(ValueWithSpan {
                value: Value::Boolean(false),
                span: sqlparser::tokenizer::Span::empty(),
            }))
        }
        PolicyPredicate::AllowAll => None,
        PolicyPredicate::Expr(predicate) => Some(*predicate),
    };
    let query = make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            projection,
            from_relation(plain_table_factor(generated_object_name(&ctx.inner_table_name))),
            selection,
        ))),
    );
    Ok(ast_builder::create_view(generated_object_name(ctx.table_name), Vec::new(), query))
}

/// `RAISE(ABORT, 'permission denied for table <t>')` where `violation` holds,
/// everywhere when it is `None`, unless the write is exempt.
fn permission_gate(
    table_name: &str,
    violation: Option<Expr>,
    options: &Pg2SqliteOptions,
) -> Statement {
    ast_builder::raise_statement(
        "ABORT",
        Some(&permission_denied(table_name)),
        write_guard_condition_expr(violation, options),
    )
}

/// Refuses an insert supplying a column the role may not insert into, which
/// is every insert when it may insert into none. A view hands the trigger an
/// omitted column as `NULL`, so only a value tells a supplied column apart.
fn insert_permission_gate(
    columns: &[TriggerColumn],
    table_name: &str,
    grants: &RoleTable,
    options: &Pg2SqliteOptions,
) -> Option<Statement> {
    if !grants.inserts_any() {
        return Some(permission_gate(table_name, None, options));
    }
    columns
        .iter()
        .filter(|column| column.is_forwarded() && !grants.inserts(&column.name))
        .map(|column| Expr::IsNotNull(Box::new(prefixed_column_expr("NEW", &column.name))))
        .reduce(|left, right| {
            Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::Or, right: Box::new(right) }
        })
        .map(|supplied| {
            permission_gate(table_name, Some(Expr::Nested(Box::new(supplied))), options)
        })
}

fn generate_insert_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    grants: &RoleTable,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Statement, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let table_name = ctx.table_name;
    let rows = row_security(table, schema, RowStatement::Insert { returning: false }, options)?;
    let WriteGuard { columns, check, .. } =
        build_write_guard(&rows, GuardKind::Insert, table, schema, options, Some(grants), emit)?;
    // A column the view does not hold is left to the backing table's default.
    let written: Vec<&TriggerColumn> =
        columns.iter().filter(|column| column.is_forwarded()).collect();
    let target_columns = written.iter().map(|column| generated_object_name(&column.name)).collect();
    let values = written
        .iter()
        .map(|column| column.forwarded_value().translate_with_warnings(schema, options, emit))
        .collect::<Result<Vec<_>, Error>>()?;

    let guard = if rows.has_no_policy(GatedCommand::Insert, RowVersion::Inserted) {
        Some(ast_builder::raise_statement(
            "ABORT",
            Some(&no_policy(WriteCommand::Insert, table_name)),
            write_guard_condition_expr(None, options),
        ))
    } else {
        match check {
            PolicyPredicate::DenyAll => {
                Some(ast_builder::raise_statement(
                    "ABORT",
                    Some(RLS_VIOLATION),
                    write_guard_condition_expr(None, options),
                ))
            }
            PolicyPredicate::AllowAll => None,
            PolicyPredicate::Expr(predicate) => {
                let violation = Expr::IsNotTrue(Box::new(Expr::Nested(predicate)));
                Some(ast_builder::raise_statement(
                    "ABORT",
                    Some(RLS_VIOLATION),
                    write_guard_condition_expr(Some(violation), options),
                ))
            }
        }
    };

    // PostgreSQL checks privileges before policies.
    let mut body = insert_permission_gate(&columns, table_name, grants, options)
        .into_iter()
        .chain(guard)
        .collect::<Vec<_>>();
    body.extend(refuse_computed_write_statements(&columns, table_name, GuardKind::Insert));
    body.push(ast_builder::insert(
        generated_object_name(&ctx.inner_table_name),
        target_columns,
        ast_builder::values(vec![values]),
    ));
    Ok(ast_builder::trigger(
        generated_object_name(&format!("{table_name}_insert_trigger")),
        generated_object_name(table_name),
        sqlparser::ast::TriggerPeriod::InsteadOf,
        sqlparser::ast::TriggerEvent::Insert,
        true,
        None,
        body,
    ))
}

fn write_exemption_expr(options: &Pg2SqliteOptions) -> Option<Expr> {
    options.get_write_exemption_function().map(|name| {
        let mut expression = simple_function_expr(name, Vec::new(), None);
        let Expr::Function(function) = &mut expression else {
            unreachable!("a function constructor must return a function")
        };
        function.name = ObjectName(vec![ObjectNamePart::Identifier(Ident::with_quote('"', name))]);
        expression
    })
}

pub(crate) fn write_guard_condition_expr(
    violation: Option<Expr>,
    options: &Pg2SqliteOptions,
) -> Option<Expr> {
    let enforcement = write_exemption_expr(options)
        .map(|call| Expr::IsNotTrue(Box::new(Expr::Nested(Box::new(call)))));
    match (enforcement, violation) {
        (None, None) => None,
        (Some(enforcement), None) => Some(enforcement),
        (None, Some(violation)) => Some(violation),
        (Some(enforcement), Some(violation)) => {
            Some(Expr::BinaryOp {
                left: Box::new(enforcement),
                op: BinaryOperator::And,
                right: Box::new(Expr::Nested(Box::new(violation))),
            })
        }
    }
}

fn policy_or_exemption_expr(predicate: Expr, options: &Pg2SqliteOptions) -> Expr {
    write_exemption_expr(options).map_or(predicate.clone(), |call| {
        Expr::BinaryOp {
            left: Box::new(Expr::IsTrue(Box::new(Expr::Nested(Box::new(call))))),
            op: BinaryOperator::Or,
            right: Box::new(Expr::Nested(Box::new(predicate))),
        }
    })
}

fn policy_violation(predicate: Expr) -> Expr {
    Expr::IsNotTrue(Box::new(Expr::Nested(Box::new(predicate))))
}

/// What a row fails when it has to pass every one of a set of predicates.
enum Violation {
    /// A predicate admits no row.
    Every,
    /// The rows failing any predicate, `None` when each admits every row.
    Rows(Option<Box<Expr>>),
}

/// A predicate appearing twice, as a `FOR ALL USING (p)` policy's check and
/// read-back do, is tested once.
fn violation(predicates: impl IntoIterator<Item = PolicyPredicate>) -> Violation {
    let mut failing = Vec::new();
    for predicate in predicates {
        match predicate {
            PolicyPredicate::DenyAll => return Violation::Every,
            PolicyPredicate::AllowAll => {}
            PolicyPredicate::Expr(predicate) => {
                let violation = policy_violation(*predicate);
                if !failing.contains(&violation) {
                    failing.push(violation);
                }
            }
        }
    }
    Violation::Rows(
        failing
            .into_iter()
            .reduce(|left, right| {
                Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOperator::Or,
                    right: Box::new(right),
                }
            })
            .map(Box::new),
    )
}

/// The `INSTEAD OF UPDATE` statement refusing an updated row that fails
/// `WITH CHECK` or, read back, the `SELECT` policies, raised only for a row
/// `using` lets the caller update.
fn new_row_guard(
    check: PolicyPredicate,
    read_back: Option<PolicyPredicate>,
    using: Option<Expr>,
    options: &Pg2SqliteOptions,
) -> Option<Statement> {
    let violation = match violation([Some(check), read_back].into_iter().flatten()) {
        Violation::Every => None,
        Violation::Rows(None) => return None,
        Violation::Rows(Some(violation)) => {
            Some(match using {
                Some(using) => {
                    // A disjunction stays one operand of the conjunct.
                    let violation =
                        if matches!(*violation, Expr::BinaryOp { op: BinaryOperator::Or, .. }) {
                            Expr::Nested(violation)
                        } else {
                            *violation
                        };
                    Expr::BinaryOp {
                        left: Box::new(Expr::Nested(Box::new(using))),
                        op: BinaryOperator::And,
                        right: Box::new(violation),
                    }
                }
                None => *violation,
            })
        }
    };
    Some(ast_builder::raise_statement(
        "ABORT",
        Some(RLS_VIOLATION),
        write_guard_condition_expr(violation, options),
    ))
}

/// A `BEFORE` trigger on a backing table raising `message` where `condition`
/// holds, unless a definer body whose owner is among `definer_owners` is
/// running.
fn backing_abort_trigger(
    name: &str,
    table_name: &str,
    event: sqlparser::ast::TriggerEvent,
    condition: Option<Expr>,
    definer_owners: &[&str],
    message: &str,
) -> Statement {
    let condition = match (condition, super::definer::outside_owners(definer_owners)) {
        (Some(condition), Some(outside)) => {
            Some(Expr::BinaryOp {
                left: Box::new(Expr::Nested(Box::new(condition))),
                op: BinaryOperator::And,
                right: Box::new(outside),
            })
        }
        (condition, outside) => condition.or(outside),
    };
    ast_builder::trigger(
        generated_object_name(name),
        generated_object_name(table_name),
        sqlparser::ast::TriggerPeriod::Before,
        event,
        true,
        condition,
        vec![ast_builder::raise_statement("ABORT", Some(message), None)],
    )
}

fn generate_insert_check_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Statement>, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let rows = row_security(table, schema, RowStatement::Insert { returning: false }, options)?;
    let (condition, message) = if rows.has_no_policy(GatedCommand::Insert, RowVersion::Inserted) {
        (write_guard_condition_expr(None, options), no_policy(WriteCommand::Insert, ctx.table_name))
    } else {
        let WriteGuard { check, .. } =
            build_write_guard(&rows, GuardKind::Insert, table, schema, options, None, emit)?;
        match check {
            PolicyPredicate::DenyAll => {
                (write_guard_condition_expr(None, options), RLS_VIOLATION.to_string())
            }
            PolicyPredicate::AllowAll => return Ok(None),
            PolicyPredicate::Expr(predicate) => {
                (
                    write_guard_condition_expr(Some(policy_violation(*predicate)), options),
                    RLS_VIOLATION.to_string(),
                )
            }
        }
    };
    Ok(Some(backing_abort_trigger(
        &format!("{}_insert_check", ctx.inner_table_name),
        &ctx.inner_table_name,
        sqlparser::ast::TriggerEvent::Insert,
        condition,
        &super::definer::exempt_owners(table, schema),
        &message,
    )))
}

/// Builds the WHEN condition that lets an internal maintenance UPDATE through
/// the backing-table BEFORE UPDATE deny guard.
///
/// A BEFORE INSERT maintenance trigger is converted to an AFTER INSERT trigger
/// whose body does `UPDATE backing SET col = expr(NEW_at_insert)`. The BEFORE
/// UPDATE guard fires on that UPDATE. To allow it through while still denying
/// caller UPDATEs, the guard fires only when `NEW.col IS DISTINCT FROM
/// expr(OLD)` — FALSE for the maintenance UPDATE (which sets col to exactly
/// that value) and TRUE for unrelated UPDATEs.
///
/// The prewalk stores (table_name, fn_name) for each BEFORE INSERT trigger so
/// this function can reach the maintenance assignments through the epoch schema
/// (which has functions and tables but not triggers). A synthetic CreateTrigger
/// carrying only the table and function fields lets TriggerLike do the lookup.
fn build_maintenance_insert_exemption(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Option<Expr> {
    let table_name = table.table_name().to_lowercase();
    let mut any_trigger_match: Option<Expr> = None;

    for (trig_table, fn_name) in options.before_insert_trigger_fns() {
        if trig_table != &table_name {
            continue;
        }

        // Synthetic trigger: only table_name and exec_body matter for
        // TriggerLike; the rest are inert defaults. The local drop
        // stays in this compilation unit (rls.rs), which already
        // handles CreateTable and its sub-types.
        let synthetic = CreateTrigger {
            or_alter: false,
            temporary: false,
            or_replace: false,
            is_constraint: false,
            name: ObjectName(Vec::new()),
            period: Some(TriggerPeriod::Before),
            period_before_table: true,
            events: vec![TriggerEvent::Insert],
            table_name: table.name.clone(),
            referenced_table_name: None,
            referencing: Vec::new(),
            trigger_object: Some(TriggerObjectKind::ForEach(TriggerObject::Row)),
            condition: None,
            exec_body: Some(TriggerExecBody {
                exec_type: TriggerExecBodyType::Function,
                func_desc: FunctionDesc { name: fn_name.clone(), args: None },
            }),
            statements_as: false,
            statements: None,
            characteristics: None,
        };
        if !synthetic.is_maintenance_trigger(schema).unwrap_or(false) {
            continue;
        }

        let per_trigger: Vec<Expr> =
            crate::impls::translator_impls::create_trigger::maintenance_chain(&synthetic, schema)
                .ok()?
                .into_iter()
                .map(|(col, raw_expr)| {
                    let old_expr =
                        crate::impls::translator_impls::create_trigger::substitute_new_with_old(
                            &raw_expr,
                        );
                    let new_col =
                        Expr::CompoundIdentifier(vec![Ident::new("NEW"), Ident::new(&col)]);
                    Expr::IsDistinctFrom(Box::new(new_col), Box::new(old_expr))
                })
                .collect();

        let trigger_matches = per_trigger.into_iter().reduce(|a, b| {
            Expr::BinaryOp { left: Box::new(a), op: BinaryOperator::Or, right: Box::new(b) }
        });

        if let Some(cond) = trigger_matches {
            any_trigger_match = Some(match any_trigger_match {
                None => cond,
                Some(prev) => {
                    Expr::BinaryOp {
                        left: Box::new(prev),
                        op: BinaryOperator::And,
                        right: Box::new(cond),
                    }
                }
            });
        }
    }
    any_trigger_match
}

fn generate_update_check_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Statement>, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    // Triggers cannot tell a blind write from one reading a column.
    let rows = row_security(table, schema, RowStatement::Update { reads: true }, options)?;
    let violation = if rows.has_no_policy(GatedCommand::Update, RowVersion::Existing)
        && rows.has_no_policy(GatedCommand::Update, RowVersion::Updated)
    {
        // The crate's own AFTER INSERT trigger UPDATEs still pass a table with
        // no UPDATE policy, through `build_maintenance_insert_exemption`.
        build_maintenance_insert_exemption(table, schema, options)
    } else {
        let WriteGuard { using, check, read_back, .. } =
            build_write_guard(&rows, GuardKind::Update, table, schema, options, None, emit)?;
        match violation([using, Some(check), read_back].into_iter().flatten()) {
            Violation::Every => None,
            Violation::Rows(None) => return Ok(None),
            Violation::Rows(Some(violation)) => Some(*violation),
        }
    };
    Ok(Some(backing_abort_trigger(
        &format!("{}_update_check", ctx.inner_table_name),
        &ctx.inner_table_name,
        sqlparser::ast::TriggerEvent::Update(Vec::new()),
        write_guard_condition_expr(violation, options),
        &super::definer::exempt_owners(table, schema),
        RLS_VIOLATION,
    )))
}

fn generate_delete_check_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Statement>, Error> {
    if options.get_write_exemption_function().is_none() {
        return Ok(None);
    }
    let ctx = RlsTriggerContext::new(table, options);
    let rows = row_security(table, schema, RowStatement::Delete { reads: true }, options)?;
    let violation = match combine_policy_predicates(
        rows.gate(GatedCommand::Delete, RowVersion::Existing),
        PolicyBindings { prefix: Some("OLD"), substitutions: &[], stored: &[] },
        options,
        table,
        schema,
        emit,
    )? {
        PolicyPredicate::DenyAll => None,
        PolicyPredicate::AllowAll => return Ok(None),
        PolicyPredicate::Expr(predicate) => Some(policy_violation(*predicate)),
    };
    Ok(Some(backing_abort_trigger(
        &format!("{}_delete_check", ctx.inner_table_name),
        &ctx.inner_table_name,
        sqlparser::ast::TriggerEvent::Delete,
        write_guard_condition_expr(violation, options),
        &super::definer::exempt_owners(table, schema),
        RLS_VIOLATION,
    )))
}

fn generated_row_filter(identity: Expr, authorization: Option<Expr>) -> Expr {
    authorization.map_or(identity.clone(), |authorization| {
        Expr::BinaryOp {
            left: Box::new(Expr::Nested(Box::new(identity))),
            op: BinaryOperator::And,
            right: Box::new(Expr::Nested(Box::new(authorization))),
        }
    })
}

fn ignored_write_body(exemption: Option<Expr>) -> Vec<Statement> {
    match exemption {
        Some(call) => {
            vec![ast_builder::raise_statement(
                "IGNORE",
                None,
                Some(Expr::IsNotTrue(Box::new(Expr::Nested(Box::new(call))))),
            )]
        }
        None => {
            vec![ast_builder::select_expression_statement(
                Expr::Value(ValueWithSpan {
                    value: Value::Null,
                    span: sqlparser::tokenizer::Span::empty(),
                }),
                None,
            )]
        }
    }
}

/// The first key column the role cannot read, which leaves an `INSTEAD OF`
/// trigger on the view no way to name the backing row. A table without a
/// primary key is keyed by every column.
///
/// # Errors
///
/// Propagates a schema lookup failure.
fn unreadable_key_column(
    table: &CreateTable,
    schema: &ParserDB,
    grants: &RoleTable,
) -> Result<Option<String>, Error> {
    let primary_key = collect_pk_column_names(table, schema)?;
    let key =
        if primary_key.is_empty() { collect_column_names(table, schema)? } else { primary_key };
    Ok(key.into_iter().find(|column| !grants.reads(column)))
}

fn view_trigger(
    name: &str,
    table_name: &str,
    event: TriggerEvent,
    body: Vec<Statement>,
) -> Statement {
    ast_builder::trigger(
        generated_object_name(name),
        generated_object_name(table_name),
        TriggerPeriod::InsteadOf,
        event,
        true,
        None,
        body,
    )
}

/// The forwarding `INSTEAD OF UPDATE` trigger, then the one refusing an
/// assignment to a column the role may not update, which SQLite fires first
/// since it fires the newest trigger first, as PostgreSQL checks privileges
/// before policies.
fn generate_update_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    grants: &RoleTable,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let table_name = ctx.table_name;
    let trigger_name = format!("{table_name}_update_trigger");
    let event = || TriggerEvent::Update(Vec::new());
    let main = if let Some(column) = unreadable_key_column(table, schema, grants)? {
        let refusal = unreadable_key(table_name, &column);
        view_trigger(
            &trigger_name,
            table_name,
            event(),
            vec![ast_builder::raise_statement("ABORT", Some(&refusal), None)],
        )
    } else {
        view_trigger(
            &trigger_name,
            table_name,
            event(),
            update_body(table, schema, options, grants, emit)?,
        )
    };
    let mut triggers = vec![main];
    let unwritable: Vec<Ident> = grants.readable_not_updatable().map(quoted_ident).collect();
    if !unwritable.is_empty() {
        triggers.push(view_trigger(
            &format!("{table_name}_update_grant_trigger"),
            table_name,
            TriggerEvent::Update(unwritable),
            vec![permission_gate(table_name, None, options)],
        ));
    }
    Ok(triggers)
}

fn update_body(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    grants: &RoleTable,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let table_name = ctx.table_name;
    // Triggers cannot tell a blind write from one reading a column.
    let rows = row_security(table, schema, RowStatement::Update { reads: true }, options)?;
    let columns = collect_column_names(table, schema)?;
    let primary_key = collect_pk_column_names(table, schema)?;
    let WriteGuard { columns: assigned, using, check, read_back } =
        build_write_guard(&rows, GuardKind::Update, table, schema, options, Some(grants), emit)?;
    let assignments = assigned
        .iter()
        .filter(|column| column.is_forwarded())
        .map(|column| {
            Assignment {
                target: AssignmentTarget::ColumnName(generated_object_name(&column.name)),
                value: prefixed_column_expr("NEW", &column.name),
            }
        })
        .collect();
    let using = using.expect("update guard always has a USING predicate");
    let using_predicate = match &using {
        PolicyPredicate::Expr(predicate) => Some(predicate.as_ref().clone()),
        PolicyPredicate::AllowAll | PolicyPredicate::DenyAll => None,
    };
    let using_denies = rows.has_no_policy(GatedCommand::Update, RowVersion::Existing)
        || matches!(using, PolicyPredicate::DenyAll);
    let authorization = if using_denies {
        write_exemption_expr(options)
            .map(|call| Expr::IsTrue(Box::new(Expr::Nested(Box::new(call)))))
    } else {
        using_predicate.clone().map(|predicate| policy_or_exemption_expr(predicate, options))
    };
    let forward = ast_builder::update(
        generated_object_name(&ctx.inner_table_name),
        assignments,
        Some(generated_row_filter(
            build_row_identity_clause(&columns, &primary_key),
            authorization,
        )),
    );
    let mut forwarded = refuse_computed_write_statements(&assigned, table_name, GuardKind::Update);
    forwarded.push(forward);

    Ok(if using_denies {
        if options.is_strict_rls_write_deny() {
            let mut body = vec![ast_builder::raise_statement(
                "ABORT",
                Some(&no_policy(WriteCommand::Update, table_name)),
                write_guard_condition_expr(None, options),
            )];
            body.extend(forwarded);
            body
        } else {
            let exemption = write_exemption_expr(options);
            let mut body = ignored_write_body(exemption.clone());
            if exemption.is_some() {
                body.extend(forwarded);
            }
            body
        }
    } else {
        // The updated row has to pass `WITH CHECK` and the `SELECT` policies.
        let mut body = new_row_guard(check, read_back, using_predicate, options)
            .into_iter()
            .collect::<Vec<_>>();
        body.extend(forwarded);
        body
    })
}

fn generate_delete_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    grants: &RoleTable,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Statement, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let table_name = ctx.table_name;
    // PostgreSQL checks privileges before policies.
    let mut body: Vec<Statement> = (!grants.deletes())
        .then(|| permission_gate(table_name, None, options))
        .into_iter()
        .collect();
    if let Some(column) = unreadable_key_column(table, schema, grants)? {
        body.push(ast_builder::raise_statement(
            "ABORT",
            Some(&unreadable_key(table_name, &column)),
            None,
        ));
    } else {
        body.extend(delete_body(table, schema, options, grants, emit)?);
    }
    Ok(view_trigger(
        &format!("{table_name}_delete_trigger"),
        table_name,
        TriggerEvent::Delete,
        body,
    ))
}

fn delete_body(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    grants: &RoleTable,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let table_name = ctx.table_name;
    let rows = row_security(table, schema, RowStatement::Delete { reads: true }, options)?;
    let columns = collect_column_names(table, schema)?;
    let primary_key = collect_pk_column_names(table, schema)?;
    let on_view = trigger_columns(table, schema, options, Some(grants))?;
    let stored = stored_substitutions(&on_view, table, schema, options)?;
    let using = combine_policy_predicates(
        rows.gate(GatedCommand::Delete, RowVersion::Existing),
        PolicyBindings { prefix: Some("OLD"), substitutions: &[], stored: &stored },
        options,
        table,
        schema,
        emit,
    )?;
    let using_denies = rows.has_no_policy(GatedCommand::Delete, RowVersion::Existing)
        || matches!(using, PolicyPredicate::DenyAll);
    let authorization = if using_denies {
        write_exemption_expr(options)
            .map(|call| Expr::IsTrue(Box::new(Expr::Nested(Box::new(call)))))
    } else {
        match &using {
            PolicyPredicate::Expr(predicate) => {
                Some(policy_or_exemption_expr(predicate.as_ref().clone(), options))
            }
            PolicyPredicate::AllowAll | PolicyPredicate::DenyAll => None,
        }
    };
    let forward = ast_builder::delete(
        generated_object_name(&ctx.inner_table_name),
        Some(generated_row_filter(
            build_row_identity_clause(&columns, &primary_key),
            authorization,
        )),
    );
    Ok(if using_denies {
        if options.is_strict_rls_write_deny() {
            vec![
                ast_builder::raise_statement(
                    "ABORT",
                    Some(&no_policy(WriteCommand::Delete, table_name)),
                    write_guard_condition_expr(None, options),
                ),
                forward,
            ]
        } else {
            let exemption = write_exemption_expr(options);
            let mut body = ignored_write_body(exemption.clone());
            if exemption.is_some() {
                body.push(forward);
            }
            body
        }
    } else {
        vec![forward]
    })
}

fn generate_readonly_backing_guard_sql(
    table: &CreateTable,
    options: &crate::options::TranslationContext<'_>,
) -> Vec<Statement> {
    if options.get_write_exemption_function().is_none() {
        return Vec::new();
    }
    let ctx = RlsTriggerContext::new(table, options);
    let condition = write_guard_condition_expr(None, options);
    let message = read_only(ctx.table_name);
    [
        ("insert", sqlparser::ast::TriggerEvent::Insert),
        ("update", sqlparser::ast::TriggerEvent::Update(Vec::new())),
        ("delete", sqlparser::ast::TriggerEvent::Delete),
    ]
    .into_iter()
    .map(|(verb, event)| {
        backing_abort_trigger(
            &format!("{}_{}_check", ctx.inner_table_name, verb),
            &ctx.inner_table_name,
            event,
            condition.clone(),
            // The owner's privileges are not modelled, so a table the session
            // role only reads stays closed to definer bodies too.
            &[],
            &message,
        )
    })
    .collect()
}

/// Generates the view standing for a table with a backing table, its write
/// triggers as `grants` allow, and, under row-level security, its policy
/// guards and validation monitors.
///
/// # Errors
///
/// Returns an error if table metadata or policy expressions cannot be
/// resolved, or a table with row-level security has no audit table.
pub(crate) fn generate_view_statements(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    grants: &RoleTable,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let audit_table_name = if table.has_row_level_security(schema)? {
        Some(options.get_rls_audit_table_name().ok_or(Error::RlsAuditTableNameRequired)?)
    } else {
        None
    };
    let unreadable_key = unreadable_key_column(table, schema, grants)?;

    let mut statements =
        vec![generate_rls_view_statement_with_context(table, schema, options, grants, emit)?];

    if grants.is_read_only() {
        statements.extend(generate_readonly_backing_guard_sql(table, options));
    } else {
        if let Some(column) = &unreadable_key
            && (grants.updates_any() || grants.deletes())
        {
            emit(crate::warnings::TranslationWarning::UnreadableRowKey {
                table: table.table_name().to_owned(),
                column: column.clone(),
            });
        }
        statements.push(generate_insert_trigger_sql(table, schema, options, grants, emit)?);

        // Backing-table guards cover the writes that reach the backing table
        // directly, the RETURNING and ON CONFLICT redirects among them.
        if let Some(check) = generate_insert_check_trigger_sql(table, schema, options, emit)? {
            statements.push(check);
        }
        if let Some(check) = generate_update_check_trigger_sql(table, schema, options, emit)? {
            statements.push(check);
        }
        if let Some(check) = generate_delete_check_trigger_sql(table, schema, options, emit)? {
            statements.push(check);
        }

        statements.extend(generate_update_trigger_sql(table, schema, options, grants, emit)?);
        statements.push(generate_delete_trigger_sql(table, schema, options, grants, emit)?);
    }

    let Some(audit_table_name) = audit_table_name else { return Ok(statements) };
    // A deny-all view makes the monitor useless rather than strict: its check
    // asks whether a backing row is visible through the view, which is
    // always no here, so it would flag every write and distinguish nothing.
    // Report the configuration once now instead of once per row at runtime.
    if matches!(rls_read_predicate(table, schema, options, emit)?, PolicyPredicate::DenyAll) {
        emit(crate::warnings::TranslationWarning::RlsDeniesEveryRow {
            table: table.table_name().to_owned(),
        });
    } else if unreadable_key.is_none() {
        // The monitor finds a written row in the view by its key.
        let validation_stmts =
            generate_rls_validation_statements(table, schema, options, audit_table_name)?;
        statements.extend(validation_stmts);
    }

    Ok(statements)
}

/// Generates all RLS-related SQL statements for a table, for a caller holding
/// every privilege.
///
/// # Errors
///
/// Returns an error if table metadata or policy expressions cannot be resolved.
pub(crate) fn generate_rls_statements_with_context(
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let grants = RoleTable::of(table, None, schema)?;
    generate_view_statements(table, schema, options, &grants, emit)
}
/// Generates row security statements from caller configuration.
#[cfg(test)]
fn generate_rls_statements(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let context = crate::options::TranslationContext::new(options);
    generate_rls_statements_with_context(table, schema, &context, emit)
}

/// Renames a CREATE TABLE statement to use the inner table name for RLS.
/// Also updates any foreign key references to other RLS tables.
#[must_use]
pub(crate) fn rename_table_for_rls(
    create_table: &CreateTable,
    options: &Pg2SqliteOptions,
    _schema: &ParserDB,
) -> CreateTable {
    let suffix = options.get_rls_table_suffix();
    let mut renamed = create_table.clone();
    renamed.name = append_suffix(&renamed.name, suffix);

    renamed
}

fn generated_column(table: &str, column: &str) -> Expr {
    Expr::CompoundIdentifier(vec![quoted_ident(table), quoted_ident(column)])
}

fn row_identifier_expr(pk_columns: &[String], prefix: &str) -> Expr {
    let mut expressions = pk_columns.iter().map(|column| {
        concat(
            string_literal(&format!("{column}=")),
            simple_function_expr("quote", vec![prefixed_column_expr(prefix, column)], None),
        )
    });
    let Some(first) = expressions.next() else {
        return string_literal("<no PK>");
    };
    expressions
        .fold(first, |joined, expression| concat(concat(joined, string_literal(", ")), expression))
}

fn row_visibility_expr(table_name: &str, pk_columns: &[String], prefix: &str) -> Expr {
    let selection = pk_columns
        .iter()
        .map(|column| {
            Expr::BinaryOp {
                left: Box::new(generated_column(table_name, column)),
                op: BinaryOperator::Eq,
                right: Box::new(prefixed_column_expr(prefix, column)),
            }
        })
        .reduce(|left, right| {
            Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::And, right: Box::new(right) }
        })
        .unwrap_or_else(|| {
            Expr::BinaryOp {
                left: Box::new(integer_literal(1)),
                op: BinaryOperator::Eq,
                right: Box::new(integer_literal(1)),
            }
        });
    Expr::Exists {
        subquery: Box::new(single_expr_query(
            integer_literal(1),
            from_relation(plain_table_factor(generated_object_name(table_name))),
            Some(selection),
        )),
        negated: false,
    }
}

fn generate_monitoring_trigger_statement(
    table_name: &str,
    inner_table_name: &str,
    pk_columns: &[String],
    audit_table_name: &str,
    strict_mode: bool,
    operation: &str,
) -> Statement {
    let event =
        if operation == "insert" { TriggerEvent::Insert } else { TriggerEvent::Update(Vec::new()) };
    let past_participle = if operation == "insert" { "inserted into" } else { "updated in" };
    let details = format!(
        "Row {past_participle} backing table but not readable through the RLS view\x3b \
         PostgreSQL allows this for writes without RETURNING"
    );
    let source = make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            ast_builder::select_items(vec![
                string_literal(table_name),
                string_literal("select_not_visible"),
                row_identifier_expr(pk_columns, "NEW"),
                string_literal("SELECT policy"),
                simple_function_expr("datetime", vec![string_literal("now")], None),
                string_literal(if strict_mode { "error" } else { "warning" }),
                string_literal(&details),
                Expr::Value(ValueWithSpan {
                    value: Value::Null,
                    span: sqlparser::tokenizer::Span::empty(),
                }),
            ]),
            Vec::new(),
            Some(Expr::UnaryOp {
                op: UnaryOperator::Not,
                expr: Box::new(Expr::Nested(Box::new(row_visibility_expr(
                    table_name, pk_columns, "NEW",
                )))),
            }),
        ))),
    );
    let audit_columns = [
        "table_name",
        "violation_type",
        "row_identifier",
        "policy_name",
        "detected_at",
        "severity",
        "details",
        "reported_at",
    ]
    .into_iter()
    .map(generated_object_name)
    .collect();
    ast_builder::trigger(
        generated_object_name(&format!("{inner_table_name}_rls_monitor_{operation}")),
        generated_object_name(inner_table_name),
        TriggerPeriod::After,
        event,
        true,
        None,
        vec![ast_builder::insert(generated_object_name(audit_table_name), audit_columns, source)],
    )
}

fn generate_validation_view_statement(
    table_name: &str,
    inner_table_name: &str,
    columns: &[String],
    pk_columns: &[String],
) -> Statement {
    let match_columns = if pk_columns.is_empty() { columns } else { pk_columns };
    let selection = match_columns
        .iter()
        .map(|column| {
            Expr::BinaryOp {
                left: Box::new(generated_column(inner_table_name, column)),
                op: BinaryOperator::Eq,
                right: Box::new(generated_column(table_name, column)),
            }
        })
        .reduce(|left, right| {
            Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::And, right: Box::new(right) }
        })
        .expect("a table has at least one column");
    let visible_row = Expr::Exists {
        subquery: Box::new(single_expr_query(
            integer_literal(1),
            from_relation(plain_table_factor(generated_object_name(table_name))),
            Some(selection),
        )),
        negated: true,
    };
    let query = make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            columns
                .iter()
                .map(|column| SelectItem::UnnamedExpr(Expr::Identifier(quoted_ident(column))))
                .collect(),
            from_relation(plain_table_factor(generated_object_name(inner_table_name))),
            Some(visible_row),
        ))),
    );
    ast_builder::create_view(
        generated_object_name(&format!("{inner_table_name}_violations")),
        Vec::new(),
        query,
    )
}

/// A column of a table the translation adds.
pub(crate) fn generated_table_column(name: &str, data_type: DataType, not_null: bool) -> ColumnDef {
    let options = if not_null {
        vec![ColumnOptionDef { name: None, option: ColumnOption::NotNull }]
    } else {
        Vec::new()
    };
    ColumnDef { name: quoted_ident(name), data_type, options }
}

fn audit_id_column() -> ColumnDef {
    let words = ["PRIMARY", "KEY", "AUTOINCREMENT"]
        .into_iter()
        .map(|value| {
            Token::Word(Word {
                value: value.to_string(),
                quote_style: None,
                keyword: sqlparser::keywords::Keyword::NoKeyword,
            })
        })
        .collect();
    ColumnDef {
        name: quoted_ident("id"),
        data_type: DataType::Integer(None),
        options: vec![ColumnOptionDef { name: None, option: ColumnOption::DialectSpecific(words) }],
    }
}

/// Generates the complete set of RLS validation statements for a table.
///
/// # Errors
///
/// Returns an error if the table metadata cannot be resolved.
pub fn generate_rls_validation_statements(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    audit_table_name: &str,
) -> Result<Vec<Statement>, Error> {
    let table_name = table.table_name();
    let inner_table_name = format!("{}{}", table_name, options.get_rls_table_suffix());
    let pk_columns = collect_pk_column_names(table, schema)?;
    let all_columns = collect_column_names(table, schema)?;
    let strict_mode = options.is_strict_rls_validation();

    Ok(vec![
        generate_monitoring_trigger_statement(
            table_name,
            &inner_table_name,
            &pk_columns,
            audit_table_name,
            strict_mode,
            "insert",
        ),
        generate_monitoring_trigger_statement(
            table_name,
            &inner_table_name,
            &pk_columns,
            audit_table_name,
            strict_mode,
            "update",
        ),
        generate_validation_view_statement(
            table_name,
            &inner_table_name,
            &all_columns,
            &pk_columns,
        ),
    ])
}

/// Builds the RLS audit table statement.
pub fn generate_rls_audit_table(audit_table_name: &str) -> Statement {
    ast_builder::create_table(
        generated_object_name(audit_table_name),
        vec![
            audit_id_column(),
            generated_table_column("table_name", DataType::Text, true),
            generated_table_column("violation_type", DataType::Text, true),
            generated_table_column("row_identifier", DataType::Text, true),
            generated_table_column("policy_name", DataType::Text, false),
            generated_table_column("detected_at", DataType::Text, true),
            generated_table_column("severity", DataType::Text, true),
            generated_table_column("details", DataType::Text, false),
            generated_table_column("reported_at", DataType::Text, false),
        ],
    )
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::{
        structs::{IdentifierCase, ParserDB, TargetName},
        traits::{ColumnLike, DatabaseLike, TableLike},
    };

    /// Resolves the column set the transformer needs, exactly as production
    /// callers do, so these tests exercise the same shapes.
    fn resolved_sets(table: &<ParserDB as DatabaseLike>::Table, schema: &ParserDB) -> Vec<String> {
        TableLike::columns(table, schema)
            .expect("columns must resolve")
            .map(|c| c.column_name().to_lowercase())
            .collect()
    }
    use sqlparser::{
        ast::{
            Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgOperator,
            FunctionArgumentList, FunctionArguments, Ident, JoinConstraint, JoinOperator,
            ObjectName, ObjectNamePart, SetExpr, Statement, TableFactor,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        ResolvedSchemaFacts, RoleTable, SubqueryTransformContext, generate_delete_trigger_sql,
        generate_insert_trigger_sql, generate_rls_audit_table, generate_rls_statements,
        generate_rls_validation_statements, generate_update_trigger_sql, generate_view_statements,
        rename_table_for_rls, transform_expr, transform_join_operator_for_subquery,
        transform_query, transform_table_factor_for_subquery, validate_session_variables,
        validate_table_policies,
    };
    use crate::{
        impls::{
            function_helpers::single_quoted_literal,
            session_variable::{self, SessionRead},
            test_fixtures::{parse_expr, parse_query},
        },
        prelude::Pg2SqliteOptions,
        traits::translation_options::SessionVariableMapping,
    };

    fn parse_statements(sql: &str) -> Vec<Statement> {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
    }

    fn schema_from_sql(sql: &str) -> ParserDB {
        ParserDB::from_statements(parse_statements(sql), "test".to_string())
            .expect("schema should build")
    }

    /// The table named `name` in the fixture schema of an RLS policy test.
    fn policy_table<'a>(schema: &'a ParserDB, name: &str) -> &'a <ParserDB as DatabaseLike>::Table {
        schema
            .table_by_target(TargetName::new(name, false), IdentifierCase::AsWritten)
            .expect("table lookup should succeed")
            .expect("table should exist")
    }

    #[test]
    fn extract_helpers_cover_string_literal_and_current_setting_edge_paths() {
        assert_eq!(
            single_quoted_literal(&Expr::Value(sqlparser::ast::ValueWithSpan::from(
                sqlparser::ast::Value::SingleQuotedString("x".to_string()),
            ))),
            Some("x")
        );
        assert!(
            single_quoted_literal(&Expr::Value(sqlparser::ast::ValueWithSpan::from(
                sqlparser::ast::Value::Boolean(true),
            )))
            .is_none()
        );
        assert!(single_quoted_literal(&parse_expr("other_col")).is_none());

        let not_setting = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("other"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::None,
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert!(session_variable::session_read_of_function(&not_setting).is_none());

        let invalid_arg = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("current_setting"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Named {
                    name: Ident::new("x"),
                    arg: FunctionArgExpr::Wildcard,
                    operator: FunctionArgOperator::RightArrow,
                }],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert!(session_variable::session_read_of_function(&invalid_arg).is_none());

        let named_expr = Function {
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Named {
                    name: Ident::new("setting"),
                    arg: FunctionArgExpr::Expr(Expr::Value(sqlparser::ast::ValueWithSpan::from(
                        sqlparser::ast::Value::SingleQuotedString("app.user_id".to_string()),
                    ))),
                    operator: FunctionArgOperator::RightArrow,
                }],
                clauses: vec![],
            }),
            ..invalid_arg
        };
        assert_eq!(
            session_variable::session_read_of_function(&named_expr),
            Some(SessionRead::Setting("app.user_id".to_string()))
        );

        let current_setting_no_args = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("current_setting"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::None,
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert!(session_variable::session_read_of_function(&current_setting_no_args).is_none());
    }

    /// The transformer owns column references and table renames. The caller's
    /// identity is not its business any more: the expression translator
    /// substitutes it for every statement kind, so a policy and a query cannot
    /// disagree about it.
    #[test]
    fn transform_expr_covers_cast_and_rename_paths() {
        let schema = schema_from_sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, title TEXT);",
        );
        let table = policy_table(&schema, "docs");
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        let keyword = transform_expr(
            &parse_expr("current_user"),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
            facts,
        );
        assert_eq!(
            keyword.to_string(),
            "current_user",
            "the keyword is left for the translator, which answers it from the deployment"
        );

        // PostgreSQL reads a quoted `"current_user"` as a column name, so the
        // transformer must not mistake it for the keyword. It is not a column
        // of this table, so it takes no prefix either.
        let quoted = transform_expr(
            &parse_expr("\"current_user\""),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
            facts,
        );
        assert_eq!(quoted.to_string(), "\"current_user\"");

        let transformed_cast = transform_expr(
            &parse_expr("owner_id::INT"),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
            facts,
        );
        assert!(transformed_cast.to_string().contains("NEW.owner_id"));

        let renamed = transform_expr(
            &parse_expr("docs.owner_id"),
            &options,
            table,
            &schema,
            None,
            Some(("docs", "docs_rls")),
            facts,
        );
        assert_eq!(renamed.to_string(), "docs_rls.owner_id");

        let prefixed_identifier = transform_expr(
            &parse_expr("owner_id"),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
            facts,
        );
        assert_eq!(prefixed_identifier.to_string(), "NEW.owner_id");
    }

    #[test]
    fn transform_expr_transforms_function_parameters_like_args() {
        let schema = schema_from_sql(
            "CREATE TABLE reports(id INTEGER PRIMARY KEY, tenant TEXT, amount INTEGER, weight INTEGER);",
        );
        let table = policy_table(&schema, "reports");
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        let call = Expr::Function(Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("quantile"))]),
            uses_odbc_syntax: false,
            parameters: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(
                    Ident::new("weight"),
                )))],
                clauses: vec![],
            }),
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(
                    Ident::new("amount"),
                )))],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
        });

        let transformed = transform_expr(&call, &options, table, &schema, Some("NEW"), None, facts);
        assert_eq!(transformed.to_string(), "quantile(NEW.weight)(NEW.amount)");
    }

    #[test]
    fn transform_query_and_subquery_helpers_cover_projection_and_join_paths() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            CREATE TABLE teams(id INTEGER PRIMARY KEY, owner_id INTEGER);
            "#,
        );
        let table = policy_table(&schema, "docs");
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        let mut wildcard_query = parse_query("SELECT * FROM docs");
        let SetExpr::Select(select) = wildcard_query.body.as_mut() else {
            panic!("expected select");
        };
        select.qualify = Some(parse_expr("id > 0"));
        let transformed = transform_query(
            &wildcard_query,
            &options,
            table,
            &schema,
            Some("NEW"),
            Some(("docs", "docs_rls")),
            facts,
        );
        assert!(transformed.to_string().contains("QUALIFY"));

        let context = SubqueryTransformContext {
            options: &options,
            table,
            schema: &schema,
            prefix: Some("NEW"),
            outer_table: Some(("docs", "docs_rls")),
            lowercased_columns: &lowercased_columns,
        };

        let mut rename_pairs = Vec::new();
        let mut already_suffixed = TableFactor::Table {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("docs_rls"))]),
            alias: None,
            args: None,
            with_hints: vec![],
            version: None,
            with_ordinality: false,
            partitions: vec![],
            json_path: None,
            sample: None,
            index_directives: vec![],
            inheritance: None,
        };
        transform_table_factor_for_subquery(&mut already_suffixed, &context, &mut rename_pairs);
        assert_eq!(rename_pairs, vec![("docs_rls".to_string(), "docs_rls".to_string())]);

        let mut table_function =
            TableFactor::TableFunction { expr: parse_expr("generate_series(1, 2)"), alias: None };
        transform_table_factor_for_subquery(&mut table_function, &context, &mut rename_pairs);
        assert!(matches!(table_function, TableFactor::TableFunction { .. }));

        let on_constraint = JoinConstraint::On(parse_expr("docs.id = teams.id"));
        let mut join_variants = vec![
            JoinOperator::Join(on_constraint.clone()),
            JoinOperator::Inner(on_constraint.clone()),
            JoinOperator::Left(on_constraint.clone()),
            JoinOperator::LeftOuter(on_constraint.clone()),
            JoinOperator::Right(on_constraint.clone()),
            JoinOperator::RightOuter(on_constraint.clone()),
            JoinOperator::FullOuter(on_constraint.clone()),
            JoinOperator::CrossJoin(on_constraint.clone()),
            JoinOperator::Semi(on_constraint.clone()),
            JoinOperator::LeftSemi(on_constraint.clone()),
            JoinOperator::RightSemi(on_constraint.clone()),
            JoinOperator::Anti(on_constraint.clone()),
            JoinOperator::LeftAnti(on_constraint.clone()),
            JoinOperator::RightAnti(on_constraint.clone()),
            JoinOperator::StraightJoin(on_constraint.clone()),
        ];
        for join_op in &mut join_variants {
            transform_join_operator_for_subquery(join_op, &context, &rename_pairs);
        }

        let mut as_of = JoinOperator::AsOf {
            constraint: on_constraint.clone(),
            match_condition: parse_expr("docs.id > teams.id"),
        };
        transform_join_operator_for_subquery(&mut as_of, &context, &rename_pairs);
        assert!(matches!(as_of, JoinOperator::AsOf { .. }));

        let mut cross_apply = JoinOperator::CrossApply;
        transform_join_operator_for_subquery(&mut cross_apply, &context, &rename_pairs);
        let mut outer_apply = JoinOperator::OuterApply;
        transform_join_operator_for_subquery(&mut outer_apply, &context, &rename_pairs);
    }

    #[test]
    fn transform_table_factor_for_subquery_does_not_downgrade_three_part_names() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            "#,
        );
        let table = policy_table(&schema, "docs");
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let lowercased_columns = resolved_sets(table, &schema);
        let context = SubqueryTransformContext {
            options: &options,
            table,
            schema: &schema,
            prefix: Some("NEW"),
            outer_table: Some(("docs", "docs_rls")),
            lowercased_columns: &lowercased_columns,
        };

        let mut rename_pairs = Vec::new();
        let mut three_part = TableFactor::Table {
            name: ObjectName(vec![
                ObjectNamePart::Identifier(Ident::new("catalog")),
                ObjectNamePart::Identifier(Ident::new("public")),
                ObjectNamePart::Identifier(Ident::new("docs")),
            ]),
            alias: None,
            args: None,
            with_hints: vec![],
            version: None,
            with_ordinality: false,
            partitions: vec![],
            json_path: None,
            sample: None,
            index_directives: vec![],
            inheritance: None,
        };
        transform_table_factor_for_subquery(&mut three_part, &context, &mut rename_pairs);

        let TableFactor::Table { name, .. } = three_part else {
            panic!("expected Table variant");
        };
        assert_eq!(name.to_string(), "catalog.public.docs");
        assert_eq!(rename_pairs, vec![("docs".to_string(), "docs".to_string())]);
    }

    #[test]
    fn transform_expr_covers_prefix_and_rename_strategy_combinations() {
        let schema = schema_from_sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);",
        );
        let table = policy_table(&schema, "docs");
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        // Bare identifier, prefix applied.
        let prefixed_ident = transform_expr(
            &parse_expr("owner_id"),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
            facts,
        );
        assert_eq!(prefixed_ident.to_string(), "NEW.owner_id");

        // Qualified identifier, rename applied with no prefix.
        let renamed_compound = transform_expr(
            &parse_expr("docs.owner_id"),
            &options,
            table,
            &schema,
            None,
            Some(("docs", "docs_inner")),
            facts,
        );
        assert_eq!(renamed_compound.to_string(), "docs_inner.owner_id");

        // Qualified identifier with BOTH a prefix and a rename: the prefix
        // wins, which is the branch the collapsed strategy resolves via
        // `prefix.unwrap_or(new_name)`.
        let prefixed_over_rename = transform_expr(
            &parse_expr("docs.owner_id"),
            &options,
            table,
            &schema,
            Some("NEW"),
            Some(("docs", "docs_inner")),
            facts,
        );
        assert_eq!(prefixed_over_rename.to_string(), "NEW.owner_id");
    }

    #[test]
    fn transform_expr_identifier_and_compound_strategy_branches_are_exercised() {
        let schema = schema_from_sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);",
        );
        let table = policy_table(&schema, "docs");
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        let prefixed_identifier = transform_expr(
            &Expr::Identifier(Ident::new("owner_id")),
            &options,
            table,
            &schema,
            Some("OLD"),
            None,
            facts,
        );
        assert_eq!(prefixed_identifier.to_string(), "OLD.owner_id");

        let renamed_compound = transform_expr(
            &Expr::CompoundIdentifier(vec![Ident::new("docs"), Ident::new("owner_id")]),
            &options,
            table,
            &schema,
            None,
            Some(("docs", "docs_inner")),
            facts,
        );
        assert_eq!(renamed_compound.to_string(), "docs_inner.owner_id");

        let prefixed_compound = transform_expr(
            &Expr::CompoundIdentifier(vec![Ident::new("docs"), Ident::new("owner_id")]),
            &options,
            table,
            &schema,
            Some("OLD"),
            Some(("docs", "docs_inner")),
            facts,
        );
        assert_eq!(prefixed_compound.to_string(), "OLD.owner_id");
    }

    #[test]
    fn validate_session_variable_and_policy_paths_cover_error_and_success_cases() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id = current_setting('app.user_id')::INT);
            "#,
        );
        let table = policy_table(&schema, "docs");

        let missing = Pg2SqliteOptions::default();
        let err = validate_session_variables(
            &parse_expr("owner_id = current_setting('app.user_id')::INT"),
            &missing,
            "docs",
            "docs_select",
        )
        .expect_err("missing current_setting mapping should error");
        assert!(err.to_string().contains("current_setting('app.user_id')"));

        let err = validate_session_variables(
            &parse_expr("current_user = 'alice'"),
            &missing,
            "docs",
            "docs_select",
        )
        .expect_err("missing current_user mapping should error");
        assert!(err.to_string().contains("current_user"));

        let deployment = sql_traits::structs::Deployment::new(
            "app",
            ["alice"],
            [sql_traits::structs::SessionSetting::value(
                "app.user_id",
                sql_traits::structs::SettingValue::Caller(sqlparser::ast::DataType::Int(None)),
            )
            .expect("a custom setting name")],
        )
        .expect("a valid deployment");
        let mapped = Pg2SqliteOptions::default()
            .with_deployment(deployment)
            .with_acting_role("alice")
            .with_session_variable(SessionVariableMapping::new("app.user_id", "sqlite_user_id"));
        validate_session_variables(
            &parse_expr("owner_id = current_setting('app.user_id')::INT"),
            &mapped,
            "docs",
            "docs_select",
        )
        .expect("mapped current_setting should pass");
        validate_session_variables(
            &parse_expr("current_user = 'alice'"),
            &mapped,
            "docs",
            "docs_select",
        )
        .expect("a named acting role should pass");

        validate_table_policies(table, &schema, &mapped).expect("policy validation should pass");
    }

    #[test]
    fn query_and_trigger_generation_helpers_cover_policy_paths() -> Result<(), crate::errors::Error>
    {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);
            CREATE TABLE teams(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (owner_id > 0);
            CREATE POLICY docs_update ON docs FOR UPDATE USING (owner_id > 0) WITH CHECK (owner_id > 0);
            CREATE POLICY docs_delete ON docs FOR DELETE USING (owner_id > 0);
            "#,
        );
        let table = policy_table(&schema, "docs");
        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_rls_audit_table_name("rls_audit")
                .with_strict_rls_validation(),
        );
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        let transformed_query = transform_query(
            &parse_query(
                "SELECT docs.owner_id + 1 AS owner_plus \
                 FROM docs INNER JOIN teams ON docs.owner_id = teams.owner_id \
                 WHERE docs.owner_id > 0 \
                 GROUP BY docs.owner_id \
                 HAVING docs.owner_id > 1 \
                 QUALIFY docs.owner_id > 2",
            ),
            &options,
            table,
            &schema,
            Some("NEW"),
            Some(("docs", "docs_rls")),
            facts,
        );
        let transformed_sql = transformed_query.to_string();
        assert!(transformed_sql.contains("NEW.owner_id"));
        assert!(transformed_sql.contains("QUALIFY"));

        let grants = RoleTable::of(table, None, &schema)?;
        let insert_trigger_sql =
            generate_insert_trigger_sql(table, &schema, &options, &grants, &mut |_| {})?
                .to_string();
        assert!(insert_trigger_sql.contains("docs_insert_trigger"));
        assert!(insert_trigger_sql.contains("RAISE(ABORT"));

        let update_trigger_sql =
            generate_update_trigger_sql(table, &schema, &options, &grants, &mut |_| {})?
                .iter()
                .map(ToString::to_string)
                .collect::<String>();
        assert!(update_trigger_sql.contains("docs_update_trigger"));
        assert!(update_trigger_sql.contains("owner_id = NEW.owner_id"));
        assert!(
            !update_trigger_sql.contains("COALESCE"),
            "the SET clause must assign NEW.col directly, not COALESCE(NEW.col, OLD.col)"
        );

        let delete_trigger_sql =
            generate_delete_trigger_sql(table, &schema, &options, &grants, &mut |_| {})?
                .to_string();
        assert!(delete_trigger_sql.contains("docs_delete_trigger"));

        Ok(())
    }

    #[test]
    fn rls_statement_generation_paths_cover_readonly_and_validation_helpers() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);
            CREATE ROLE reader;
            GRANT SELECT ON docs TO reader;
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (owner_id > 0);
            CREATE POLICY docs_update ON docs FOR UPDATE USING (owner_id > 0) WITH CHECK (owner_id > 0);
            CREATE POLICY docs_delete ON docs FOR DELETE USING (owner_id > 0);
            "#,
        );
        let table = policy_table(&schema, "docs");
        let reader = RoleTable::of(table, schema.role("reader"), &schema)
            .expect("the reader's grants resolve");

        let missing_audit =
            crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let err = generate_rls_statements(table, &schema, &missing_audit, &mut |_| {})
            .expect_err("missing audit table should error");
        assert!(err.to_string().contains("RLS audit table name"));
        let err = generate_view_statements(table, &schema, &missing_audit, &reader, &mut |_| {})
            .expect_err("missing audit table should error");
        assert!(err.to_string().contains("RLS audit table name"));

        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default()
                .with_rls_audit_table_name("rls_audit")
                .with_strict_rls_validation(),
        );
        let statements = generate_rls_statements(table, &schema, &options, &mut |_| {})
            .expect("full RLS statements should build");
        assert!(!statements.is_empty(), "a guarded table emits at least one statement");
        assert!(
            statements
                .iter()
                .any(|stmt| stmt.to_string().contains("CREATE TRIGGER docs_insert_trigger"))
        );

        let readonly = generate_view_statements(table, &schema, &options, &reader, &mut |_| {})
            .expect("readonly RLS should build");
        assert!(!readonly.is_empty(), "a read-only table emits at least one statement");
        assert!(!readonly.iter().any(|stmt| stmt.to_string().contains("docs_insert_trigger")));

        let validation = generate_rls_validation_statements(table, &schema, &options, "rls_audit")
            .expect("validation statements should build");
        assert!(!validation.is_empty(), "validation emits at least one statement");
        assert!(
            validation
                .iter()
                .any(|stmt| stmt.to_string().contains("CREATE VIEW docs_rls_violations"))
        );

        let audit_table = generate_rls_audit_table("rls_audit");
        assert!(audit_table.to_string().contains("CREATE TABLE"));

        let create_table_stmt =
            parse_statements("CREATE TABLE docs(id INTEGER PRIMARY KEY)").remove(0);
        let Statement::CreateTable(create_table) = create_table_stmt else {
            panic!("expected create table");
        };
        let renamed = rename_table_for_rls(&create_table, &options, &schema);
        assert!(renamed.name.to_string().ends_with("_rls"));
    }

    #[test]
    fn rls_generation_supports_quoted_identifiers() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE "Order Items"("doc id" INTEGER PRIMARY KEY, "owner id" INTEGER, "body text" TEXT);
            ALTER TABLE "Order Items" ENABLE ROW LEVEL SECURITY;
            CREATE POLICY order_items_select ON "Order Items" FOR SELECT USING ("owner id" > 0);
            CREATE POLICY order_items_insert ON "Order Items" FOR INSERT WITH CHECK ("owner id" > 0);
            CREATE POLICY order_items_update ON "Order Items" FOR UPDATE USING ("owner id" > 0) WITH CHECK ("owner id" > 0);
            CREATE POLICY order_items_delete ON "Order Items" FOR DELETE USING ("owner id" > 0);
            "#,
        );
        let table = schema
            .table_by_target(
                TargetName::parse("\"Order Items\"").expect("one identifier"),
                IdentifierCase::AsWritten,
            )
            .expect("table lookup should succeed")
            .expect("quoted table should exist (pass the quoted lookup form)");
        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"),
        );

        let statements = generate_rls_statements(table, &schema, &options, &mut |_| {})
            .expect("quoted identifiers in RLS SQL should translate");
        assert!(
            statements.iter().any(|stmt| stmt.to_string().contains("CREATE VIEW")),
            "expected generated RLS view"
        );
    }
}
