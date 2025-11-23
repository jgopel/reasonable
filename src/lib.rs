//! # Reasonable
//!
//! `reasonable` provides a rate-limiting mechanism that enforces a cooldown period
//! after a resource has been used.
//!
//! The main type is [`UnfairRateLimiter`], which limits the number of simultaneous
//! "permits". Unlike a standard semaphore or token bucket, `reasonable` is designed
//! for scenarios where the cooldown should depend on when the resource is *released*
//! (returned), not when it was acquired.
//!
//! When a permit is dropped, the slot it occupied remains unavailable for a specified
//! duration. This is useful for rate-limiting access to APIs or resources where
//! the "cost" is paid upon completion or where you want to enforce a quiet period
//! after usage.

mod unfair_rate_limiter;

pub use unfair_rate_limiter::*;

/// A trait for all the rate limiters in this crate.
pub trait RateLimiter {
    /// A RAII permit for a single unit of concurrency.
    type SinglePermit<'a>
    where
        Self: 'a;

    /// A RAII permit for multiple units of concurrency.
    type MultiPermit<'a>
    where
        Self: 'a;

    /// The error type returned when a permit cannot be acquired.
    type Error<'a>
    where
        Self: 'a;

    /// Attempts to acquire a single permit.
    ///
    /// # Errors
    ///
    /// Returns an error if a permit cannot be acquired.
    fn try_acquire_permit(&self) -> Result<Self::SinglePermit<'_>, Self::Error<'_>>;

    /// Attempts to acquire multiple permits at once.
    ///
    /// # Errors
    ///
    /// Returns an error if the requested number of permits cannot be acquired.
    fn try_acquire_permits(
        &self,
        num_permits: usize,
    ) -> Result<Self::MultiPermit<'_>, Self::Error<'_>>;
}
