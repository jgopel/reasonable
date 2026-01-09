//! Example usage of the `UnfairRateLimiter` in an async context.
//!
//! This example demonstrates how to use the rate limiter to control access to a shared resource
//! where the "cost" (cooldown) is applied after the work is done.

use chrono::Duration;
use reasonable::{Error, RateLimiter, UnfairRateLimiter};

const DEFAULT_SLEEP: std::time::Duration = std::time::Duration::from_millis(100);

#[tokio::main]
async fn main() {
    // Create a limiter allowing 1 concurrent operation.
    // Once an operation finishes, that slot is unavailable for 1 second.
    let limiter = UnfairRateLimiter::<1>::new(Duration::seconds(1));

    println!("--- Starting Async Example ---");

    {
        println!("Acquiring permit...");
        let _permit = limiter
            .try_acquire_permit()
            .expect("Permit should be available");

        // Simulate long async work
        println!("Doing work (sleeping for 5 seconds)...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        println!("Work done. Dropping permit.");
    }
    // Permit is dropped here. The 1-second cooldown starts NOW.

    println!("Trying to re-acquire immediately (should fail)...");
    match limiter.try_acquire_permit() {
        Ok(_) => println!("Unexpectedly acquired permit!"),
        Err(Error::NoPermitAvailable(maybe_next_time)) => {
            if let Some(next_time) = maybe_next_time {
                println!("Failed as expected. Next permit available at: {next_time}");

                let now = chrono::Utc::now();
                if next_time > now {
                    // Calculate wait time
                    let wait_duration = (next_time - now)
                        .to_std()
                        .expect("Duration should be positive");

                    // Add a tiny buffer (10ms) to ensure we don't wake up slightly too early due to precision issues
                    let sleep_duration = wait_duration + std::time::Duration::from_millis(10);

                    println!("Sleeping for {sleep_duration:?} based on error info...");
                    tokio::time::sleep(sleep_duration).await;
                }
            } else {
                // This happens if the rate limiter doesn't know when the next slot opens
                // (e.g., if all slots are currently held by active users).
                // In this case, we fall back to a default polling interval.
                println!(
                    "No next available time returned. Sleeping for default duration: {DEFAULT_SLEEP:?}"
                );
                tokio::time::sleep(DEFAULT_SLEEP).await;
            }
        }
        Err(e) => println!("Failed with unexpected error: {e}"),
    }

    println!("Trying to acquire again...");
    match limiter.try_acquire_permit() {
        Ok(_) => println!("Success! Permit acquired."),
        Err(e) => println!("Failed: {e}"),
    }
}
