//! The name a trigger's function takes when `ALTER FUNCTION ... RENAME TO`,
//! `ALTER FUNCTION ... SET SCHEMA` or `ALTER SCHEMA ... RENAME TO` moves it.
//!
//! PostgreSQL binds a trigger to the function it runs, so the trigger keeps
//! running the function under its new name.

use sqlparser::ast::{
    AlterFunction, AlterFunctionOperation, AlterFunctionSignature, AlterSchema,
    AlterSchemaOperation, CreateTrigger, ObjectName, ObjectNamePart, Statement,
};

use super::function_cascade::{parts, schema_or_public, trigger_runs};
use crate::impls::object_name::same_identifier;

/// Whether `statement` can move a function a trigger runs.
pub(crate) fn moves_functions(statement: &Statement) -> bool {
    match statement {
        Statement::AlterFunction(AlterFunction {
            signature: AlterFunctionSignature::Function(_),
            operation:
                AlterFunctionOperation::RenameTo { .. } | AlterFunctionOperation::SetSchema { .. },
        }) => true,
        Statement::AlterSchema(AlterSchema { operations, .. }) => {
            operations
                .iter()
                .any(|operation| matches!(operation, AlterSchemaOperation::Rename { .. }))
        }
        _ => false,
    }
}

/// The name a trigger running `function` names it by after `statement`, when
/// `statement` moves it.
pub(crate) fn moved(statement: &Statement, function: &ObjectName) -> Option<ObjectName> {
    let (Some(last), schema) = parts(function) else { return None };
    match statement {
        Statement::AlterFunction(AlterFunction {
            signature: AlterFunctionSignature::Function(desc),
            operation,
        }) if trigger_runs(desc, function) => {
            match operation {
                AlterFunctionOperation::RenameTo { new_name } => {
                    let mut renamed = function.clone();
                    renamed.0.pop();
                    renamed.0.push(ObjectNamePart::Identifier(new_name.clone()));
                    Some(renamed)
                }
                AlterFunctionOperation::SetSchema { schema_name } => {
                    let mut moved = schema_name.clone();
                    moved.0.push(ObjectNamePart::Identifier(last.clone()));
                    Some(moved)
                }
                _ => None,
            }
        }
        Statement::AlterSchema(AlterSchema { name, operations, .. }) => {
            let (Some(renamed), _) = parts(name) else { return None };
            if !same_identifier(renamed, &schema_or_public(schema)) {
                return None;
            }
            operations.iter().rev().find_map(|operation| {
                let AlterSchemaOperation::Rename { name } = operation else { return None };
                let mut moved = name.clone();
                moved.0.push(ObjectNamePart::Identifier(last.clone()));
                Some(moved)
            })
        }
        _ => None,
    }
}

/// Carries `trigger` along `statement`, which may move the function it runs.
pub(crate) fn carry(trigger: &mut CreateTrigger, statement: &Statement) {
    if let Some(body) = &mut trigger.exec_body
        && let Some(name) = moved(statement, &body.func_desc.name)
    {
        body.func_desc.name = name;
    }
}
