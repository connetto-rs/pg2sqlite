#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

/// The `alloc` items the std prelude brings into scope, which every module
/// glob-imports when it builds without std.
#[cfg(not(feature = "std"))]
mod no_std {
    pub(crate) mod prelude {
        pub(crate) use alloc::{
            borrow::ToOwned,
            boxed::Box,
            format,
            string::{String, ToString},
            vec,
            vec::Vec,
        };
    }
}

pub mod errors;
pub(crate) mod history;
pub(crate) mod impls;
pub mod initialization;
pub mod manifest;
pub mod options;
pub mod pg2sqlite;
pub mod script;
pub mod traits;
pub mod warnings;
pub mod write_guard;

/// Prelude module for the library.
pub mod prelude {
    pub use crate::{
        errors::{Error, RefusalCategory, SqlParseError, TranslationDirection, TranslationRefusal},
        initialization::{InitializationError, InitializationPlan},
        manifest::{
            ColumnManifestEntry, ColumnStorage, Placement, TableManifestEntry, VectorElement,
            WrapperKind,
        },
        options::{AttachedTables, Pg2SqliteOptions, TranslationContext},
        pg2sqlite::Pg2Sqlite,
        script::{FreshFiles, Script},
        traits::{
            ArrayRepresentation, ReverseTranslator, Schema, SessionVariableMapping,
            TimeZoneFunctions, Translator, TranslatorWithContext, UuidRepresentation, UuidVersion,
        },
        warnings::{TranslationReport, TranslationWarning, WarningSink},
        write_guard::is_policy_refusal,
    };
}
