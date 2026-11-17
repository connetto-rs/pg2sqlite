//! A call to a `SECURITY DEFINER` SQL helper, written as the set it returns.
//!
//! A policy compares a column to what a helper returns, as
//! `x IN (SELECT authz.f('admin'))` or `x = ANY (ARRAY(SELECT
//! authz.f('admin')))`. SQLite has no functions of its own to call, so the call
//! is replaced by the query `FunctionLike::helper_set` reads out of the
//! helper's body, one `SELECT` per arm joined by `UNION`. Each arm reads the
//! backing table of a table that has one, because the helper runs as an owner
//! that `helper_set` has checked is exempt from every policy it reads past,
//! and the owner reads past the acting role's grants.

use core::{convert::Infallible, ops::ControlFlow};

use sql_traits::{
    errors::HelperSetError,
    structs::{
        AntiJoin, CalledArm, CallerGate, CapOperand, GrantArm, GrantedTo, HelperArm, HelperSet,
        ParentArm, ParserDB, Restriction, Returned, RowCap, SetPacking, SettingComparison,
        SettingContent, SettingValue, TimeBound, TimeComparison, WalkArm, WalkDirection,
    },
    traits::{ColumnLike, DatabaseLike, FunctionLike, TableLike},
};
use sqlparser::ast::{
    ArrayElemTypeDef, BinaryOperator, CastKind, CreateTable, Cte, DataType, Expr, Function,
    FunctionArg, FunctionArgExpr, FunctionArguments, Ident, InTarget, Join, JoinConstraint,
    JoinOperator, ObjectName, ObjectNamePart, Query, SelectItem, SetExpr, SetOperator,
    SetQuantifier, TableAlias, TableAliasColumnDef, TableFactor, TableWithJoins, Value,
    ValueWithSpan, VisitMut, VisitorMut, With, helpers::attached_token::AttachedToken,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        function_helpers::{query_projection, simple_function_expr, string_literal},
        privileges::has_backing_table,
        query_builder::{
            from_relation, make_query, make_simple_select, plain_table_factor, single_expr_query,
        },
        translator_impls::sql_helper::declared_helper,
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
/// `FunctionLike::helper_set` cannot answer the helper, and when the query
/// over its tables does not translate.
pub(crate) fn helper_membership(
    subquery: &Query,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Query>, Error> {
    let Some(Expr::Function(call)) = query_projection(subquery) else { return Ok(None) };
    // A helper returning one value stands for its body, which the call's
    // ordinary translation writes in place of it.
    let Some(helper) =
        declared_helper(&call.name, schema, options)?.filter(|helper| helper.returns_set())
    else {
        return Ok(None);
    };
    let name = &call.name;
    let Some(deployment) = options.get_deployment() else {
        return Err(Error::forward_refusal(format!(
            "{name}() is a function the schema declares, and a call to it translates as the set \
             it returns for the caller, which takes the deployment declaring the caller's \
             setting. Pass it with with_deployment(...)."
        )));
    };
    let set = arguments(call)
        .and_then(|arguments| helper.helper_set(&arguments, deployment, schema))
        .map_err(|error| {
            Error::forward_refusal(format!(
                "{name}() cannot be translated as the set it returns: {error}"
            ))
        })?;
    let mut lowering = Lowering { helper: name, schema, read: Vec::new(), walks: 0 };
    let lowered = lowering.set(&set)?;
    let mut guarded = Vec::new();
    for table in lowering.read {
        let table_name = table.table_name();
        if !guarded.contains(&table_name) && has_backing_table(table, schema, options)? {
            guarded.push(table_name);
        }
    }
    let mut lowered = lowered.translate_with_warnings(schema, options, emit)?;
    let ControlFlow::Continue(()) = lowered
        .visit(&mut BackingTables { guarded: &guarded, suffix: options.get_rls_table_suffix() });
    Ok(Some(lowered))
}

/// The literals a call passes, one per argument.
fn arguments(call: &Function) -> Result<Vec<Value>, HelperSetError> {
    let unsupported = |argument: &dyn ToString| {
        HelperSetError::UnsupportedArgument {
            function: call.name.to_string(),
            argument: argument.to_string(),
        }
    };
    let FunctionArguments::List(list) = &call.args else { return Err(unsupported(&call.args)) };
    if !list.clauses.is_empty() {
        return Err(unsupported(list));
    }
    list.args
        .iter()
        .map(|argument| {
            match argument {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value))) => {
                    Ok(value.value.clone())
                }
                other => Err(unsupported(other)),
            }
        })
        .collect()
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
}

impl<'db> Lowering<'_, 'db> {
    /// The set as one query: its arms joined by `UNION`, grown along its
    /// walks, and kept to the rows its selection keeps.
    fn set(&mut self, set: &HelperSet<'db, ParserDB>) -> Result<Query, Error> {
        let mut arms = Vec::new();
        let mut edges = Vec::new();
        for arm in set.arms() {
            match arm {
                HelperArm::Grant(grant) => arms.push(self.grant(grant)?),
                HelperArm::Parent(parent) => arms.push(self.parent(parent)?),
                HelperArm::Called(called) => arms.extend(self.called(called)?),
                HelperArm::Walk(walk) => edges.push(self.edge(walk)),
            }
        }
        let selection = match set.selection() {
            Some(selection) => {
                let mut kept = Vec::new();
                for arm in selection {
                    kept.push(self.grant(arm)?);
                }
                // A selection none of whose arms keeps a row keeps no value.
                let Some(kept) = union(kept, SetQuantifier::None) else { return Ok(empty_set()) };
                Some(kept)
            }
            None => None,
        };
        // A helper keeping no row for these arguments returns the empty set.
        let Some(arms) = union(arms, SetQuantifier::None) else { return Ok(empty_set()) };
        if edges.is_empty() && selection.is_none() {
            return Ok(make_query(None, arms));
        }
        Ok(self.reached(arms, union(edges, SetQuantifier::All), selection))
    }

    /// `WITH [RECURSIVE] <walk> (node) AS (<arms> [UNION <step>]) SELECT node
    /// FROM <walk> [WHERE node IN (<selection>)]`, one step adding the next
    /// row along every edge from a row the walk holds.
    ///
    /// The step joins the walk once, to the union of the edges, since both
    /// engines read a recursive reference in only one place.
    fn reached(
        &mut self,
        arms: SetExpr,
        edges: Option<SetExpr>,
        selection: Option<SetExpr>,
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
        let walked = single_expr_query(
            node,
            from_relation(plain_table_factor(ObjectName::from(vec![walk]))),
            kept,
        );
        make_query(Some(with), *walked.body)
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
    fn parent(&mut self, parent: &ParentArm<'db, ParserDB>) -> Result<SetExpr, Error> {
        self.read.push(parent.table());
        let parents = self.set(parent.set())?;
        Ok(SetExpr::Select(Box::new(make_simple_select(
            vec![SelectItem::UnnamedExpr(qualified(ARM, column_ident(parent.key())))],
            from_relation(aliased_table(parent.table(), ARM)),
            Some(in_query(qualified(ARM, column_ident(parent.column())), parents)),
        ))))
    }

    /// `SELECT <returned> FROM (<set>) AS called [CROSS JOIN <labels>]...`,
    /// the rows the called helper returns under positional column names, or
    /// `None` where it returns no row.
    fn called(&mut self, called: &CalledArm<'db, ParserDB>) -> Result<Option<SetExpr>, Error> {
        // `SELECT NULL WHERE false` has one column, so it cannot stand for
        // the rows of a helper returning several.
        if called.set().arms().is_empty() {
            return Ok(None);
        }
        let mut rows = self.set(called.set())?;
        // A helper returning `SETOF` a scalar type declares no column.
        let arity = called.function().returned_columns(self.schema).count().max(1);
        let columns: Vec<Ident> = (0..arity).map(called_column).collect();
        name_columns(&mut rows.body, &columns);
        let (projection, labels) = returned_projection(called.returned(), |position| {
            qualified(CALLED, called_column(*position))
        });
        let relation = TableFactor::Derived {
            lateral: false,
            subquery: Box::new(rows),
            alias: Some(alias(CALLED)),
            sample: None,
        };
        Ok(Some(SetExpr::Select(Box::new(make_simple_select(
            projection,
            vec![TableWithJoins { relation, joins: labels }],
            None,
        )))))
    }

    /// `SELECT <returned> FROM <table> [JOIN <joined table> ON <key>]...
    /// [CROSS JOIN <labels>]... WHERE <conditions>`, every condition the arm
    /// holds written out, so the arm keeps no row its helper does not return.
    fn grant(&mut self, grant: &GrantArm<'db, ParserDB>) -> Result<SetExpr, Error> {
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
            GrantedTo::Request(comparison) => conditions.push(self.request(comparison)?),
            GrantedTo::Set(grantees) => {
                let set = self.set(grantees.set())?;
                conditions.push(in_query(qualified(ARM, column_ident(grantees.column())), set));
            }
            GrantedTo::Anyone => {}
        }
        if let Some(gate) = grant.gate() {
            conditions.extend(self.gate(gate)?);
        }
        conditions.extend(restrictions(ARM, grant.restrictions()));
        conditions.extend(time_bounds(ARM, grant.time_bounds()));
        for cap in grant.caps() {
            conditions.push(self.cap(cap)?);
        }
        for anti_join in grant.anti_joins() {
            conditions.push(self.anti_join(anti_join));
        }
        let (projection, labels) =
            returned_projection(grant.returned(), |column| qualified(ARM, column_ident(column)));
        joins.extend(labels);
        Ok(SetExpr::Select(Box::new(make_simple_select(
            projection,
            vec![TableWithJoins { relation: aliased_table(grant.table(), ARM), joins }],
            conjunction(conditions),
        ))))
    }

    /// The arm's column compared with the principal or the set the request
    /// carries, read as the body reads it.
    fn request(&self, comparison: &SettingComparison<'db, ParserDB>) -> Result<Expr, Error> {
        let setting = comparison.setting();
        let read = |empty_is_null| {
            setting_read(setting.name(), comparison.raises_when_unset(), empty_is_null)
        };
        let column = qualified(ARM, column_ident(comparison.column()));
        match setting.content() {
            SettingContent::Value(
                SettingValue::Caller(declared) | SettingValue::Principal { identity: declared, .. },
            ) => Ok(equals(column, cast(read(comparison.excludes_unset()), declared.clone()))),
            SettingContent::Value(SettingValue::Set {
                element,
                packing: SetPacking::Delimited(delimiter),
            }) => {
                // `string_to_array` unpacks `''` to no element, so the read
                // needs no `NULLIF`.
                let elements = simple_function_expr(
                    "string_to_array",
                    vec![read(false), string_literal(delimiter)],
                    None,
                );
                Ok(Expr::AnyOp {
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
                })
            }
            SettingContent::Value(SettingValue::Set {
                element,
                packing: SetPacking::JsonArray,
            }) => {
                let elements = simple_function_expr(
                    "jsonb_array_elements_text",
                    vec![cast(read(comparison.excludes_unset()), DataType::JSONB)],
                    None,
                );
                Ok(in_query(
                    column,
                    single_expr_query(cast(elements, element.clone()), Vec::new(), None),
                ))
            }
            SettingContent::Value(SettingValue::Scalar(_)) | SettingContent::Json(_) => {
                Err(Error::forward_refusal(format!(
                    "{}() compares a column with {}, which the deployment declares as neither a \
                     principal nor a set of them.",
                    self.helper,
                    setting.name()
                )))
            }
        }
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
            in_query(row, self.set(cap.set())?)
        };
        Ok(match cap.lift() {
            Some(lift) => {
                let read = setting_read(
                    lift.setting().name(),
                    lift.raises_when_unset(),
                    lift.lifts_on_empty(),
                );
                Expr::Nested(Box::new(Expr::BinaryOp {
                    left: Box::new(Expr::IsNull(Box::new(read))),
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

/// The projection of an arm returning `returned`, `read` writing a value read
/// off the arm's row, and the lists of labels it joins, one per position
/// returning labels.
fn returned_projection<S>(
    returned: &[Returned<S>],
    read: impl Fn(&S) -> Expr,
) -> (Vec<SelectItem>, Vec<Join>) {
    let mut lists = Vec::new();
    let projection = returned
        .iter()
        .enumerate()
        .map(|(position, returned)| {
            SelectItem::UnnamedExpr(match returned {
                Returned::Read(value) => read(value),
                Returned::Labels(labels) => {
                    let name = format!("labels_{position}");
                    lists.push(label_list(labels, &name));
                    qualified(&name, Ident::new(LABEL))
                }
            })
        })
        .collect();
    (projection, lists)
}

/// `CROSS JOIN (SELECT <label> AS label [UNION ALL SELECT <label>]...) AS
/// <name>`, which pairs every row of the arm with each label, and with none
/// for no label.
fn label_list(labels: &[Value], name: &str) -> Join {
    let rows = labels.iter().map(|label| {
        SetExpr::Select(Box::new(make_simple_select(
            vec![SelectItem::UnnamedExpr(Expr::Value(label.clone().into()))],
            Vec::new(),
            None,
        )))
    });
    let mut body = union(rows.collect(), SetQuantifier::All).unwrap_or_else(|| *empty_set().body);
    name_columns(&mut body, &[Ident::new(LABEL)]);
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

/// Names the columns every `SELECT` of `body` projects `columns`, which both
/// engines read the columns of a `UNION` by.
fn name_columns(body: &mut SetExpr, columns: &[Ident]) {
    match body {
        SetExpr::Select(select) => {
            for (item, column) in select.projection.iter_mut().zip(columns) {
                match item {
                    SelectItem::UnnamedExpr(expr) => {
                        let expr = core::mem::replace(expr, Expr::Value(Value::Null.into()));
                        *item = SelectItem::ExprWithAlias { expr, alias: column.clone() };
                    }
                    SelectItem::ExprWithAlias { alias, .. } => alias.clone_from(column),
                    _ => {}
                }
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            name_columns(left, columns);
            name_columns(right, columns);
        }
        SetExpr::Query(query) => name_columns(&mut query.body, columns),
        _ => {}
    }
}

/// `SELECT NULL WHERE false`, the set holding no value.
fn empty_set() -> Query {
    single_expr_query(
        Expr::Value(Value::Null.into()),
        Vec::new(),
        Some(Expr::Value(Value::Boolean(false).into())),
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
