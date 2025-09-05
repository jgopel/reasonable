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
