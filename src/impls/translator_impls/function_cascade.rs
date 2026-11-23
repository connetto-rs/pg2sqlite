//! The triggers `DROP FUNCTION ... CASCADE` drops along with the functions they
//! run.
//!
//! SQLite inlines a trigger function into each trigger running it, so the
//! translation drops those triggers as a written `DROP TRIGGER` would. Any
//! other object the cascade takes stays in SQLite, so such a cascade is
//! refused.

use sql_traits::structs::ParserDBIngestor;
use sqlparser::ast::{
    ArgMode, CreateTrigger, DropBehavior, DropFunction, DropTrigger, Ident, ObjectName,
    ObjectNamePart, Statement,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{errors::Error, impls::object_name::same_identifier};

/// Whether a trigger running `function` runs one of the functions `statement`
/// drops with `CASCADE`.
pub(crate) fn runs_dropped(statement: &Statement, function: &ObjectName) -> bool {
    let Statement::DropFunction(DropFunction {
        func_desc,
        drop_behavior: Some(DropBehavior::Cascade),
        ..
    }) = statement
    else {
        return false;
    };
    // A trigger function takes no argument.
    func_desc.iter().any(|dropped| {
        dropped
            .args
            .as_deref()
            .is_none_or(|args| args.iter().all(|arg| arg.mode == Some(ArgMode::Out)))
            && same_function(&dropped.name, function)
    })
}

/// Whether two written names reach one function, a bare one in `public`.
fn same_function(left: &ObjectName, right: &ObjectName) -> bool {
    let public = Ident::new("public");
    let ((Some(left), left_schema), (Some(right), right_schema)) = (parts(left), parts(right))
    else {
        return false;
    };
    same_identifier(left, right)
        && same_identifier(left_schema.unwrap_or(&public), right_schema.unwrap_or(&public))
}

/// The last part of `name` and the schema before it.
fn parts(name: &ObjectName) -> (Option<&Ident>, Option<&Ident>) {
    let mut idents = name.0.iter().rev().filter_map(ObjectNamePart::as_ident);
    (idents.next(), idents.next())
}

/// The `DROP TRIGGER` taking `trigger` off its table.
pub(crate) fn drop_of(trigger: &CreateTrigger) -> Statement {
    Statement::DropTrigger(DropTrigger {
        if_exists: false,
        trigger_name: trigger.name.clone(),
        table_name: Some(trigger.table_name.clone()),
        option: None,
    })
}

/// Refuses `statement` when it is a `DROP FUNCTION ... CASCADE` that `input`
/// says takes more than the triggers running the function.
///
/// # Errors
///
/// Returns the refusal naming the first other dependent, or the error the
/// drop meets without `CASCADE` once those triggers are gone.
pub(crate) fn reject_other_dependents(
    statement: &Statement,
    input: &ParserDBIngestor,
) -> Result<(), Error> {
    use sql_traits::traits::DatabaseLike;

    let Statement::DropFunction(drop) = statement else { return Ok(()) };
    if drop.drop_behavior != Some(DropBehavior::Cascade) {
        return Ok(());
    }
    let schema = input.snapshot();
    let triggers: Vec<Statement> = schema
        .triggers()
        .filter(|trigger| {
            trigger
                .exec_body
                .as_ref()
                .is_some_and(|body| runs_dropped(statement, &body.func_desc.name))
        })
        .map(drop_of)
        .collect();
    let mut probe = schema.into_ingestor();
    for trigger in triggers {
        probe = probe.apply_statement(trigger)?;
    }
    let restricted = DropFunction { drop_behavior: None, ..drop.clone() };
    match probe.apply_statement(Statement::DropFunction(restricted)) {
        Ok(_) => Ok(()),
        Err(sql_traits::errors::Error::FunctionReferenced {
            function_name,
            dependent_kind,
            dependent_name,
            ..
        }) => {
            Err(Error::forward_refusal(format!(
                "`{statement}` also drops {} `{dependent_name}` along with function \
                 `{function_name}`, and SQLite would keep it. Drop it before the function.",
                dependent_kind.to_string().to_lowercase()
            )))
        }
        Err(error) => Err(error.into()),
    }
}
