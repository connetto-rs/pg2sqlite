//! A `RETURN` ends the body it runs in, and the emitted trigger has no control
//! flow to leave early. A body is therefore rewritten before it is translated,
//! the statements after a `RETURN`, or after an `IF` or block holding one,
//! moved into the paths that do not return, so every `RETURN` ends the list it
//! sits in and the branch guards alone decide what runs.

use sqlparser_plpgsql::embedded::{Block, If, Node, NodeKind};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// `nodes` with what follows each `RETURN`, and each `IF` or block that may
/// return, moved into the paths that do not return, so every `RETURN` ends
/// the list it sits in.
pub(super) fn without_early_returns(nodes: Vec<Node>) -> Vec<Node> {
    with_continuation(nodes, &[])
}

/// `nodes` followed by `continuation`, run on every path that does not
/// return, with no node after a `RETURN`. `continuation` already has none.
pub(super) fn with_continuation(nodes: Vec<Node>, continuation: &[Node]) -> Vec<Node> {
    let mut result = Vec::with_capacity(nodes.len() + continuation.len());
    let mut nodes = nodes.into_iter();
    while let Some(node) = nodes.next() {
        let rest = || with_continuation(nodes.as_slice().to_vec(), continuation);
        let Node { kind, span } = node;
        match kind {
            NodeKind::Return { .. } => {
                result.push(Node { kind, span });
                return result;
            }
            NodeKind::If(conditional) if may_return_in(&conditional) => {
                let rest = rest();
                let If { branches, otherwise } = conditional;
                let branches = branches
                    .into_iter()
                    .map(|mut branch| {
                        branch.statements = with_continuation(branch.statements, &rest);
                        branch
                    })
                    .collect();
                let otherwise = with_continuation(otherwise, &rest);
                result.push(Node { kind: NodeKind::If(If { branches, otherwise }), span });
                return result;
            }
            NodeKind::Block(block) if block.statements.iter().any(may_return) => {
                let rest = rest();
                let Block { label, declarations, statements, handlers } = block;
                let statements = with_continuation(statements, &rest);
                let block = Block { label, declarations, statements, handlers };
                result.push(Node { kind: NodeKind::Block(block), span });
                return result;
            }
            kind => result.push(Node { kind, span }),
        }
    }
    result.extend(continuation.iter().cloned());
    result
}

/// Whether `node` holds a `RETURN` on some path through it.
fn may_return(node: &Node) -> bool {
    match &node.kind {
        NodeKind::Return { .. } => true,
        NodeKind::If(conditional) => may_return_in(conditional),
        NodeKind::Block(block) => block.statements.iter().any(may_return),
        _ => false,
    }
}

/// Whether a branch of `conditional` holds a `RETURN`.
fn may_return_in(conditional: &If) -> bool {
    conditional
        .branches
        .iter()
        .flat_map(|branch| &branch.statements)
        .chain(&conditional.otherwise)
        .any(may_return)
}

#[cfg(test)]
mod tests {
    use sqlparser_plpgsql::embedded::embedded_sql;

    use super::*;

    fn statements(body: &str) -> Vec<Node> {
        embedded_sql(body).expect("the body parses").statements
    }

    /// The statements after an `IF` that returns run only on the branches
    /// that do not, and a `RETURN` ends its list.
    #[test]
    fn what_follows_a_return_moves_into_the_branches_that_do_not_return() {
        let moved = without_early_returns(statements(
            "BEGIN
                 IF a THEN RETURN; ELSIF b THEN PERFORM 1; END IF;
                 PERFORM 2;
                 RETURN;
                 PERFORM 3;
             END",
        ));
        let expected = statements(
            "BEGIN
                 IF a THEN RETURN;
                 ELSIF b THEN PERFORM 1; PERFORM 2; RETURN;
                 ELSE PERFORM 2; RETURN;
                 END IF;
             END",
        );
        assert_eq!(moved, expected);
    }
}
