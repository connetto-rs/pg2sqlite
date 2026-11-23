//! The stack the translator runs on.

/// The most one step of a translator recursion cycle uses, which a debug build
/// takes up to about 140 KiB of.
#[cfg(feature = "std")]
const HEADROOM: usize = 256 * 1024;

/// Raises `recursive`'s process-wide minimum to [`HEADROOM`], never lowering
/// it.
pub(crate) fn ensure() {
    #[cfg(feature = "std")]
    if recursive::get_minimum_stack_size() < HEADROOM {
        recursive::set_minimum_stack_size(HEADROOM);
    }
}

/// The stack a translation entry point runs on, room for `sqlparser`'s
/// derived `Clone`, `Display`, `Hash` and `Visit`, which recurse over a whole
/// statement without a guard.
#[cfg(feature = "std")]
const DEEP_STACK: usize = 64 * 1024 * 1024;

/// Runs `work` with at least half of [`DEEP_STACK`] left, on a stack grown on
/// the heap when less remains.
pub(crate) fn on_deep_stack<R>(work: impl FnOnce() -> R) -> R {
    ensure();
    #[cfg(feature = "std")]
    return stacker::maybe_grow(DEEP_STACK / 2, DEEP_STACK, work);
    #[cfg(not(feature = "std"))]
    work()
}
