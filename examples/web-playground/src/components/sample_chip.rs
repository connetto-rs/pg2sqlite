//! The sample-query chip button, shared by the query and reverse panels.

use dioxus::prelude::*;
use dioxus_free_icons::{
    Icon,
    icons::fa_solid_icons::{FaBan, FaCircleCheck},
};

use crate::samples::SampleQueryKind;

/// A sample-query chip whose click behaviour the caller supplies.
#[component]
pub fn SampleQueryChip(
    label: &'static str,
    sql: &'static str,
    kind: SampleQueryKind,
    on_click: EventHandler<MouseEvent>,
) -> Element {
    let class = match kind {
        SampleQueryKind::Positive => "sample-query-chip",
        SampleQueryKind::Negative => "sample-query-chip negative",
    };

    rsx! {
        button {
            r#type: "button",
            class: class,
            title: "{sql}",
            onclick: on_click,
            match kind {
                SampleQueryKind::Positive => rsx! {
                    Icon { width: 12, height: 12, icon: FaCircleCheck, class: "chip-icon".to_string() }
                },
                SampleQueryKind::Negative => rsx! {
                    Icon { width: 12, height: 12, icon: FaBan, class: "chip-icon".to_string() }
                },
            }
            "{label}"
        }
    }
}
