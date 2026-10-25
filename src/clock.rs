//! `Instant` that also works on `wasm32-unknown-unknown`, where `std::time::Instant::now`
//! panics. There the wall-clock extraction budgets never trip (elapsed is always zero);
//! the count-based limits still apply.

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;

/// Stand-in clock for wasm32 (no time source in the sandbox).
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Debug)]
pub struct Instant;

#[cfg(target_arch = "wasm32")]
impl Instant {
    /// Start a clock.
    pub fn now() -> Self {
        Self
    }

    /// Always zero.
    pub fn elapsed(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}
