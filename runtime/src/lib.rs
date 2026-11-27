#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;
// The `Arbitrary` derive names `std`, and the diesel authorizer keeps its
// last refusal behind a `std` mutex.
#[cfg(any(feature = "arbitrary", feature = "diesel"))]
extern crate std;

mod case_fold;
mod helper_function;
mod jsonb;
pub mod statement_refusals;
pub mod write_guard;

pub use case_fold::CaseFoldFunctions;
pub use helper_function::HelperFunction;
pub use jsonb::JsonbFunctions;
