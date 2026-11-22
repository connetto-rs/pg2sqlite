//! A call to a `SECURITY DEFINER` SQL helper, written as the set it returns.
//!
//! A policy compares a column to what a helper returns, as
//! `x IN (SELECT authz.f('admin'))` or `x = ANY (ARRAY(SELECT
//! authz.f('admin')))`, or reads the helper's rows where a table goes, as
//! `(a, b) IN (SELECT * FROM authz.f('admin'))`. SQLite has no functions of
//! its own to call, so the call is replaced by the query
//! `FunctionLike::helper_set` reads out of the helper's body, one `SELECT` per
//! arm joined by `UNION`. Each arm reads the backing table of a table that has
//! one, because the helper runs as an owner that this reading has checked is
//! exempt from every policy it reads past, and the owner reads past the acting
//! role's grants. A table on which the acting role holds no privilege is left
//! out of the replica, so a helper reading one is refused.

use core::{convert::Infallible, ops::ControlFlow};

use sql_traits::{
    errors::HelperSetError,
    structs::{
        AntiJoin, ArmCondition, CalledArm, CallerGate, CapOperand, ExcludedSet, GrantArm,
        GrantedTo, HelperArm, HelperSet, ParentArm, ParserDB, RequestNormalization, Restriction,
        Returned, RowCap, SetPacking, SettingComparison, SettingContent, SettingValue, TimeBound,
        TimeComparison, UnsetSetting, WalkArm, WalkDirection,
    },
    traits::{ColumnLike, DatabaseLike, FunctionLike, TableLike},
};
use sqlparser::ast::{
    ArrayElemTypeDef, BinaryOperator, CastKind, CreateTable, Cte, DataType, Expr, FunctionArg,
    FunctionArgExpr, FunctionArguments, Ident, InTarget, Join, JoinConstraint, JoinOperator,
    ObjectName, ObjectNamePart, Query, SelectItem, SetExpr, SetOperator, SetQuantifier, TableAlias,
    TableAliasColumnDef, TableFactor, TableWithJoins, Value, ValueWithSpan, VisitMut, VisitorMut,
    With, helpers::attached_token::AttachedToken, visit_expressions,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        column_access::{from_names, holds},
        function_helpers::{query_projection, simple_function_expr, string_literal},
        object_name::last_ident,
        privileges::{RoleTable, has_backing_table},
        query_builder::{
            from_relation, make_query, make_simple_select, plain_table_factor, single_expr_query,
        },
        session_variable,
        shared_helpers::leaf_factors,
        translator_impls::sql_helper::{declared_helper, hidden_read, inlined_rows},
    },
    options::TranslationContext,
    traits::TranslatorWithContext,
    warnings::WarningSink,
};

/// The SQLite query answering `subquery`, when `subquery` is `SELECT f(...)`
/// and `f` is a function the schema declares.
///
/// `None` leaves the subquery to the ordinary translation, for a call the
/// schema does not declare and for an unqualified call the destination
/// registers under `with_user_defined_functions`, which is a claim the caller
/// made about that very name.
///
/// # Errors
///
/// Refuses the call when no deployment is configured, when
/// `FunctionLike::helper_set` cannot answer the helper, when the helper or a
/// set it excludes reads a table the acting role holds no privilege on, which
/// the translation leaves out of the replica, and when the query over its
/// tables does not translate.
pub(crate) fn helper_membership(
    subquery: &Query,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Query>, Error> {
    let Some(Expr::Function(call)) = query_projection(subquery) else { return Ok(None) };
    let name = &call.name;
    // A helper returning one value stands for its body, which the call's
    // ordinary translation writes in place of it.
    let Some(helper) =
        declared_helper(name, schema, options)?.filter(|helper| helper.returns_set())
    else {
        return Ok(None);
    };
    let arguments = match &call.args {
        FunctionArguments::List(list) if list.clauses.is_empty() => arguments(name, &list.args),
        other => Err(unsupported_argument(name, other)),
    };
    helper_query(helper, name, arguments, &[], schema, options, emit).map(Some)
}

/// The derived table answering `FROM f(...) [AS alias [(columns)]]`, when `f`
/// is a function the schema declares, under the relation and column names
/// PostgreSQL gives the call's rows.
///
/// The relation is the alias, or the function's name without one. The
/// columns are the ones `RETURNS TABLE (...)` declares, or the relation's name
/// for the one column of `RETURNS SETOF` a scalar type, the alias's columns
/// renaming the leading ones. A `LANGUAGE sql` helper running as its caller
/// reads the rows of its body, as [`inlined_rows`] writes them, and any other
/// the set `helper_set` reads. `None` is as for [`helper_membership`].
///
/// # Errors
///
/// Refuses the call as [`helper_membership`] or [`inlined_rows`] does, and an
/// alias carrying a data type or naming more columns than the helper
/// returns, both of which PostgreSQL refuses too.
pub(crate) fn helper_rows(
    name: &ObjectName,
    arguments: &[FunctionArg],
    alias: Option<&TableAlias>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<TableFactor>, Error> {
    let Some(helper) = declared_helper(name, schema, options)? else { return Ok(None) };
    let Some(function) = last_ident(name) else { return Ok(None) };
    let relation = alias.map_or(function, |alias| &alias.name).clone();
    let mut columns: Vec<Ident> = helper
        .returned_columns(schema)
        .map(|(column, _)| {
            if column.name_is_quoted() {
                Ident::with_quote('"', column.name())
            } else {
                Ident::new(column.name())
            }
        })
        .collect();
    if columns.is_empty() {
        columns.push(relation.clone());
    }
    if let Some(alias) = alias {
        if let Some(typed) = alias.columns.iter().find(|column| column.data_type.is_some()) {
            return Err(Error::forward_refusal(format!(
                "{name}() AS {} ({} ...) carries a data type in the column alias list, which \
                 PostgreSQL accepts only on a function returning record. Name the columns alone.",
                alias.name, typed.name
            )));
        }
        if alias.columns.len() > columns.len() {
            return Err(Error::forward_refusal(format!(
                "The alias {} names {} columns for {name}(), which returns {}. PostgreSQL refuses \
                 the longer list too. Name at most the columns the function returns.",
                alias.name,
                alias.columns.len(),
                columns.len()
            )));
        }
        for (column, renamed) in columns.iter_mut().zip(&alias.columns) {
            column.clone_from(&renamed.name);
        }
    }
    let rows = match inlined_rows(name, arguments, helper, &columns, schema, options, emit)? {
        Some(rows) => rows,
        None => {
            helper_query(
                helper,
                name,
                self::arguments(name, arguments),
                &columns,
                schema,
                options,
                emit,
            )?
        }
    };
    Ok(Some(TableFactor::Derived {
        lateral: false,
        subquery: Box::new(rows),
        alias: Some(TableAlias { explicit: true, name: relation, columns: Vec::new(), at: None }),
        sample: None,
    }))
}

/// Refuses a call in the FROM list `from` to a function the schema declares
/// that is passed a column another item of that list may hold.
///
/// PostgreSQL reads such a call `LATERAL`, in the FROM list of a `SELECT` or
/// an `UPDATE` and the `USING` list of a `DELETE` alike, so a bare column or
/// one qualified by the name an item of the list exposes reads that item, and
/// SQLite reads the derived table written in place of the call only against
/// the queries around it, where the same name may read another relation. A
/// column qualified by a name no item exposes reads an enclosing query in
/// both. A PL/pgSQL variable, a field of one, and a field of a trigger's
/// `NEW` or `OLD` are no item of the list and pass.
///
/// # Errors
///
/// Refuses the first such call, naming it.
pub(crate) fn check_lateral_calls(
    from: &[TableWithJoins],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    let factors = leaf_factors(from);
    if factors.len() < 2 {
        return Ok(());
    }
    let mut exposed = Vec::new();
    for item in from {
        from_names(item, &mut exposed);
    }
    for factor in factors {
        let (name, arguments) = match factor {
            TableFactor::Table { name, args: Some(arguments), .. } => (name, &arguments.args),
            TableFactor::Function { name, args, .. } => (name, args),
            _ => continue,
        };
        let reads_column = visit_expressions(arguments, |expr| {
            let reaches = match expr {
                Expr::Identifier(ident) => !options.is_variable(&ident.value),
                Expr::CompoundIdentifier(parts) => {
                    match parts.as_slice() {
                        [first, ..]
                            if options.is_variable(&first.value)
                                || options
                                    .trigger_row(&Expr::Identifier(first.clone()))
                                    .is_some() =>
                        {
                            false
                        }
                        parts => {
                            parts
                                .iter()
                                .rev()
                                .nth(1)
                                .is_none_or(|qualifier| holds(&exposed, qualifier))
                        }
                    }
                }
                _ => false,
            };
            if reaches { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        })
        .is_break();
        if reads_column && declared_helper(name, schema, options)?.is_some() {
            return Err(Error::forward_refusal(format!(
                "{name}() is passed a column beside another item of the FROM list, which \
                 PostgreSQL reads LATERAL, reaching that item, and SQLite has no LATERAL. Call \
                 it alone in the FROM list of a subquery, as EXISTS (SELECT 1 FROM {name}(...))."
            )));
        }
    }
    Ok(())
}

/// The refusal of a call to `name` that `helper_set` cannot answer.
fn untranslatable(name: &ObjectName, error: &HelperSetError) -> Error {
    Error::forward_refusal(format!("{name}() cannot be translated as the set it returns: {error}"))
}

/// The SQLite query returning the set `helper` returns for `arguments` under
/// `names`, or the refusal of the reason they cannot be read.
fn helper_query(
    helper: &<ParserDB as DatabaseLike>::Function,
    name: &ObjectName,
    arguments: Result<Vec<Value>, HelperSetError>,
    names: &[Ident],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Query, Error> {
    let Some(deployment) = options.get_deployment() else {
        return Err(Error::forward_refusal(format!(
            "{name}() is a function the schema declares, and a call to it translates as the set \
             it returns for the caller, which takes the deployment declaring the caller's \
             setting. Pass it with with_deployment(...)."
        )));
    };
    let set = arguments
        .and_then(|arguments| {
            helper.helper_set(
                &arguments,
                deployment,
                schema,
                &[ArmCondition::Exclusion, ArmCondition::UnsetSetting],
            )
        })
        .map_err(|error| untranslatable(name, &error))?;
    let mut lowering =
        Lowering { helper: name, schema, read: Vec::new(), walks: 0, called: Vec::new() };
    let lowered = lowering.set(&set, names)?;
    let acting = session_variable::acting_role(schema, options)?;
    let mut guarded = Vec::new();
    for table in lowering.read {
        if let Some(acting) = acting
            && !RoleTable::of(table, Some(acting), schema)?.reads_any()
        {
            return Err(Error::forward_refusal(format!(
                "{name}() cannot be translated as the set it returns: it {}.",
                hidden_read(&table.name, acting)
            )));
        }
        let table_name = table.table_name();
        if !guarded.contains(&table_name) && has_backing_table(table, schema, options)? {
            guarded.push(table_name);
        }
    }
    let mut lowered = lowered.translate_with_warnings(schema, options, emit)?;
    let ControlFlow::Continue(()) = lowered
        .visit(&mut BackingTables { guarded: &guarded, suffix: options.get_rls_table_suffix() });
    let mut helpers = vec![helper.target_name().to_string()];
    helpers.append(&mut lowering.called);
    options.record_helper_set(&lowered, helpers);
    Ok(lowered)
}

/// The literals a call passes, one per argument.
fn arguments(name: &ObjectName, arguments: &[FunctionArg]) -> Result<Vec<Value>, HelperSetError> {
    arguments
        .iter()
        .map(|argument| {
            match argument {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value))) => {
                    Ok(value.value.clone())
                }
                other => Err(unsupported_argument(name, other)),
            }
        })
        .collect()
}

/// The refusal of `argument`, which is no literal a helper set can bind.
fn unsupported_argument(name: &ObjectName, argument: &dyn ToString) -> HelperSetError {
    HelperSetError::UnsupportedArgument {
        function: name.to_string(),
        argument: argument.to_string(),
    }
}

/// The alias of the table a grant or parent arm reads.
const ARM: &str = "arm";
/// The alias of the table an anti-join reads.
const BLOCKING: &str = "blocking";
/// The one column of a walk.
const NODE: &str = "node";
/// The alias of the edges a walk follows, and their two columns.
const EDGE: &str = "edge";
const NEXT: &str = "next";
const PREVIOUS: &str = "previous";

/// The alias of the rows a called arm reads.
const CALLED: &str = "called";
/// The one column of a list of labels.
const LABEL: &str = "label";

/// Writes a helper's set as a PostgreSQL query over the tables its arms read,
/// under their declared names so the translation resolves their columns.
struct Lowering<'h, 'db> {
    /// The helper, as refusals name it.
    helper: &'h ObjectName,
    schema: &'db ParserDB,
    /// Every table the query reads, in the order it reads them.
    read: Vec<&'db CreateTable>,
    /// How many walks the query names so far.
    walks: usize,
    /// The helpers whose sets the query reads, one entry per call.
    called: Vec<String>,
}

impl<'db> Lowering<'_, 'db> {
    /// The set as one query: its arms joined by `UNION`, grown along its
    /// walks, and kept to the rows its selection keeps, projecting its
    /// columns under `names`, or unnamed where `names` is empty.
    #[cfg_attr(feature = "std", recursive::recursive)]
    fn set(&mut self, set: &HelperSet<'db, ParserDB>, names: &[Ident]) -> Result<Query, Error> {
        // A walk reads the arms' rows as its own column and names what it
        // keeps.
        let walked = set.selection().is_some()
            || set.arms().iter().any(|arm| matches!(arm, HelperArm::Walk(_)));
        let arm_names = if walked { &[] } else { names };
        let mut arms = Vec::new();
        let mut edges = Vec::new();
        for arm in set.arms() {
            match arm {
                HelperArm::Grant(grant) => arms.push(self.grant(grant, arm_names)?),
                HelperArm::Parent(parent) => arms.push(self.parent(parent, arm_names)?),
                HelperArm::Called(called) => arms.push(self.called(called, arm_names)?),
                HelperArm::Walk(walk) => edges.push(self.edge(walk)),
            }
        }
        let selection = match set.selection() {
            Some(selection) => {
                let mut kept = Vec::new();
                for arm in selection {
                    kept.push(self.grant(arm, &[])?);
                }
                // A selection none of whose arms keeps a row keeps no value.
                let Some(kept) = union(kept, SetQuantifier::None) else {
                    return Ok(empty_set(names));
                };
                Some(kept)
            }
            None => None,
        };
        // A helper keeping no row for these arguments returns the empty set.
        let Some(arms) = union(arms, SetQuantifier::None) else { return Ok(empty_set(names)) };
        if !walked {
            return Ok(make_query(None, arms));
        }
        Ok(self.reached(arms, union(edges, SetQuantifier::All), selection, names))
    }

    /// [`Self::set`] for the set a call to `function` returns.
    fn called_set(
        &mut self,
        function: &<ParserDB as DatabaseLike>::Function,
        set: &HelperSet<'db, ParserDB>,
        names: &[Ident],
    ) -> Result<Query, Error> {
        self.called.push(function.target_name().to_string());
        self.set(set, names)
    }

    /// `WITH [RECURSIVE] <walk> (node) AS (<arms> [UNION <step>]) SELECT node
    /// [AS <name>] FROM <walk> [WHERE node IN (<selection>)]`, one step adding
    /// the next row along every edge from a row the walk holds.
    ///
    /// The step joins the walk once, to the union of the edges, since both
    /// engines read a recursive reference in only one place.
    fn reached(
        &mut self,
        arms: SetExpr,
        edges: Option<SetExpr>,
        selection: Option<SetExpr>,
        names: &[Ident],
    ) -> Query {
        let walk = self.walk_name();
        let recursive = edges.is_some();
        let body = match edges {
            Some(edges) => {
                let step = make_simple_select(
                    vec![SelectItem::UnnamedExpr(qualified(EDGE, Ident::new(NEXT)))],
                    vec![TableWithJoins {
                        relation: plain_table_factor(ObjectName::from(vec![walk.clone()])),
                        joins: vec![Join {
                            relation: TableFactor::Derived {
                                lateral: false,
                                subquery: Box::new(make_query(None, edges)),
                                alias: Some(alias(EDGE)),
                                sample: None,
                            },
                            global: false,
                            join_operator: JoinOperator::Join(JoinConstraint::On(equals(
                                qualified(EDGE, Ident::new(PREVIOUS)),
                                qualified(&walk.value, Ident::new(NODE)),
                            ))),
                        }],
                    }],
                    None,
                );
                set_operation(arms, SetExpr::Select(Box::new(step)), SetQuantifier::None)
            }
            None => arms,
        };
        let cte = Cte {
            alias: TableAlias {
                explicit: false,
                name: walk.clone(),
                columns: vec![TableAliasColumnDef { name: Ident::new(NODE), data_type: None }],
                at: None,
            },
            query: Box::new(make_query(None, body)),
            from: None,
            materialized: None,
            closing_paren_token: AttachedToken::empty(),
        };
        let node = Expr::Identifier(Ident::new(NODE));
        let kept = selection.map(|selection| in_query(node.clone(), make_query(None, selection)));
        let with = With { with_token: AttachedToken::empty(), recursive, cte_tables: vec![cte] };
        let walked = make_simple_select(
            projection([node], names),
            from_relation(plain_table_factor(ObjectName::from(vec![walk]))),
            kept,
        );
        make_query(Some(with), SetExpr::Select(Box::new(walked)))
    }

    /// A name for the next walk that names no table, so it reaches no
    /// backing table either.
    fn walk_name(&mut self) -> Ident {
        loop {
            let name = format!("walk_{}", self.walks);
            self.walks += 1;
            if !self.schema.tables().any(|table| table.table_name() == name) {
                return Ident::new(name);
            }
        }
    }

    /// `SELECT <next> AS next, <previous> AS previous FROM <table>`, the edges
    /// of one walk, each from the row the walk holds to the row it adds.
    fn edge(&mut self, walk: &WalkArm<'db, ParserDB>) -> SetExpr {
        self.read.push(walk.table());
        let (next, previous) = match walk.direction() {
            WalkDirection::Down => (walk.child(), walk.column()),
            WalkDirection::Up => (walk.column(), walk.child()),
        };
        let projected = |column: &<ParserDB as DatabaseLike>::Column, name: &str| {
            SelectItem::ExprWithAlias {
                expr: Expr::Identifier(column_ident(column)),
                alias: Ident::new(name),
            }
        };
        SetExpr::Select(Box::new(make_simple_select(
            vec![projected(next, NEXT), projected(previous, PREVIOUS)],
            from_relation(plain_table_factor(walk.table().name.clone())),
            None,
        )))
    }

    /// `SELECT <key> FROM <table> WHERE <column> IN (<parents>)`.
    fn parent(
        &mut self,
        parent: &ParentArm<'db, ParserDB>,
        names: &[Ident],
    ) -> Result<SetExpr, Error> {
        self.read.push(parent.table());
        let parents = self.called_set(parent.function(), parent.set(), &[])?;
        Ok(SetExpr::Select(Box::new(make_simple_select(
            projection([qualified(ARM, column_ident(parent.key()))], names),
            from_relation(aliased_table(parent.table(), ARM)),
            Some(in_query(qualified(ARM, column_ident(parent.column())), parents)),
        ))))
    }

    /// `SELECT <returned> FROM (<set>) AS called [CROSS JOIN <labels>]...`,
    /// the rows the called helper returns under positional column names.
    fn called(
        &mut self,
        called: &CalledArm<'db, ParserDB>,
        names: &[Ident],
    ) -> Result<SetExpr, Error> {
        // A helper returning `SETOF` a scalar type declares no column.
        let arity = called.function().returned_columns(self.schema).count().max(1);
        let columns: Vec<Ident> = (0..arity).map(called_column).collect();
        let rows = self.called_set(called.function(), called.set(), &columns)?;
        let (returned, labels) = returned_values(called.returned(), |position| {
            qualified(CALLED, called_column(*position))
        });
        let relation = TableFactor::Derived {
            lateral: false,
            subquery: Box::new(rows),
            alias: Some(alias(CALLED)),
            sample: None,
        };
        Ok(SetExpr::Select(Box::new(make_simple_select(
            projection(returned, names),
            vec![TableWithJoins { relation, joins: labels }],
            None,
        ))))
    }

    /// `SELECT <returned> FROM <table> [JOIN <joined table> ON <key>]...
    /// [CROSS JOIN <labels>]... WHERE <conditions>`, every condition the arm
    /// holds written out, so the arm keeps no row its helper does not return.
    fn grant(
        &mut self,
        grant: &GrantArm<'db, ParserDB>,
        names: &[Ident],
    ) -> Result<SetExpr, Error> {
        self.read.push(grant.table());
        let mut conditions = Vec::new();
        let mut joins = Vec::new();
        for (index, joined) in grant.joined_rows().iter().enumerate() {
            let name = joined_alias(index);
            self.read.push(joined.table());
            let key = joined.columns().iter().map(|(host, referenced)| {
                equals(
                    qualified(&name, column_ident(referenced)),
                    qualified(ARM, column_ident(host)),
                )
            });
            joins.push(Join {
                relation: aliased_table(joined.table(), &name),
                global: false,
                join_operator: JoinOperator::Join(
                    conjunction(key).map_or(JoinConstraint::None, JoinConstraint::On),
                ),
            });
            conditions.extend(restrictions(&name, joined.restrictions()));
            conditions.extend(time_bounds(&name, joined.time_bounds()));
        }
        match grant.granted_to() {
            GrantedTo::Request(comparison) => conditions.push(request(comparison)),
            GrantedTo::Set(grantees) => {
                let set = self.called_set(grantees.function(), grantees.set(), &[])?;
                conditions.push(in_query(qualified(ARM, column_ident(grantees.column())), set));
            }
            GrantedTo::Anyone => {}
        }
        if let Some(gate) = grant.gate() {
            conditions.extend(self.gate(gate)?);
        }
        conditions.extend(grant.unset_settings().iter().map(unset));
        conditions.extend(restrictions(ARM, grant.restrictions()));
        conditions.extend(time_bounds(ARM, grant.time_bounds()));
        for cap in grant.caps() {
            conditions.push(self.cap(cap)?);
        }
        for anti_join in grant.anti_joins() {
            conditions.push(self.anti_join(anti_join));
        }
        for exclusion in grant.exclusions() {
            conditions.push(self.exclusion(exclusion)?);
        }
        let (returned, labels) =
            returned_values(grant.returned(), |column| qualified(ARM, column_ident(column)));
        joins.extend(labels);
        Ok(SetExpr::Select(Box::new(make_simple_select(
            projection(returned, names),
            vec![TableWithJoins { relation: aliased_table(grant.table(), ARM), joins }],
            conjunction(conditions),
        ))))
    }

    /// `<setting> IS NOT NULL` and `<setting> <> <value>` for each excluded
    /// value.
    fn gate(&self, gate: &CallerGate<'db>) -> Result<Vec<Expr>, Error> {
        let setting = gate.setting();
        let mut read =
            setting_read(setting.name(), gate.raises_when_unset(), gate.excludes_unset());
        if gate.compares_in_declared_type() {
            let SettingContent::Value(SettingValue::Caller(declared)) = setting.content() else {
                return Err(Error::forward_refusal(format!(
                    "{}() gates on {}, which the deployment declares as no caller.",
                    self.helper,
                    setting.name()
                )));
            };
            read = cast(read, declared.clone());
        }
        let mut conditions = vec![Expr::IsNotNull(Box::new(read.clone()))];
        conditions.extend(gate.excluded().iter().map(|value| {
            Expr::BinaryOp {
                left: Box::new(read.clone()),
                op: BinaryOperator::NotEq,
                right: Box::new(Expr::Value(value.clone().into())),
            }
        }));
        Ok(conditions)
    }

    /// `[<setting> IS NULL OR] (<operands>) IN (<set>)`.
    fn cap(&mut self, cap: &RowCap<'db, ParserDB>) -> Result<Expr, Error> {
        let mut operands: Vec<Expr> = cap
            .operands()
            .iter()
            .map(|operand| {
                match operand {
                    CapOperand::Column(column) => qualified(ARM, column_ident(column)),
                    CapOperand::Joined { row, column } => {
                        qualified(&joined_alias(*row), column_ident(column))
                    }
                    CapOperand::Value(value) => Expr::Value(value.clone().into()),
                }
            })
            .collect();
        let row = match operands.len() {
            1 => operands.remove(0),
            _ => Expr::Tuple(operands),
        };
        // `SELECT NULL WHERE false` has one column, so a row cap cannot compare
        // with it.
        let capped = if cap.set().arms().is_empty() {
            Expr::Value(Value::Boolean(false).into())
        } else {
            in_query(row, self.called_set(cap.function(), cap.set(), &[])?)
        };
        Ok(match cap.lift() {
            Some(lift) => {
                Expr::Nested(Box::new(Expr::BinaryOp {
                    left: Box::new(unset(lift)),
                    op: BinaryOperator::Or,
                    right: Box::new(capped),
                }))
            }
            None => capped,
        })
    }

    /// `NOT EXISTS (SELECT 1 FROM <table> WHERE <correlations> AND
    /// <conditions>)`.
    fn anti_join(&mut self, anti_join: &AntiJoin<'db, ParserDB>) -> Expr {
        self.read.push(anti_join.table());
        let mut conditions: Vec<Expr> = anti_join
            .correlations()
            .iter()
            .map(|correlation| {
                equals(
                    qualified(BLOCKING, column_ident(correlation.blocking_column())),
                    qualified(ARM, column_ident(correlation.arm_column())),
                )
            })
            .collect();
        conditions.extend(restrictions(BLOCKING, anti_join.restrictions()));
        conditions.extend(time_bounds(BLOCKING, anti_join.time_bounds()));
        Expr::Exists {
            subquery: Box::new(single_expr_query(
                Expr::Value(Value::Number("1".into(), false).into()),
                from_relation(aliased_table(anti_join.table(), BLOCKING)),
                conjunction(conditions),
            )),
            negated: true,
        }
    }

    /// `<column> NOT IN (<set>)`, which is `NULL`, dropping the row, where
    /// the column holds `NULL` and the set any row, or the set holds `NULL`
    /// but not the column's value, as PostgreSQL's exclusion is.
    fn exclusion(&mut self, exclusion: &ExcludedSet<'db, ParserDB>) -> Result<Expr, Error> {
        let excluded = self.set(exclusion.set(), &[])?;
        Ok(not_in_query(qualified(ARM, column_ident(exclusion.column())), excluded))
    }
}

/// The alias of the row a grant arm joins at `row` in its joined rows.
fn joined_alias(row: usize) -> String {
    format!("joined_{row}")
}

/// The name a called arm reads the column of its helper's rows at `position`
/// under.
fn called_column(position: usize) -> Ident {
    Ident::new(format!("column_{position}"))
}

/// The values an arm returning `returned` projects, `read` writing a value
/// read off the arm's row, and the lists of labels it joins, one per position
/// returning labels.
fn returned_values<S>(
    returned: &[Returned<S>],
    read: impl Fn(&S) -> Expr,
) -> (Vec<Expr>, Vec<Join>) {
    let mut lists = Vec::new();
    let values = returned
        .iter()
        .enumerate()
        .map(|(position, returned)| {
            match returned {
                Returned::Read(value) => read(value),
                Returned::Labels(labels) => {
                    let name = format!("labels_{position}");
                    lists.push(label_list(labels, &name));
                    qualified(&name, Ident::new(LABEL))
                }
            }
        })
        .collect();
    (values, lists)
}

/// `values` as a projection, each under the name at its position in `names`
/// and unnamed past the last name.
fn projection(values: impl IntoIterator<Item = Expr>, names: &[Ident]) -> Vec<SelectItem> {
    let mut names = names.iter();
    values
        .into_iter()
        .map(|expr| {
            match names.next() {
                Some(name) => SelectItem::ExprWithAlias { expr, alias: name.clone() },
                None => SelectItem::UnnamedExpr(expr),
            }
        })
        .collect()
}

/// `CROSS JOIN (SELECT <label> AS label [UNION ALL SELECT <label> AS
/// label]...) AS <name>`, which pairs every row of the arm with each label,
/// and with none for no label.
fn label_list(labels: &[Value], name: &str) -> Join {
    let column = [Ident::new(LABEL)];
    let rows = labels.iter().map(|label| {
        SetExpr::Select(Box::new(make_simple_select(
            projection([Expr::Value(label.clone().into())], &column),
            Vec::new(),
            None,
        )))
    });
    let body =
        union(rows.collect(), SetQuantifier::All).unwrap_or_else(|| *empty_set(&column).body);
    Join {
        relation: TableFactor::Derived {
            lateral: false,
            subquery: Box::new(make_query(None, body)),
            alias: Some(alias(name)),
            sample: None,
        },
        global: false,
        join_operator: JoinOperator::CrossJoin(JoinConstraint::None),
    }
}

/// `SELECT NULL [AS <name>]... WHERE false`, the set holding no row, with a
/// column per name, or one unnamed column for no name.
fn empty_set(names: &[Ident]) -> Query {
    let nulls = (0..names.len().max(1)).map(|_| Expr::Value(Value::Null.into()));
    make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            projection(nulls, names),
            Vec::new(),
            Some(Expr::Value(Value::Boolean(false).into())),
        ))),
    )
}

/// The bodies joined left to right by `UNION` under `quantifier`, `None`
/// for no body.
fn union(bodies: Vec<SetExpr>, quantifier: SetQuantifier) -> Option<SetExpr> {
    bodies.into_iter().reduce(|left, right| set_operation(left, right, quantifier))
}

fn set_operation(left: SetExpr, right: SetExpr, set_quantifier: SetQuantifier) -> SetExpr {
    SetExpr::SetOperation {
        op: SetOperator::Union,
        set_quantifier,
        left: Box::new(left),
        right: Box::new(right),
    }
}

/// What each column has to hold, `relation` naming the row read.
fn restrictions<'r>(
    relation: &'r str,
    restrictions: &'r [Restriction<'_, ParserDB>],
) -> impl Iterator<Item = Expr> + 'r {
    restrictions.iter().map(move |restriction| {
        let column = Box::new(qualified(relation, column_ident(restriction.column())));
        match restriction {
            Restriction::In(set) => {
                Expr::In {
                    expr: column,
                    target: InTarget::List(
                        set.values()
                            .iter()
                            .map(|value| Expr::Value(ValueWithSpan::from(value.clone())))
                            .collect(),
                    ),
                    negated: false,
                    global: false,
                }
            }
            Restriction::Null(_) => Expr::IsNull(column),
            Restriction::NotNull(_) => Expr::IsNotNull(column),
        }
    })
}

/// `(<column> <comparison> now() [OR <column> IS NULL])` for each bound,
/// `relation` naming the row read.
fn time_bounds<'r>(
    relation: &'r str,
    bounds: &'r [TimeBound<'_, ParserDB>],
) -> impl Iterator<Item = Expr> + 'r {
    bounds.iter().map(move |bound| {
        let column = qualified(relation, column_ident(bound.column()));
        let operator = match bound.comparison() {
            TimeComparison::Later => BinaryOperator::Gt,
            TimeComparison::LaterOrEqual => BinaryOperator::GtEq,
            TimeComparison::Earlier => BinaryOperator::Lt,
            TimeComparison::EarlierOrEqual => BinaryOperator::LtEq,
        };
        let compared = Expr::BinaryOp {
            left: Box::new(column.clone()),
            op: operator,
            right: Box::new(simple_function_expr("now", Vec::new(), None)),
        };
        if !bound.keeps_null() {
            return compared;
        }
        Expr::Nested(Box::new(Expr::BinaryOp {
            left: Box::new(compared),
            op: BinaryOperator::Or,
            right: Box::new(Expr::IsNull(Box::new(column))),
        }))
    })
}

/// The arm's column compared with the value or the set the request carries,
/// read as the body reads it.
fn request(comparison: &SettingComparison<'_, ParserDB>) -> Expr {
    let name = comparison.setting().name();
    let read = |empty_is_null| setting_read(name, comparison.raises_when_unset(), empty_is_null);
    let column = qualified(ARM, column_ident(comparison.column()));
    match comparison.value() {
        SettingValue::Caller(declared)
        | SettingValue::Principal { identity: declared, .. }
        | SettingValue::Scalar(declared) => {
            let mut value = read(comparison.excludes_unset());
            if let Some(normalization) = comparison.normalization() {
                let function = match normalization {
                    RequestNormalization::Lower => "lower",
                };
                value = simple_function_expr(function, vec![value], None);
            }
            equals(column, cast(value, declared.clone()))
        }
        SettingValue::Set { element, packing: SetPacking::Delimited(delimiter) } => {
            // `string_to_array` unpacks `''` to no element, so the read needs
            // no `NULLIF`.
            let elements = simple_function_expr(
                "string_to_array",
                vec![read(false), string_literal(delimiter)],
                None,
            );
            Expr::AnyOp {
                left: Box::new(column),
                compare_op: BinaryOperator::Eq,
                right: Box::new(cast(
                    elements,
                    DataType::Array(ArrayElemTypeDef::SquareBracket(
                        Box::new(element.clone()),
                        None,
                    )),
                )),
                is_some: false,
            }
        }
        SettingValue::Set { element, packing: SetPacking::JsonArray } => {
            let elements = simple_function_expr(
                "jsonb_array_elements_text",
                vec![cast(read(comparison.excludes_unset()), DataType::JSONB)],
                None,
            );
            in_query(column, single_expr_query(cast(elements, element.clone()), Vec::new(), None))
        }
    }
}

/// `<setting> IS NULL`, read as the body reads it.
fn unset(setting: &UnsetSetting<'_>) -> Expr {
    Expr::IsNull(Box::new(setting_read(
        setting.setting().name(),
        setting.raises_when_unset(),
        setting.reads_empty_as_unset(),
    )))
}

/// `current_setting('<name>'[, true])`, through `NULLIF(..., '')` when
/// `empty_is_null`, `raises_when_unset` choosing the one-argument read.
fn setting_read(name: &str, raises_when_unset: bool, empty_is_null: bool) -> Expr {
    let mut arguments = vec![string_literal(name)];
    if !raises_when_unset {
        arguments.push(Expr::Value(Value::Boolean(true).into()));
    }
    let read = simple_function_expr("current_setting", arguments, None);
    if !empty_is_null {
        return read;
    }
    simple_function_expr("nullif", vec![read, string_literal("")], None)
}

fn cast(expr: Expr, data_type: DataType) -> Expr {
    Expr::Cast { kind: CastKind::DoubleColon, expr: Box::new(expr), data_type, format: None }
}

fn equals(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::Eq, right: Box::new(right) }
}

fn in_query(expr: Expr, query: Query) -> Expr {
    Expr::In {
        expr: Box::new(expr),
        target: InTarget::Subquery(Box::new(query)),
        negated: false,
        global: false,
    }
}

fn not_in_query(expr: Expr, query: Query) -> Expr {
    Expr::In {
        expr: Box::new(expr),
        target: InTarget::Subquery(Box::new(query)),
        negated: true,
        global: false,
    }
}

/// The conditions joined by `AND`, `None` for no condition.
fn conjunction(conditions: impl IntoIterator<Item = Expr>) -> Option<Expr> {
    conditions.into_iter().reduce(|left, right| {
        Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::And, right: Box::new(right) }
    })
}

fn alias(name: &str) -> TableAlias {
    TableAlias { explicit: true, name: Ident::new(name), columns: Vec::new(), at: None }
}

/// `<table> AS <name>`.
fn aliased_table(table: &CreateTable, name: &str) -> TableFactor {
    let mut relation = plain_table_factor(table.name.clone());
    if let TableFactor::Table { alias: slot, .. } = &mut relation {
        *slot = Some(alias(name));
    }
    relation
}

/// `<relation>.<column>`.
fn qualified(relation: &str, column: Ident) -> Expr {
    Expr::CompoundIdentifier(vec![Ident::new(relation), column])
}

/// A column's name, quoted where the schema quotes it.
fn column_ident(column: &<ParserDB as DatabaseLike>::Column) -> Ident {
    let name = column.column_name();
    if column.column_name_is_quoted() { Ident::with_quote('"', name) } else { Ident::new(name) }
}

/// Points every relation of a lowered set at its table's backing table.
///
/// Every relation in the query is one an arm reads, so each naming a table
/// with row-level security is renamed, the view carrying its name filtering
/// rows the helper's owner reads past.
struct BackingTables<'a> {
    guarded: &'a [&'a str],
    suffix: &'a str,
}

impl VisitorMut for BackingTables<'_> {
    type Break = Infallible;

    fn post_visit_relation(&mut self, relation: &mut ObjectName) -> ControlFlow<Self::Break> {
        if let Some(ObjectNamePart::Identifier(last)) = relation.0.last_mut()
            && self.guarded.contains(&last.value.as_str())
        {
            last.value.push_str(self.suffix);
        }
        ControlFlow::Continue(())
    }
}
