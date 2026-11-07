//! Everything this suite checks against a real PostgreSQL. Each gauntlet keeps
//! its own module so the files stay separable.

#[path = "gauntlet/source.rs"]
mod source;

#[path = "gauntlet/harness.rs"]
mod harness;

#[path = "gauntlet/reverse.rs"]
mod reverse;

#[path = "gauntlet/parity_rls.rs"]
mod parity_rls;

#[path = "gauntlet/parity_grants.rs"]
mod parity_grants;

#[path = "gauntlet/parity_any_array_subquery.rs"]
mod parity_any_array_subquery;

#[path = "gauntlet/parity_definer_helpers.rs"]
mod parity_definer_helpers;

#[path = "gauntlet/parity_plpgsql.rs"]
mod parity_plpgsql;

#[path = "gauntlet/parity_text.rs"]
mod parity_text;

#[path = "gauntlet/parity_datetime.rs"]
mod parity_datetime;

#[path = "gauntlet/parity_ddl.rs"]
mod parity_ddl;

#[path = "gauntlet/parity_dml.rs"]
mod parity_dml;

#[path = "gauntlet/parity_nulls_not_distinct.rs"]
mod parity_nulls_not_distinct;

#[path = "gauntlet/parity_definer_triggers.rs"]
mod parity_definer_triggers;

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

#[path = "gauntlet/parity_deployment.rs"]
mod parity_deployment;

#[path = "gauntlet/parity_timestamptz_zone.rs"]
mod parity_timestamptz_zone;

#[path = "gauntlet/parity_privileges.rs"]
mod parity_privileges;

#[path = "gauntlet/parity_table_not_null.rs"]
mod parity_table_not_null;
