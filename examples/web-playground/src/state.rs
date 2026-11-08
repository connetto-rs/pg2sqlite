//! Shared Dioxus signals for the web app.
//!
//! Per-signal so Dioxus's reactivity tracks each piece independently:
//! the SQLite output pane shouldn't invalidate when the user toggles
//! a checkbox in the Advanced options, and vice versa.

use dioxus::prelude::*;
use pg2sqlite::prelude::{
    ArrayRepresentation, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation,
};
use sql_traits::structs::{SessionSetting, SettingValue};
use sqlparser::{ast::DataType, dialect::PostgreSqlDialect, parser::Parser};

use crate::{request, runner::QueryOutcome, samples::SAMPLES};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    Parser,
    Schema,
    Unsupported,
    ConfigRequired,
    Other,
}

impl ErrorCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Parser => "Parser",
            Self::Schema => "Schema",
            Self::Unsupported => "Unsupported",
            Self::ConfigRequired => "Config required",
            Self::Other => "Error",
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct TranslationError {
    pub category: ErrorCategory,
    pub message: String,
}

#[derive(Clone, Copy, PartialEq)]
pub struct TranslationStats {
    pub statement_count: usize,
    pub elapsed_ms: f64,
}

/// One session-variable entry the user configured: the setting
/// `current_setting` reads, the PostgreSQL type its values carry, and the
/// SQLite function the replica registers as its answer.
#[derive(Clone, PartialEq)]
pub struct SessionVarEntry {
    pub setting: String,
    pub pg_type: String,
    pub function: String,
}

impl Default for SessionVarEntry {
    fn default() -> Self {
        Self { setting: String::new(), pg_type: "text".to_string(), function: String::new() }
    }
}

/// Mirrors `Pg2SqliteOptions` with plain public fields. The upstream builder
/// can only set fields, never clear them, so we rebuild from scratch each
/// translation.
#[derive(Clone, PartialEq)]
pub struct WebOptions {
    pub uuid_representation: Option<UuidRepresentation>,
    pub array_representation: Option<ArrayRepresentation>,
    /// Empty means "leave at the crate default", which is `uuid`.
    pub uuid_function_name: String,
    /// Empty means "the destination has no version 7 generator", which is the
    /// crate default and makes `uuidv7()` a refusal.
    pub uuid_v7_function_name: String,
    /// Empty means "don't configure", which is fine for non-RLS schemas.
    pub rls_audit_table_name: String,
    pub session_variables: Vec<SessionVarEntry>,
    /// The server's `TimeZone`, which a `timestamptz` read and a temporal
    /// default answer from.
    pub time_zone: String,
}

impl Default for WebOptions {
    fn default() -> Self {
        Self {
            uuid_representation: Some(UuidRepresentation::Blob),
            array_representation: Some(ArrayRepresentation::Json),
            uuid_function_name: String::new(),
            uuid_v7_function_name: String::new(),
            rls_audit_table_name: String::new(),
            session_variables: Vec::new(),
            time_zone: request::DEFAULT_TIME_ZONE.to_string(),
        }
    }
}

impl WebOptions {
    pub fn to_options(&self) -> Pg2SqliteOptions {
        let mut opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();
        if let Some(rep) = self.uuid_representation {
            opts = opts.with_uuid_representation(rep);
        }
        if let Some(rep) = self.array_representation {
            opts = opts.with_array_representation(rep);
        }
        if !self.uuid_function_name.is_empty() {
            opts = opts.with_uuid_function_name(self.uuid_function_name.clone());
        }
        if !self.uuid_v7_function_name.is_empty() {
            opts = opts.with_uuid_v7_function_name(self.uuid_v7_function_name.clone());
        }
        if !self.rls_audit_table_name.is_empty() {
            opts = opts.with_rls_audit_table_name(self.rls_audit_table_name.clone());
        }
        // An entry whose type does not parse, or whose name is no custom
        // setting name, is skipped from the deployment, so a read of that
        // setting is refused as undeclared.
        let mut settings = Vec::new();
        let mut functions: Vec<(String, String)> = Vec::new();
        for entry in &self.session_variables {
            if functions.iter().any(|(declared, _)| declared.eq_ignore_ascii_case(&entry.setting)) {
                continue;
            }
            let Some(data_type) = parse_pg_type(&entry.pg_type) else {
                continue;
            };
            let Ok(setting) =
                SessionSetting::value(&entry.setting, SettingValue::Scalar(data_type))
            else {
                continue;
            };
            settings.push(setting.may_be_unset());
            functions.push((entry.setting.clone(), entry.function.clone()));
        }
        // A zone PostgreSQL refuses leaves the server's zone unset, so a read
        // needing it is refused and the options panel names the zone.
        let (deployment, _) = request::deployment(&self.time_zone, settings);
        opts = opts.with_deployment(deployment);
        for (setting, function) in functions {
            opts = opts.with_session_variable(SessionVariableMapping::new(setting, function));
        }
        opts
    }

    /// Why PostgreSQL refuses [`time_zone`](Self::time_zone), when it does.
    pub fn time_zone_error(&self) -> Option<String> {
        request::deployment(&self.time_zone, Vec::new()).1.map(|error| error.to_string())
    }
}

/// The PostgreSQL type `spelling` names, if it names one.
fn parse_pg_type(spelling: &str) -> Option<DataType> {
    Parser::new(&PostgreSqlDialect {})
        .try_with_sql(spelling)
        .and_then(|mut parser| parser.parse_data_type())
        .ok()
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum QueryDialect {
    #[default]
    Postgres,
    Sqlite,
}

#[derive(Clone, PartialEq)]
pub struct QueryDisplay {
    pub effective_sql: String,
    pub outcome: QueryOutcome,
}

pub type ReverseOutcome = Result<String, TranslationError>;

#[derive(Clone, Copy)]
pub struct AppState {
    pub pg_input: Signal<String>,
    /// Kept alongside `pg_input` so the pane can reorder and remove files,
    /// rebuilding `pg_input` from this list.
    pub input_files: Signal<Vec<(String, String)>>,
    /// Bumped after a discrete edit (sample badge, file operation) so the
    /// auto-translate watcher skips its typing debounce.
    pub translate_now: Signal<u32>,
    pub sqlite_output: Signal<Option<String>>,
    pub translation_error: Signal<Option<TranslationError>>,
    pub apply_error: Signal<Option<String>>,
    pub apply_ok: Signal<bool>,
    pub stats: Signal<Option<TranslationStats>>,
    pub options: Signal<WebOptions>,
    pub query_input: Signal<String>,
    pub query_dialect: Signal<QueryDialect>,
    pub query_result: Signal<Option<QueryDisplay>>,
    pub reverse_input: Signal<String>,
    pub reverse_output: Signal<Option<ReverseOutcome>>,
}

impl AppState {
    /// Construct initial state. Seeded with the first sample so a first visitor
    /// lands on a real translation.
    pub fn new() -> Self {
        let seed = &SAMPLES[0];
        let mut options = WebOptions::default();
        (seed.apply_options)(&mut options);
        Self {
            pg_input: Signal::new(seed.sql.to_string()),
            input_files: Signal::new(Vec::new()),
            translate_now: Signal::new(0),
            sqlite_output: Signal::new(None),
            translation_error: Signal::new(None),
            apply_error: Signal::new(None),
            apply_ok: Signal::new(false),
            stats: Signal::new(None),
            options: Signal::new(options),
            query_input: Signal::new(String::new()),
            query_dialect: Signal::new(QueryDialect::default()),
            query_result: Signal::new(None),
            reverse_input: Signal::new(String::new()),
            reverse_output: Signal::new(None),
        }
    }

    /// Call after setting `pg_input` from a discrete action rather than typing.
    pub fn request_immediate_translation(&self) {
        let mut counter = self.translate_now;
        let next = counter.peek().wrapping_add(1);
        counter.set(next);
    }
}
