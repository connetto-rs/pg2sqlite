//! Source-order captures for folded maintenance bodies.

use core::ops::ControlFlow;

use sqlparser::ast::{DataType, Expr, Ident, ObjectName, Value, visit_expressions_mut};
use sqlparser_plpgsql::{
    embedded::{Assignment, Block, Branch, If, Node, NodeKind, Target},
    namespace::Signature,
};

use super::scope::fold;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{errors::Error, impls::query_builder::single_expr_query};

struct MaintenanceAssignment<'assignments> {
    path: Vec<(usize, Option<usize>)>,
    column: &'assignments str,
    data_type: &'assignments DataType,
}

pub(super) struct MaintenanceBody<'assignments> {
    block: Block,
    signature: Signature,
    paths: Vec<MaintenanceAssignment<'assignments>>,
    current: usize,
    marker: String,
    reached: String,
}

impl<'assignments> MaintenanceBody<'assignments> {
    pub(super) fn new(
        block: Block,
        signature: Signature,
        columns: &[String],
        assignments: &'assignments [(String, DataType, Expr)],
    ) -> Self {
        let mut paths = Vec::new();
        collect(&block.statements, &mut Vec::new(), assignments, &mut paths);
        let fields = block.fields_of_new(&signature).expect("maintenance body was validated");
        let mut number = 0;
        let (marker, reached) = loop {
            let marker = format!("__pg2sqlite_capture_{number}");
            let reached = format!("__pg2sqlite_reached_{number}");
            if !columns
                .iter()
                .chain(fields.iter().map(|field| &field.field.value))
                .any(|name| name == &marker || name == &reached)
            {
                break (marker, reached);
            }
            number += 1;
        };
        Self { block, signature, paths, current: 0, marker, reached }
    }

    pub(super) fn next(
        &mut self,
    ) -> Result<Option<(&'assignments str, &'assignments DataType, Expr, Expr)>, Error> {
        let Some(assignment) = self.paths.get(self.current) else { return Ok(None) };
        let path = &assignment.path;
        let column = assignment.column;
        let data_type = assignment.data_type;
        let original = node_at(&mut self.block.statements, path).clone();
        let NodeKind::Assignment(assignment) = &original.kind else { unreachable!() };
        let make_assignment = |name: &str, value| {
            Node {
                span: original.span,
                kind: NodeKind::Assignment(Assignment {
                    target: Target {
                        name: ObjectName::from(vec![Ident::new("NEW"), Ident::new(name)]),
                        indirection: Vec::new(),
                    },
                    value,
                }),
            }
        };
        let truth = || {
            Box::new(single_expr_query(
                Expr::Value(Value::Boolean(true).with_empty_span()),
                Vec::new(),
                None,
            ))
        };
        *node_at(&mut self.block.statements, path) = Node {
            span: original.span,
            kind: NodeKind::If(If {
                branches: vec![Branch {
                    condition: truth(),
                    statements: vec![
                        make_assignment(&self.marker, assignment.value.clone()),
                        make_assignment(&self.reached, truth()),
                        original.clone(),
                    ],
                }],
                otherwise: Vec::new(),
            }),
        };
        let fields = self
            .block
            .fields_of_new(&self.signature)
            .map_err(|error| Error::forward_refusal(error.to_string()));
        *node_at(&mut self.block.statements, path) = original;
        let mut value = None;
        let mut guard = None;
        for field in fields? {
            if field.field.value == self.marker {
                value = Some(with_initial(field.value, &self.marker, &Value::Null));
            } else if field.field.value == self.reached {
                guard = Some(with_initial(field.value, &self.reached, &Value::Boolean(false)));
            }
        }
        self.current += 1;
        Ok(Some((
            column,
            data_type,
            value.expect("capture field folds"),
            guard.expect("capture reachability folds"),
        )))
    }

    pub(super) fn bind(&mut self, value: Expr) {
        let node = node_at(&mut self.block.statements, &self.paths[self.current - 1].path);
        let NodeKind::Assignment(assignment) = &mut node.kind else { unreachable!() };
        *assignment.value = single_expr_query(value, Vec::new(), None);
    }

    pub(super) fn finish(self) -> Vec<(String, Expr)> {
        self.block
            .fields_of_new(&self.signature)
            .expect("bound maintenance values preserve folding")
            .into_iter()
            .map(|field| (fold(&field.field), field.value))
            .collect()
    }
}

fn with_initial(mut value: Expr, name: &str, initial: &Value) -> Expr {
    let _: ControlFlow<()> = visit_expressions_mut(&mut value, |expression| {
        if let Expr::CompoundIdentifier(parts) = expression
            && parts.len() == 2
            && parts[0].value.eq_ignore_ascii_case("new")
            && parts[1].value == name
        {
            *expression = Expr::Value(initial.clone().with_empty_span());
        }
        ControlFlow::Continue(())
    });
    value
}

fn collect<'assignments>(
    nodes: &[Node],
    prefix: &mut Vec<(usize, Option<usize>)>,
    assignments: &'assignments [(String, DataType, Expr)],
    paths: &mut Vec<MaintenanceAssignment<'assignments>>,
) {
    for (index, node) in nodes.iter().enumerate() {
        if let NodeKind::Assignment(assignment) = &node.kind {
            let target = assignment
                .target
                .name
                .0
                .last()
                .and_then(|part| part.as_ident())
                .expect("maintenance assignment has a field");
            let target = fold(target);
            for (column, data_type, _) in
                assignments.iter().filter(|(column, _, _)| column == &target)
            {
                prefix.push((index, None));
                paths.push(MaintenanceAssignment { path: prefix.clone(), column, data_type });
                prefix.pop();
            }
        }
        let NodeKind::If(conditional) = &node.kind else { continue };
        for (branch, body) in conditional.branches.iter().enumerate() {
            prefix.push((index, Some(branch)));
            collect(&body.statements, prefix, assignments, paths);
            prefix.pop();
        }
        prefix.push((index, None));
        collect(&conditional.otherwise, prefix, assignments, paths);
        prefix.pop();
    }
}

fn node_at<'body>(nodes: &'body mut [Node], path: &[(usize, Option<usize>)]) -> &'body mut Node {
    let (index, branch) = path[0];
    let node = &mut nodes[index];
    if path.len() == 1 {
        return node;
    }
    let NodeKind::If(conditional) = &mut node.kind else { unreachable!() };
    let children = match branch {
        Some(branch) => &mut conditional.branches[branch].statements,
        None => &mut conditional.otherwise,
    };
    node_at(children, &path[1..])
}
