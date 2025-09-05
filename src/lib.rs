pub struct AcquireError(std::time::Duration);
pub struct Permit;

struct AtomicData {
    active_connection_count: usize,
    // TODO: Ring buffer
    expiry_times: std::collections::VecDeque<std::time::Instant>,
}

pub struct UnfairRateLimiter<const MAX_SIMULTANEOUS: usize> {
    interval: std::time::Duration,

    atomic_data: std::sync::Mutex<AtomicData>,
}

impl<const MAX_SIMULTANEOUS: usize> UnfairRateLimiter<MAX_SIMULTANEOUS> {
    // TODO: Start empty vs start full
    pub fn new(interval: std::time::Duration) -> Self {
        Self {
            interval,
            semaphore: tokio::sync::Semaphore::new(MAX_SIMULTANEOUS),
            expiry_times: std::collections::VecDeque::default(),
        }
    }

    pub fn try_acquire_permit(&mut self) -> Result<Permit, AcquireError> {
        // TODO: just lock()?
        let Ok(atomic_data) = self.atomic_data.try_lock() else {
            return Err(AcquireError(std::time::Duration::from_secs(0)));
        };

        let least_recent_expiry_time_opt = atomic_data.expiry_times.front();
        // TODO

        if atomic_data.active_connection_count == (MAX_SIMULTANEOUS - 1) {
            return Err(AcquireError(std::time::Duration::from_secs(0)));
        }


        // if there's a connection available on the semaphore
        // AND
        // if the expiry list is not full
        // OR
        // the least recent item on the list is expired
    }

    pub async fn acquire_permit(&mut self) -> Permit {
        loop {
            match self.try_acquire_permit() {
                Ok(permit) => return permit,
                Err(AcquireError(wait_time)) => tokio::time::sleep(wait_time).await,
            }
        }
    }
}

// async fn place_orders(orders: Vec<Order>) {
//     let _permit = self.rate_limiter.acquire_permits(orders.len()).await;
//     for order in orders {
//         actually_make_api_call(order);
//     }
// }
//
// pub fn add(left: u64, right: u64) -> u64 {
//     left + right
// }
//
// #[cfg(test)]
// mod tests {
//     use super::*;
//
//     #[test]
//     fn it_works() {
//         let result = add(2, 2);
//         assert_eq!(result, 4);
//     }
// }
