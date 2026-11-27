#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;
// The `Arbitrary` derive names `std`, and a managed connection locks its
// clocks behind a `std` mutex.
#[cfg(any(feature = "arbitrary", feature = "managed"))]
extern crate std;

mod case_fold;
mod clock;
mod helper_function;
mod jsonb;
#[cfg(feature = "managed")]
mod managed;
mod temporal;
#[cfg(feature = "managed")]
mod transaction;
mod typmod;
pub mod write_guard;

pub use case_fold::CaseFoldFunctions;
#[cfg(feature = "clock")]
pub use clock::JiffUtcProvider;
pub use clock::{ClockError, UtcInstant, UtcProvider};
#[cfg(feature = "managed")]
pub use diesel_dynamic_schema::dynamic_value::{DynamicRow, SqliteAnyValue};
pub use helper_function::HelperFunction;
pub use jsonb::JsonbFunctions;
#[cfg(feature = "managed")]
pub use managed::{
    BeginMode, CLOCK_TIMESTAMP, ExecutionError, Guard, ManagedConnection, ManagedRows, Nested,
    Outermost, READER_ERROR_PREFIX, Registration, STATEMENT_TIMESTAMP, Savepoint, StatementError,
    TRANSACTION_TIMESTAMP, Transaction,
};
pub use temporal::{
    TemporalArgument, TemporalError, TemporalFunction, TemporalFunctions, TemporalKind,
    TemporalValue, extract_scale,
};
