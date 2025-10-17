// TODO: Add tracing

#[derive(Debug, thiserror::Error)]
pub enum Error<T> {
    #[error("Mutex posioned")]
    MutexPosioned(#[from] std::sync::PoisonError<T>),
    #[error("A permit cannot currently be acquired")]
    NoPermitAvailable,
}

pub struct Permit {}

type ExpiryTimes = std::collections::VecDeque<chrono::NaiveDateTime>;
#[derive(Debug, Clone, Default)]
pub struct State {
    active_connection_count: usize,
    // TODO: Ring buffer
    expiry_times: ExpiryTimes,
}

pub struct UnfairRateLimiter<const MAX_SIMULTANEOUS: usize> {
    interval: chrono::Duration,
    state: std::sync::Mutex<State>,
}

impl<const MAX_SIMULTANEOUS: usize> UnfairRateLimiter<MAX_SIMULTANEOUS> {
    pub fn new(interval: chrono::Duration) -> Self {
        Self {
            interval,
            state: std::sync::Mutex::new(State::default()),
        }
    }
    // TODO: new_max()? - for cases where you want to start assuming previous saturation

    fn remove_old_expiries(
        expiry_times: &mut ExpiryTimes,
        for_time: &chrono::NaiveDateTime,
        interval: &chrono::Duration,
    ) {
        let partition_point = expiry_times.partition_point(|time| *time < (*for_time - *interval));
        for _ in 0..partition_point {
            let _ = expiry_times.pop_front();
        }
        // TODO: Should this return something?
    }

    fn try_acquire_permit_impl(
        &self,
        for_time: &chrono::NaiveDateTime,
    ) -> Result<Permit, Error<std::sync::MutexGuard<'_, State>>> {
        let mut state = self.state.lock()?;

        debug_assert!(state.active_connection_count <= MAX_SIMULTANEOUS);
        if state.active_connection_count == MAX_SIMULTANEOUS {
            return Err(Error::NoPermitAvailable);
        }

        Self::remove_old_expiries(&mut state.expiry_times, for_time, &self.interval);

        debug_assert!(state.expiry_times.len() <= MAX_SIMULTANEOUS);
        if state.expiry_times.len() == MAX_SIMULTANEOUS {
            return Err(Error::NoPermitAvailable);
        }

        debug_assert!(state.expiry_times.len() + state.active_connection_count <= MAX_SIMULTANEOUS);
        if state.active_connection_count + state.expiry_times.len() == MAX_SIMULTANEOUS {
            return Err(Error::NoPermitAvailable);
        }

        Ok(Permit {})
    }

    pub fn try_acquire_permit(&self) -> Result<Permit, Error<std::sync::MutexGuard<'_, State>>> {
        self.try_acquire_permit_impl(&chrono::Utc::now().naive_utc())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt_from_str(str: &str) -> chrono::NaiveDateTime {
        chrono::DateTime::parse_from_rfc3339(str)
            .unwrap()
            .naive_utc()
    }

    #[test]
    fn test_can_acquire_permit_from_empty_rate_limiter() {
        let initial_state = State {
            active_connection_count: 0,
            expiry_times: std::collections::VecDeque::default(),
        };
        let rate_limiter = UnfairRateLimiter::<42> {
            interval: chrono::Duration::seconds(43),
            state: std::sync::Mutex::new(initial_state),
        };

        let result = rate_limiter.try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"));

        assert!(result.is_ok());
        // TODO: Do we need to assert anything about the permit?
    }

    #[test]
    fn test_cannot_acquire_permit_from_rate_limiter_with_max_active_connections() {
        const CONNECTION_COUNT: usize = 10;
        let initial_state = State {
            active_connection_count: CONNECTION_COUNT,
            expiry_times: std::collections::VecDeque::default(),
        };
        let rate_limiter = UnfairRateLimiter::<CONNECTION_COUNT> {
            interval: chrono::Duration::seconds(43),
            state: std::sync::Mutex::new(initial_state),
        };

        let result = rate_limiter.try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"));

        assert!(matches!(result, Err(Error::NoPermitAvailable)));
    }

    #[test]
    fn test_cannot_acquire_permit_when_previous_permits_are_not_expired() {
        let initial_state = State {
            active_connection_count: 0,
            expiry_times: std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]),
        };
        let rate_limiter = UnfairRateLimiter::<2> {
            interval: chrono::Duration::seconds(5),
            state: std::sync::Mutex::new(initial_state),
        };

        let result = rate_limiter.try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"));

        assert!(matches!(result, Err(Error::NoPermitAvailable)));
    }

    #[test]
    fn test_cannot_acquire_permit_when_sum_of_active_connections_and_expired_connections_equals_max()
     {
        let initial_state = State {
            active_connection_count: 8,
            expiry_times: std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]),
        };
        let rate_limiter = UnfairRateLimiter::<10> {
            interval: chrono::Duration::seconds(5),
            state: std::sync::Mutex::new(initial_state),
        };

        let result = rate_limiter.try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"));

        assert!(matches!(result, Err(Error::NoPermitAvailable)));
    }

    #[test]
    fn test_can_acquire_permit_from_full_expiries_after_interval_has_passed() {
        let initial_state = State {
            active_connection_count: 0,
            expiry_times: std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:03:59Z"),
                dt_from_str("2022-01-02 03:04:00Z"),
                dt_from_str("2022-01-02 03:04:01Z"),
                dt_from_str("2022-01-02 03:04:02Z"),
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]),
        };
        let rate_limiter = UnfairRateLimiter::<6> {
            interval: chrono::Duration::seconds(5),
            state: std::sync::Mutex::new(initial_state),
        };

        let result = rate_limiter.try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"));

        assert!(result.is_ok());
    }

    // TODO: Test that dropping a permit adds it to the state correctly
    // TODO: Test all times in expiry_times exactly at current time
    // TODO: Test all times in expiry_times maximally far from current time
}
