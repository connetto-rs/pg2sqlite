//! Collapsed `<details>` form exposing the live `WebOptions`.
//! Binds to `WebOptions` rather than `Pg2SqliteOptions` directly because the
//! upstream builder can only set, never clear, a field.

use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::fa_solid_icons::FaSliders};
use pg2sqlite::prelude::{ArrayRepresentation, UuidRepresentation};

use crate::state::{AppState, SessionVarEntry};

#[component]
pub fn OptionsPanel() -> Element {
    rsx! {
        details { class: "options-panel",
            summary {
                Icon {
                    width: 14,
                    height: 14,
                    icon: FaSliders,
                    class: "summary-icon".to_string(),
                }
                "Advanced options"
            }
            div { class: "options-grid",
                UuidRepRow {}
                ArrayRepRow {}
                UuidFunctionRow {}
                UuidV7FunctionRow {}
                RlsAuditRow {}
                SessionVarsRow {}
            }
        }
    }
}

#[component]
fn ArrayRepRow() -> Element {
    let state: AppState = use_context();
    let current = state.options.read().array_representation;

    let set_rep = move |rep: Option<ArrayRepresentation>| {
        let mut options = state.options;
        let mut opts = options.read().clone();
        opts.array_representation = rep;
        options.set(opts);
    };

    rsx! {
        div { class: "options-row",
            span { class: "options-label", "Array representation" }
            div { class: "options-control",
                label {
                    input {
                        r#type: "radio",
                        name: "array-rep",
                        checked: current.is_none(),
                        onchange: move |_| set_rep(None),
                    }
                    " (none - errors on array constructs)"
                }
                label {
                    input {
                        r#type: "radio",
                        name: "array-rep",
                        checked: current == Some(ArrayRepresentation::Json),
                        onchange: move |_| set_rep(Some(ArrayRepresentation::Json)),
                    }
                    " JSON (TEXT column, json1 operations)"
                }
            }
        }
    }
}

#[component]
fn UuidRepRow() -> Element {
    let state: AppState = use_context();
    let current = state.options.read().uuid_representation;

    let set_rep = move |rep: Option<UuidRepresentation>| {
        let mut options = state.options;
        let mut opts = options.read().clone();
        opts.uuid_representation = rep;
        options.set(opts);
    };

    rsx! {
        div { class: "options-row",
            span { class: "options-label", "UUID representation" }
            div { class: "options-control",
                label {
                    input {
                        r#type: "radio",
                        name: "uuid-rep",
                        checked: current.is_none(),
                        onchange: move |_| set_rep(None),
                    }
                    " (none - errors on UUID columns)"
                }
                label {
                    input {
                        r#type: "radio",
                        name: "uuid-rep",
                        checked: current == Some(UuidRepresentation::Blob),
                        onchange: move |_| set_rep(Some(UuidRepresentation::Blob)),
                    }
                    " Blob (16 bytes)"
                }
                label {
                    input {
                        r#type: "radio",
                        name: "uuid-rep",
                        checked: current == Some(UuidRepresentation::Text),
                        onchange: move |_| set_rep(Some(UuidRepresentation::Text)),
                    }
                    " Text (36-char string)"
                }
            }
        }
    }
}

#[component]
fn UuidFunctionRow() -> Element {
    let state: AppState = use_context();
    let current = state.options.read().uuid_function_name.clone();

    rsx! {
        div { class: "options-row",
            span { class: "options-label", "UUID function name" }
            div { class: "options-control",
                input {
                    r#type: "text",
                    placeholder: "uuidv7",
                    value: current,
                    oninput: move |evt| {
                        let mut options = state.options;
                        let mut opts = options.read().clone();
                        opts.uuid_function_name = evt.value();
                        options.set(opts);
                    },
                }
            }
        }
    }
}

#[component]
fn UuidV7FunctionRow() -> Element {
    let state: AppState = use_context();
    let current = state.options.read().uuid_v7_function_name.clone();

    rsx! {
        div { class: "options-row",
            span { class: "options-label", "UUID v7 function name" }
            div { class: "options-control",
                input {
                    r#type: "text",
                    placeholder: "uuid7",
                    value: current,
                    oninput: move |evt| {
                        let mut options = state.options;
                        let mut opts = options.read().clone();
                        opts.uuid_v7_function_name = evt.value();
                        options.set(opts);
                    },
                }
            }
        }
    }
}

#[component]
fn RlsAuditRow() -> Element {
    let state: AppState = use_context();
    let current = state.options.read().rls_audit_table_name.clone();

    rsx! {
        div { class: "options-row",
            span { class: "options-label", "RLS audit table" }
            div { class: "options-control",
                input {
                    r#type: "text",
                    placeholder: "rls_violations",
                    value: current,
                    oninput: move |evt| {
                        let mut options = state.options;
                        let mut opts = options.read().clone();
                        opts.rls_audit_table_name = evt.value();
                        options.set(opts);
                    },
                }
            }
        }
    }
}

/// Lists the current session-variable entries and lets the user add
/// or remove `current_setting('<name>')::<type> -> <sqlite_fn>()` pairs.
#[component]
fn SessionVarsRow() -> Element {
    let state: AppState = use_context();
    let entries: Vec<SessionVarEntry> = state.options.read().session_variables.clone();
    let mut new_setting = use_signal(String::new);
    let mut new_type = use_signal(String::new);
    let mut new_function = use_signal(String::new);

    rsx! {
        div { class: "options-row",
            span { class: "options-label", "Session variables" }
            div { class: "options-control session-vars",
                if entries.is_empty() {
                    span { class: "options-muted", "None configured." }
                }
                for (idx, entry) in entries.iter().enumerate() {
                    div { class: "session-var-row",
                        code { "current_setting('{entry.setting}')" }
                        code { " :: {entry.pg_type}" }
                        span { " → " }
                        code { "{entry.function}()" }
                        button {
                            r#type: "button",
                            class: "session-var-remove",
                            title: "Remove this entry",
                            onclick: move |_| {
                                let mut options = state.options;
                                let mut opts = options.read().clone();
                                opts.session_variables.remove(idx);
                                options.set(opts);
                            },
                            "×"
                        }
                    }
                }
                div { class: "session-var-form",
                    input {
                        r#type: "text",
                        placeholder: "app.user_id",
                        value: new_setting(),
                        oninput: move |evt| new_setting.set(evt.value()),
                    }
                    input {
                        r#type: "text",
                        placeholder: "text",
                        value: new_type(),
                        oninput: move |evt| new_type.set(evt.value()),
                    }
                    input {
                        r#type: "text",
                        placeholder: "current_app_user",
                        value: new_function(),
                        oninput: move |evt| new_function.set(evt.value()),
                    }
                    button {
                        r#type: "button",
                        disabled: new_setting().is_empty() || new_function().is_empty(),
                        onclick: move |_| {
                            let setting = new_setting();
                            let pg_type = new_type();
                            let function = new_function();
                            if setting.is_empty() || function.is_empty() {
                                return;
                            }
                            let mut options = state.options;
                            let mut opts = options.read().clone();
                            opts.session_variables.push(SessionVarEntry {
                                setting,
                                pg_type: if pg_type.is_empty() {
                                    "text".to_string()
                                } else {
                                    pg_type
                                },
                                function,
                            });
                            options.set(opts);
                            new_setting.set(String::new());
                            new_type.set(String::new());
                            new_function.set(String::new());
                        },
                        "Add"
                    }
                }
            }
        }
    }
}
