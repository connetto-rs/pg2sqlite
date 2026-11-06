//! Tests that register sqlite-vec with `sqlite3_auto_extension`, which loads
//! it into every SQLite connection the process opens afterwards. They run in
//! their own binary so the tests in `it` keep proving their output against an
//! SQLite without the extension.

#[path = "../helpers/mod.rs"]
#[expect(dead_code, reason = "this binary uses part of the shared helpers")]
mod helpers;

mod support;
mod test_bound_param_conversion;
mod test_cast_operator;
mod test_data_type_translation;
mod test_expr_errors;
mod test_forward_misc;
mod test_manifest_column_storage;
mod test_vector_edge_cases;
mod test_vector_insert_value_wrapping;
mod test_vector_native_compatibility;
mod test_vector_semantic;
mod test_vector_translation;
