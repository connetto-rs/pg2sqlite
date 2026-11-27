#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;
// The `Arbitrary` derive names `std`, and a managed connection locks its
// clocks behind a `std` mutex.
#[cfg(any(feature = "arbitrary", feature = "managed"))]
extern crate std;

mod case_fold;
mod clock;
mod group;
mod helper_function;
mod jsonb;
#[cfg(feature = "managed")]
mod managed;
#[cfg(feature = "managed")]
mod transaction;
mod typmod;
pub mod write_guard;

pub use case_fold::CaseFoldFunctions;
#[cfg(feature = "clock")]
pub use clock::JiffUtcProvider;
pub use clock::{
    CLOCK_TIMESTAMP, ClockError, STATEMENT_TIMESTAMP, TRANSACTION_TIMESTAMP, UtcInstant,
    UtcProvider,
};
#[cfg(feature = "managed")]
pub use diesel_dynamic_schema::dynamic_value::{DynamicRow, SqliteAnyValue};
pub use group::{BeginMode, ExecutionGroup, TransactionControl};
pub use helper_function::HelperFunction;
pub use jsonb::JsonbFunctions;
#[cfg(feature = "managed")]
pub use managed::{
    ExecutionError, ManagedConnection, ManagedCursor, ManagedRows, ManagedTransactionManager,
    READER_ERROR_PREFIX, Registration, StatementError,
};
pub use typmod::{RoundingError, TypmodRounding};
