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
#[cfg(feature = "parity")]
pub mod parity;
pub mod pg2sqlite;
#[cfg(feature = "diesel")]
pub mod programs;
pub mod script;
pub mod sqlstate;
pub mod traits;
pub mod warnings;
pub(crate) mod write_guard;

/// Prelude module for the library.
pub mod prelude {
    pub use pg2sqlite_runtime::{
        BeginMode, ExecutionGroup, TemporalArgument, TemporalError, TemporalValue,
        TransactionControl, extract_scale,
        write_guard::{GuardRefusal, guard_refusal},
    };
    #[cfg(feature = "managed")]
    pub use pg2sqlite_runtime::{
        ClockError, ExecutionError, JiffUtcProvider, ManagedConnection, UtcInstant, UtcProvider,
    };

    #[cfg(feature = "managed")]
    pub use crate::script::OpenError;
    pub use crate::{
        errors::{Error, RefusalCategory, SqlParseError, TranslationDirection, TranslationRefusal},
        initialization::{InitializationError, InitializationPlan},
        manifest::{
            ColumnManifestEntry, ColumnStorage, Placement, TableManifestEntry, VectorElement,
            WrapperKind,
        },
        options::{AttachedTables, Pg2SqliteOptions, TranslationContext},
        pg2sqlite::Pg2Sqlite,
        script::{FreshFiles, Group, HelperFunction, Script},
        sqlstate::{PostgresError, SqlState, postgres_error},
        traits::{
            ArrayRepresentation, CaseFoldFunctions, JsonbFunctions, ReverseTranslator, Schema,
            SessionVariableMapping, TemporalFunction, TemporalFunctions, TemporalKind, Translator,
            TranslatorWithContext, UuidRepresentation, UuidVersion,
        },
        warnings::{TranslationReport, TranslationWarning, WarningSink},
    };
}
