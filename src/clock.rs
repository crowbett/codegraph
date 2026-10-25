//! Clocks that also work on `wasm32-unknown-unknown`, where `Instant::now` and
//! `SystemTime::now` panic. There the wall-clock extraction budgets never trip (elapsed is
//! always zero; the count-based limits still apply) and the Unix time is fixed.

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

/// Time since the Unix epoch (zero if the clock is before it).
#[cfg(not(target_arch = "wasm32"))]
pub fn since_epoch() -> std::time::Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}

/// Time since the Unix epoch: fixed at one second on wasm32, so stored timestamps are
/// deterministic and never the 0 that means "not indexed yet".
#[cfg(target_arch = "wasm32")]
pub fn since_epoch() -> std::time::Duration {
    std::time::Duration::from_secs(1)
}
