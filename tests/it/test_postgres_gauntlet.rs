//! Everything this suite checks against a real PostgreSQL. Each gauntlet keeps
//! its own module so the files stay separable.

#[path = "gauntlet/source.rs"]
mod source;

#[path = "gauntlet/harness.rs"]
mod harness;

#[path = "gauntlet/timestamptz_harness.rs"]
mod timestamptz_harness;

#[path = "gauntlet/reverse.rs"]
mod reverse;

#[path = "gauntlet/parity_rls.rs"]
mod parity_rls;

#[path = "gauntlet/parity_grants.rs"]
mod parity_grants;

#[path = "gauntlet/parity_any_array_subquery.rs"]
mod parity_any_array_subquery;

#[path = "gauntlet/parity_array_containment_subquery.rs"]
mod parity_array_containment_subquery;

#[path = "gauntlet/parity_definer_helpers.rs"]
mod parity_definer_helpers;

#[path = "gauntlet/parity_helper_conditions.rs"]
mod parity_helper_conditions;

#[path = "gauntlet/parity_lateral.rs"]
mod parity_lateral;

#[path = "gauntlet/parity_boolean_helpers.rs"]
mod parity_boolean_helpers;

#[path = "gauntlet/parity_sharable_grants.rs"]
mod parity_sharable_grants;

#[path = "gauntlet/parity_plpgsql.rs"]
mod parity_plpgsql;
#[path = "gauntlet/parity_scalar_helpers.rs"]
mod parity_scalar_helpers;

#[path = "gauntlet/parity_set_returning_helpers.rs"]
mod parity_set_returning_helpers;

#[path = "gauntlet/parity_text.rs"]
mod parity_text;

#[path = "gauntlet/parity_datetime.rs"]
mod parity_datetime;

#[path = "gauntlet/parity_ddl.rs"]
mod parity_ddl;

#[path = "gauntlet/parity_enum.rs"]
mod parity_enum;

#[path = "gauntlet/parity_dml.rs"]
mod parity_dml;

#[path = "gauntlet/parity_nulls_not_distinct.rs"]
mod parity_nulls_not_distinct;

#[path = "gauntlet/parity_definer_triggers.rs"]
mod parity_definer_triggers;

#[path = "gauntlet/parity_deferred_constraint_triggers.rs"]
mod parity_deferred_constraint_triggers;

#[path = "gauntlet/parity_deferred_helper_triggers.rs"]
mod parity_deferred_helper_triggers;

#[path = "gauntlet/parity_deferred_loop_checks.rs"]
mod parity_deferred_loop_checks;

#[path = "gauntlet/parity_values.rs"]
mod parity_values;

#[path = "gauntlet/parity_numeric.rs"]
mod parity_numeric;

#[path = "gauntlet/parity_query.rs"]
mod parity_query;

#[path = "gauntlet/parity_index_names.rs"]
mod parity_index_names;

#[path = "gauntlet/parity_json_values.rs"]
mod parity_json_values;

#[path = "gauntlet/parity_whole_row.rs"]
mod parity_whole_row;

#[path = "gauntlet/parity_deployment.rs"]
mod parity_deployment;

#[path = "gauntlet/parity_timestamptz_zone.rs"]
mod parity_timestamptz_zone;

#[path = "gauntlet/parity_policy_literals.rs"]
mod parity_policy_literals;
#[path = "gauntlet/parity_timestamptz_instants.rs"]
mod parity_timestamptz_instants;
#[path = "gauntlet/parity_timestamptz_reads.rs"]
mod parity_timestamptz_reads;

#[path = "gauntlet/parity_privileges.rs"]
mod parity_privileges;

#[path = "gauntlet/parity_withheld_columns.rs"]
mod parity_withheld_columns;

#[path = "gauntlet/parity_uuid_text.rs"]
mod parity_uuid_text;

#[path = "gauntlet/parity_fts_keys.rs"]
mod parity_fts_keys;

#[path = "gauntlet/parity_trigger_order.rs"]
mod parity_trigger_order;

#[path = "gauntlet/parity_maintenance_reentry.rs"]
mod parity_maintenance_reentry;

#[path = "gauntlet/parity_maintenance_slot.rs"]
mod parity_maintenance_slot;

#[path = "gauntlet/parity_maintenance_frame.rs"]
mod parity_maintenance_frame;

#[path = "gauntlet/parity_new_assignments.rs"]
mod parity_new_assignments;

#[path = "gauntlet/parity_trigger_body_statements.rs"]
mod parity_trigger_body_statements;

#[path = "gauntlet/parity_perform_helpers.rs"]
mod parity_perform_helpers;

#[path = "gauntlet/parity_jsonb.rs"]
mod parity_jsonb;

#[path = "gauntlet/parity_raise_sqlstate.rs"]
mod parity_raise_sqlstate;

#[path = "gauntlet/parity_trigger_if_control_flow.rs"]
mod parity_trigger_if_control_flow;

#[path = "gauntlet/parity_bool_aggregates.rs"]
mod parity_bool_aggregates;

#[path = "gauntlet/parity_per_row_checks.rs"]
mod parity_per_row_checks;

#[path = "gauntlet/parity_trigger_variable_reassignment.rs"]
mod parity_trigger_variable_reassignment;

#[path = "gauntlet/parity_trigger_unassigned_variables.rs"]
mod parity_trigger_unassigned_variables;
