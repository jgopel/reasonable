/// Errors that can occur when interacting with the [`UnfairRateLimiter`].
#[derive(Debug, thiserror::Error)]
pub enum Error<T> {
    /// The internal mutex was poisoned.
    #[error("Mutex poisoned")]
    MutexPoisoned(#[from] std::sync::PoisonError<T>),
    /// A permit cannot currently be acquired because the limit has been reached.
    ///
    /// Contains the time when the next permit might become available, if known.
    #[error("A permit cannot currently be acquired")]
    NoPermitAvailable(Option<chrono::DateTime<chrono::Utc>>),
}

/// A RAII permit for a single unit of concurrency.
///
/// When this value is dropped (returned), the permit is released, but the "slot" it occupied
/// remains unavailable for the configured `interval` of the rate limiter. This means the
/// cooldown period starts at the moment the permit is dropped, not when it was created.
#[derive(Debug)]
#[must_use]
pub struct SinglePermit<'a, const MAX_SIMULTANEOUS: usize> {
    parent_rate_limiter: &'a UnfairRateLimiter<MAX_SIMULTANEOUS>,
}

impl<'a, const MAX_SIMULTANEOUS: usize> SinglePermit<'a, MAX_SIMULTANEOUS> {
    fn new(
        parent_rate_limiter: &'a UnfairRateLimiter<MAX_SIMULTANEOUS>,
    ) -> Result<Self, Error<std::sync::MutexGuard<'a, State>>> {
        parent_rate_limiter.state.lock()?.active_connection_count += 1;
        Ok(Self {
            parent_rate_limiter,
        })
    }

    fn drop_impl(&mut self, at_time: chrono::NaiveDateTime) {
        tracing::trace!("Dropping permit at {at_time}");
        let mut state = self
            .parent_rate_limiter
            .state
            .lock()
            .expect("This should never fail");
        state.active_connection_count -= 1;
        state.expiry_times.push_back(at_time);
    }
}

impl<const MAX_SIMULTANEOUS: usize> Drop for SinglePermit<'_, MAX_SIMULTANEOUS> {
    fn drop(&mut self) {
        self.drop_impl(chrono::Utc::now().naive_utc());
    }
}

/// A RAII permit for multiple units of concurrency.
///
/// When this value is dropped (returned), the permits are released, but the "slots" they occupied
/// remain unavailable for the configured `interval` of the rate limiter. This means the
/// cooldown period starts at the moment the permits are dropped.
#[derive(Debug)]
#[must_use]
pub struct MultiPermit<'a, const MAX_SIMULTANEOUS: usize> {
    parent_rate_limiter: &'a UnfairRateLimiter<MAX_SIMULTANEOUS>,
    num_permits: usize,
}

impl<'a, const MAX_SIMULTANEOUS: usize> MultiPermit<'a, MAX_SIMULTANEOUS> {
    fn new(
        parent_rate_limiter: &'a UnfairRateLimiter<MAX_SIMULTANEOUS>,
        num_permits: usize,
    ) -> Result<Self, Error<std::sync::MutexGuard<'a, State>>> {
        parent_rate_limiter.state.lock()?.active_connection_count += num_permits;
        Ok(Self {
            parent_rate_limiter,
            num_permits,
        })
    }

    fn drop_impl(&mut self, at_time: chrono::NaiveDateTime) {
        tracing::trace!(
            "Dropping {num_permits} permits at {at_time}",
            num_permits = self.num_permits
        );
        let mut state = self
            .parent_rate_limiter
            .state
            .lock()
            .expect("This should never fail");

        state.active_connection_count -= self.num_permits;
        for _ in 0..self.num_permits {
            state.expiry_times.push_back(at_time);
        }
    }
}

impl<const MAX_SIMULTANEOUS: usize> Drop for MultiPermit<'_, MAX_SIMULTANEOUS> {
    fn drop(&mut self) {
        self.drop_impl(chrono::Utc::now().naive_utc());
    }
}

type ExpiryTimes = std::collections::VecDeque<chrono::NaiveDateTime>;

/// Internal state of the rate limiter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    active_connection_count: usize,
    expiry_times: ExpiryTimes,
}

/// A rate limiter that enforces a cooldown period after usage (return-time based).
///
/// `MAX_SIMULTANEOUS` defines the maximum number of "slots" available.
/// A slot is occupied if a permit is currently held, OR if a permit was recently
/// held and the cooldown `interval` has not yet passed since it was dropped.
///
/// This implies that long-running tasks holding a permit will delay the availability
/// of that slot for future tasks until `duration_held + interval` time has passed.
#[derive(Debug)]
pub struct UnfairRateLimiter<const MAX_SIMULTANEOUS: usize> {
    interval: chrono::Duration,
    state: std::sync::Mutex<State>,
}

impl<const MAX_SIMULTANEOUS: usize> UnfairRateLimiter<MAX_SIMULTANEOUS> {
    /// Creates a new rate limiter with the specified cooldown `interval`.
    ///
    /// The `interval` specifies how long a slot remains unavailable *after* a permit is dropped.
    /// Initially, all permits are available.
    #[must_use]
    pub fn new(interval: chrono::Duration) -> Self {
        Self {
            interval,
            state: std::sync::Mutex::new(State::default()),
        }
    }

    fn new_exhausted_impl(interval: chrono::Duration, start_time: chrono::NaiveDateTime) -> Self {
        Self {
            interval,
            state: std::sync::Mutex::new(State {
                expiry_times: std::collections::VecDeque::from([start_time; MAX_SIMULTANEOUS]),
                ..Default::default()
            }),
        }
    }

    /// Creates a new rate limiter that is initially exhausted.
    ///
    /// This simulates a state where all permits have just been used and dropped
    /// at the current time, so no new permits can be acquired until the `interval` cooldown has passed.
    #[must_use]
    pub fn new_exhausted(interval: chrono::Duration) -> Self {
        let start_time = chrono::Utc::now().naive_utc();
        Self::new_exhausted_impl(interval, start_time)
    }

    fn remove_old_expiries(
        expiry_times: &mut ExpiryTimes,
        for_time: &chrono::NaiveDateTime,
        interval: &chrono::Duration,
    ) -> usize {
        let partition_point = expiry_times.partition_point(|time| *time < (*for_time - *interval));
        for _ in 0..partition_point {
            let _ = expiry_times.pop_front();
        }
        partition_point
    }

    fn try_acquire_permit_impl(
        &self,
        for_time: &chrono::NaiveDateTime,
    ) -> Result<SinglePermit<'_, MAX_SIMULTANEOUS>, Error<std::sync::MutexGuard<'_, State>>> {
        tracing::debug!("Trying to acquire permit for {for_time}");
        let mut state = self.state.lock()?;
        tracing::trace!("{for_time} - lock acquired");

        debug_assert!(state.active_connection_count <= MAX_SIMULTANEOUS);
        let next_available_time = state
            .expiry_times
            .front()
            .map(|time| time.and_utc() + self.interval);
        if state.active_connection_count == MAX_SIMULTANEOUS {
            tracing::trace!("{for_time} - No permit available, all connections in use");
            return Err(Error::NoPermitAvailable(next_available_time));
        }

        Self::remove_old_expiries(&mut state.expiry_times, for_time, &self.interval);

        debug_assert!(state.expiry_times.len() <= MAX_SIMULTANEOUS);
        if state.expiry_times.len() == MAX_SIMULTANEOUS {
            tracing::trace!("{for_time} - No permit available, at rate limit");
            return Err(Error::NoPermitAvailable(next_available_time));
        }

        debug_assert!(state.expiry_times.len() + state.active_connection_count <= MAX_SIMULTANEOUS);
        if state.active_connection_count + state.expiry_times.len() == MAX_SIMULTANEOUS {
            tracing::trace!("{for_time} - No permit available, rate limit reached");
            return Err(Error::NoPermitAvailable(next_available_time));
        }
        drop(state);

        SinglePermit::new(self)
    }

    fn try_acquire_permits_impl(
        &self,
        for_time: &chrono::NaiveDateTime,
        num_permits: usize,
    ) -> Result<MultiPermit<'_, MAX_SIMULTANEOUS>, Error<std::sync::MutexGuard<'_, State>>> {
        debug_assert!(num_permits > 0);
        debug_assert!(num_permits <= MAX_SIMULTANEOUS);

        tracing::debug!("Trying to acquire {num_permits} permits for {for_time}");
        let mut state = self.state.lock()?;
        tracing::trace!("{for_time} - lock acquired");

        Self::remove_old_expiries(&mut state.expiry_times, for_time, &self.interval);

        debug_assert!(state.active_connection_count <= MAX_SIMULTANEOUS);

        if state.active_connection_count + num_permits > MAX_SIMULTANEOUS {
            tracing::trace!(
                concat!(
                    "{for_time} - Not enough permits available, {num_conn} ",
                    "connections in use ({num_permits} requested)",
                ),
                for_time = for_time,
                num_conn = state.active_connection_count,
                num_permits = num_permits,
            );
            return Err(Error::NoPermitAvailable(None));
        }

        let num_expired = state.expiry_times.len();
        if state.active_connection_count + num_expired + num_permits > MAX_SIMULTANEOUS {
            let available = MAX_SIMULTANEOUS - state.active_connection_count - num_expired;
            tracing::trace!(
                "{for_time} - Not enough permits available. {num_permits} requested, {available} available"
            );
            let next_time = state
                .expiry_times
                .get(num_permits - 1)
                .or(state.expiry_times.back())
                .map(|t| t.and_utc() + self.interval);

            return Err(Error::NoPermitAvailable(next_time));
        }

        drop(state);

        MultiPermit::new(self, num_permits)
    }
}

impl<const MAX_SIMULTANEOUS: usize> super::RateLimiter for UnfairRateLimiter<MAX_SIMULTANEOUS> {
    type SinglePermit<'a> = SinglePermit<'a, MAX_SIMULTANEOUS>;
    type MultiPermit<'a> = MultiPermit<'a, MAX_SIMULTANEOUS>;
    type Error<'a> = Error<std::sync::MutexGuard<'a, State>>;

    /// Attempts to acquire a single permit.
    ///
    /// Returns a [`SinglePermit`] if a slot is available.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoPermitAvailable`] if all slots are occupied (either by active permits or by cooldowns from recently dropped permits).
    /// Returns [`Error::MutexPoisoned`] if the internal state mutex is poisoned.
    fn try_acquire_permit(&self) -> Result<Self::SinglePermit<'_>, Self::Error<'_>> {
        self.try_acquire_permit_impl(&chrono::Utc::now().naive_utc())
    }

    /// Attempts to acquire multiple permits at once.
    ///
    /// Returns a [`MultiPermit`] if enough slots are available.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoPermitAvailable`] if there are insufficient slots.
    /// Returns [`Error::MutexPoisoned`] if the internal state mutex is poisoned.
    fn try_acquire_permits(
        &self,
        num_permits: usize,
    ) -> Result<Self::MultiPermit<'_>, Self::Error<'_>> {
        self.try_acquire_permits_impl(&chrono::Utc::now().naive_utc(), num_permits)
    }
}

#[cfg(feature = "tokio")]
impl<const MAX_SIMULTANEOUS: usize> UnfairRateLimiter<MAX_SIMULTANEOUS> {
    async fn retry_until_acquired<'a, TReturn>(
        &self,
        mut acquire_fn: impl FnMut() -> Result<TReturn, Error<std::sync::MutexGuard<'a, State>>>,
    ) -> TReturn {
        loop {
            let result = acquire_fn();
            let next_time = match result {
                Ok(permit) => return permit,
                Err(Error::NoPermitAvailable(next_time)) => next_time,
                Err(Error::MutexPoisoned(_)) => panic!("Internal mutex is poisoned"),
            };
            let wait_time =
                next_time.map_or(self.interval, |wake_time| wake_time - chrono::Utc::now());
            tokio::time::sleep(
                wait_time
                    .to_std()
                    // The wake time was in the past when the now calculation
                    // was made, so just re-wake immediately
                    .unwrap_or(std::time::Duration::from_secs(0)),
            )
            .await;
        }
    }
}

#[cfg(feature = "tokio")]
impl<const MAX_SIMULTANEOUS: usize> super::AsyncRateLimiter
    for UnfairRateLimiter<MAX_SIMULTANEOUS>
{
    /// Asynchronously acquires a single permit.
    ///
    /// Waits until a slot is available if the rate limit has been reached.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    fn acquire_permit(&self) -> impl Future<Output = Self::SinglePermit<'_>> {
        use super::RateLimiter;

        self.retry_until_acquired(move || self.try_acquire_permit())
    }

    /// Asynchronously acquires multiple permits.
    ///
    /// Waits until enough slots are available if the rate limit has been reached.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    fn acquire_permits(&self, num_permits: usize) -> impl Future<Output = Self::MultiPermit<'_>> {
        use super::RateLimiter;

        self.retry_until_acquired(move || self.try_acquire_permits(num_permits))
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn dt_from_str(str: &str) -> chrono::NaiveDateTime {
        chrono::DateTime::parse_from_rfc3339(str)
            .unwrap()
            .naive_utc()
    }

    #[test]
    fn test_can_acquire_permit_immediately_after_normal_construction() {
        let rate_limiter = UnfairRateLimiter::<42>::new(chrono::Duration::seconds(43));

        let _permit = rate_limiter
            .try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"))
            .unwrap();
    }

    #[test]
    fn test_cannot_acquire_permit_immediately_after_exhausted_construction() {
        let start_time = dt_from_str("2022-01-02 03:04:05Z");
        let interval = chrono::Duration::seconds(43);
        let rate_limiter = UnfairRateLimiter::<42>::new_exhausted_impl(interval, start_time);

        let result = rate_limiter.try_acquire_permit_impl(&start_time);

        let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
            panic!("Expected NoPermitAvailable error");
        };
        assert_eq!(
            next_permit_time,
            Some(dt_from_str("2022-01-02 03:04:48Z").and_utc())
        );
    }

    mod single_permit {
        use pretty_assertions::assert_eq;

        use super::*;

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

            let _permit = rate_limiter
                .try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"))
                .unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 1,
                    expiry_times: std::collections::VecDeque::default()
                }
            );
        }

        #[test]
        fn test_can_acquire_permit_when_exactly_1_connection_is_available() {
            const CONNECTION_COUNT: usize = 10;
            let initial_state = State {
                active_connection_count: CONNECTION_COUNT - 1,
                expiry_times: std::collections::VecDeque::default(),
            };
            let rate_limiter = UnfairRateLimiter::<CONNECTION_COUNT> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };

            let _permit = rate_limiter
                .try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"))
                .unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: CONNECTION_COUNT,
                    expiry_times: std::collections::VecDeque::default()
                }
            );
        }

        #[test]
        fn test_can_acquire_permit_when_expiry_times_is_nearly_full() {
            let current_time = dt_from_str("2022-01-02 03:04:05Z");
            let initial_expiry_times = std::collections::VecDeque::from([
                current_time,
                current_time,
                current_time,
                current_time,
            ]);
            let initial_state = State {
                active_connection_count: 0,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<5> {
                interval: chrono::Duration::seconds(5),
                state: std::sync::Mutex::new(initial_state),
            };

            let _permit = rate_limiter.try_acquire_permit_impl(&current_time).unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 1,
                    expiry_times: initial_expiry_times
                }
            );
        }

        #[test]
        fn test_can_acquire_permit_from_full_expiries_after_interval_has_passed() {
            let initial_expiry_times = std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:03:59Z"),
                dt_from_str("2022-01-02 03:04:00Z"),
                dt_from_str("2022-01-02 03:04:01Z"),
                dt_from_str("2022-01-02 03:04:02Z"),
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]);
            let initial_state = State {
                active_connection_count: 0,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<6> {
                interval: chrono::Duration::seconds(5),
                state: std::sync::Mutex::new(initial_state),
            };

            let _permit = rate_limiter
                .try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"))
                .unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 1,
                    expiry_times: initial_expiry_times.into_iter().skip(1).collect()
                }
            );
        }

        #[test]
        fn test_cannot_acquire_permit_when_all_connections_are_active() {
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

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };
            assert_eq!(next_permit_time, None);

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: CONNECTION_COUNT,
                    expiry_times: std::collections::VecDeque::default()
                }
            );
        }

        #[test]
        fn test_cannot_acquire_permit_when_previous_permits_are_not_expired() {
            let initial_expiry_times = std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]);
            let initial_state = State {
                active_connection_count: 0,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<2> {
                interval: chrono::Duration::seconds(5),
                state: std::sync::Mutex::new(initial_state),
            };

            let result = rate_limiter.try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"));

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };
            assert_eq!(
                next_permit_time,
                Some(dt_from_str("2022-01-02 03:04:08Z").and_utc())
            );

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 0,
                    expiry_times: initial_expiry_times
                }
            );
        }

        #[test]
        fn test_cannot_acquire_permit_when_all_expiry_times_are_exactly_at_current_time() {
            let current_time = dt_from_str("2022-01-02 03:04:05Z");
            let initial_expiry_times = std::collections::VecDeque::from([
                current_time,
                current_time,
                current_time,
                current_time,
                current_time,
            ]);
            let initial_state = State {
                active_connection_count: 0,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<5> {
                interval: chrono::Duration::seconds(5),
                state: std::sync::Mutex::new(initial_state),
            };

            let result = rate_limiter.try_acquire_permit_impl(&current_time);

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };
            assert_eq!(
                next_permit_time,
                Some(dt_from_str("2022-01-02 03:04:10Z").and_utc())
            );

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 0,
                    expiry_times: initial_expiry_times
                }
            );
        }

        #[test]
        fn test_cannot_acquire_permit_when_all_expiry_times_are_about_to_be_retired() {
            let current_time = dt_from_str("2022-01-02 03:04:05Z");
            let initial_expiry_times = std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:04:00Z"),
                dt_from_str("2022-01-02 03:04:00Z"),
                dt_from_str("2022-01-02 03:04:00Z"),
                dt_from_str("2022-01-02 03:04:00Z"),
                dt_from_str("2022-01-02 03:04:00Z"),
            ]);
            let initial_state = State {
                active_connection_count: 0,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<5> {
                interval: chrono::Duration::seconds(5),
                state: std::sync::Mutex::new(initial_state),
            };

            let result = rate_limiter.try_acquire_permit_impl(&current_time);

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };
            assert_eq!(
                next_permit_time,
                Some(dt_from_str("2022-01-02 03:04:05Z").and_utc())
            );

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 0,
                    expiry_times: initial_expiry_times
                }
            );
        }

        #[test]
        fn test_cannot_acquire_permit_when_sum_of_active_connections_and_expired_connections_equals_max()
         {
            let initial_expiry_times = std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]);
            let initial_state = State {
                active_connection_count: 8,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<10> {
                interval: chrono::Duration::seconds(5),
                state: std::sync::Mutex::new(initial_state),
            };

            let result = rate_limiter.try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"));

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };
            assert_eq!(
                next_permit_time,
                Some(dt_from_str("2022-01-02 03:04:08Z").and_utc())
            );

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 8,
                    expiry_times: initial_expiry_times
                }
            );
        }

        #[test]
        fn test_dropping_permit_updates_the_state() {
            let initial_state = State {
                active_connection_count: 1,
                expiry_times: std::collections::VecDeque::default(),
            };
            let rate_limiter = UnfairRateLimiter::<42> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };

            let mut permit = rate_limiter
                .try_acquire_permit_impl(&dt_from_str("2022-01-02 03:04:05Z"))
                .unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 2,
                    expiry_times: std::collections::VecDeque::default()
                }
            );

            permit.drop_impl(dt_from_str("2022-01-02 03:04:06Z"));

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 1,
                    expiry_times: std::collections::VecDeque::from([dt_from_str(
                        "2022-01-02 03:04:06Z"
                    )])
                }
            );
        }
    }

    mod multi_permit {
        use pretty_assertions::assert_eq;

        use super::*;

        #[test]
        fn test_can_acquire_5_permits_when_all_connections_are_available() {
            let initial_state = State {
                active_connection_count: 0,
                expiry_times: std::collections::VecDeque::default(),
            };
            let rate_limiter = UnfairRateLimiter::<42> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };

            let _permit = rate_limiter
                .try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), 5)
                .unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 5,
                    expiry_times: std::collections::VecDeque::default()
                }
            );
        }

        #[test]
        fn test_can_acquire_5_permits_when_exactly_5_connections_are_available() {
            const CAPACITY: usize = 42;
            const REQUESTED: usize = 5;

            let initial_state = State {
                active_connection_count: CAPACITY - REQUESTED,
                expiry_times: std::collections::VecDeque::default(),
            };
            let rate_limiter = UnfairRateLimiter::<CAPACITY> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };

            let _permit = rate_limiter
                .try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), REQUESTED)
                .unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: CAPACITY,
                    expiry_times: std::collections::VecDeque::default()
                }
            );
        }

        #[test]
        fn test_can_acquire_5_permits_when_all_expiries_slots_are_empty() {
            let initial_state = State {
                active_connection_count: 0,
                expiry_times: std::collections::VecDeque::default(),
            };
            let rate_limiter = UnfairRateLimiter::<42> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };

            let _permit = rate_limiter
                .try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), 5)
                .unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 5,
                    expiry_times: std::collections::VecDeque::default()
                }
            );
        }

        #[test]
        fn test_can_acquire_5_permits_when_exactly_5_expiry_slots_are_available() {
            const CAPACITY: usize = 10;
            const REQUESTED: usize = 5;

            let initial_expiry_times = std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:04:00Z"),
                dt_from_str("2022-01-02 03:04:01Z"),
                dt_from_str("2022-01-02 03:04:02Z"),
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]);

            let initial_state = State {
                active_connection_count: 0,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<CAPACITY> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };

            let _permit = rate_limiter
                .try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), REQUESTED)
                .unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: REQUESTED,
                    expiry_times: initial_expiry_times
                }
            );
        }

        #[test]
        fn test_can_acquire_max_permits() {
            const CAPACITY: usize = 10;

            let initial_state = State {
                active_connection_count: 0,
                expiry_times: std::collections::VecDeque::default(),
            };
            let rate_limiter = UnfairRateLimiter::<CAPACITY> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };

            let _permit = rate_limiter
                .try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), CAPACITY)
                .unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: CAPACITY,
                    expiry_times: std::collections::VecDeque::default(),
                }
            );
        }

        #[test]
        fn test_cannot_acquire_5_permits_when_0_connections_are_available() {
            const CAPACITY: usize = 42;

            let initial_state = State {
                active_connection_count: CAPACITY,
                expiry_times: std::collections::VecDeque::default(),
            };
            let rate_limiter = UnfairRateLimiter::<CAPACITY> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };

            let result =
                rate_limiter.try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), 5);

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };
            assert_eq!(next_permit_time, None);

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: CAPACITY,
                    expiry_times: std::collections::VecDeque::default()
                }
            );
        }

        #[test]
        fn test_cannot_acquire_5_permits_when_1_connection_is_available() {
            const CAPACITY: usize = 42;

            let initial_state = State {
                active_connection_count: CAPACITY - 1,
                expiry_times: std::collections::VecDeque::default(),
            };
            let rate_limiter = UnfairRateLimiter::<CAPACITY> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };

            let result =
                rate_limiter.try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), 5);

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };
            assert_eq!(next_permit_time, None);

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: CAPACITY - 1,
                    expiry_times: std::collections::VecDeque::default()
                }
            );
        }

        #[test]
        fn test_cannot_acquire_5_permits_when_4_connections_are_available() {
            const CAPACITY: usize = 42;

            let initial_state = State {
                active_connection_count: CAPACITY - 4,
                expiry_times: std::collections::VecDeque::default(),
            };
            let rate_limiter = UnfairRateLimiter::<CAPACITY> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };

            let result =
                rate_limiter.try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), 5);

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };
            assert_eq!(next_permit_time, None);

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: CAPACITY - 4,
                    expiry_times: std::collections::VecDeque::default()
                }
            );
        }

        #[test]
        fn test_cannot_acquire_5_permits_when_0_expiry_slots_are_available() {
            const CAPACITY: usize = 10;

            let initial_expiry_times = std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:03:55Z"),
                dt_from_str("2022-01-02 03:03:56Z"),
                dt_from_str("2022-01-02 03:03:57Z"),
                dt_from_str("2022-01-02 03:03:58Z"),
                dt_from_str("2022-01-02 03:03:59Z"),
                dt_from_str("2022-01-02 03:04:00Z"),
                dt_from_str("2022-01-02 03:04:01Z"),
                dt_from_str("2022-01-02 03:04:02Z"),
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]);

            let initial_state = State {
                active_connection_count: 0,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<CAPACITY> {
                interval: chrono::Duration::seconds(15),
                state: std::sync::Mutex::new(initial_state),
            };

            let result =
                rate_limiter.try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), 5);

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };
            assert_eq!(
                next_permit_time,
                Some(dt_from_str("2022-01-02 03:04:14Z").and_utc())
            );

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 0,
                    expiry_times: initial_expiry_times
                }
            );
        }

        #[test]
        fn test_cannot_acquire_5_permits_when_1_expiry_slot_is_available() {
            const CAPACITY: usize = 10;

            let initial_expiry_times = std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:03:56Z"),
                dt_from_str("2022-01-02 03:03:57Z"),
                dt_from_str("2022-01-02 03:03:58Z"),
                dt_from_str("2022-01-02 03:03:59Z"),
                dt_from_str("2022-01-02 03:04:00Z"),
                dt_from_str("2022-01-02 03:04:01Z"),
                dt_from_str("2022-01-02 03:04:02Z"),
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]);

            let initial_state = State {
                active_connection_count: 0,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<CAPACITY> {
                interval: chrono::Duration::seconds(15),
                state: std::sync::Mutex::new(initial_state),
            };

            let result =
                rate_limiter.try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), 5);

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };

            assert_eq!(
                next_permit_time,
                Some(dt_from_str("2022-01-02 03:04:15Z").and_utc())
            );

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 0,
                    expiry_times: initial_expiry_times
                }
            );
        }

        #[test]
        fn test_cannot_acquire_5_permits_when_4_expiry_slots_are_available() {
            const CAPACITY: usize = 10;

            let initial_expiry_times = std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:03:59Z"),
                dt_from_str("2022-01-02 03:04:00Z"),
                dt_from_str("2022-01-02 03:04:01Z"),
                dt_from_str("2022-01-02 03:04:02Z"),
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]);

            let initial_state = State {
                active_connection_count: 0,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<CAPACITY> {
                interval: chrono::Duration::seconds(15),
                state: std::sync::Mutex::new(initial_state),
            };

            let result =
                rate_limiter.try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), 5);

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };

            assert_eq!(
                next_permit_time,
                Some(dt_from_str("2022-01-02 03:04:18Z").and_utc())
            );

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 0,
                    expiry_times: initial_expiry_times
                }
            );
        }

        #[test]
        fn test_cannot_acquire_5_permits_when_sum_of_active_connections_and_expired_connections_is_too_close_to_max()
         {
            const CAPACITY: usize = 10;

            let initial_expiry_times = std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:04:02Z"),
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]);
            let initial_state = State {
                active_connection_count: 5,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<CAPACITY> {
                interval: chrono::Duration::seconds(15),
                state: std::sync::Mutex::new(initial_state),
            };

            let result =
                rate_limiter.try_acquire_permits_impl(&dt_from_str("2022-01-02 03:04:05Z"), 5);

            let Err(Error::NoPermitAvailable(next_permit_time)) = result else {
                panic!("Expected NoPermitAvailable error");
            };

            assert_eq!(
                next_permit_time,
                Some(dt_from_str("2022-01-02 03:04:19Z").and_utc())
            );

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 5,
                    expiry_times: initial_expiry_times
                }
            );
        }

        #[test]
        fn test_dropping_permits_updates_the_state() {
            let initial_expiry_times = std::collections::VecDeque::from([
                dt_from_str("2022-01-02 03:04:02Z"),
                dt_from_str("2022-01-02 03:04:03Z"),
                dt_from_str("2022-01-02 03:04:04Z"),
            ]);
            let initial_state = State {
                active_connection_count: 5,
                expiry_times: initial_expiry_times.clone(),
            };
            let rate_limiter = UnfairRateLimiter::<42> {
                interval: chrono::Duration::seconds(43),
                state: std::sync::Mutex::new(initial_state),
            };
            let mut permit = rate_limiter
                .try_acquire_permits_impl(&dt_from_str("2022-01-01 12:00:00Z"), 3)
                .unwrap();

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 8,
                    expiry_times: initial_expiry_times,
                }
            );

            permit.drop_impl(dt_from_str("2022-01-01 12:00:05Z"));

            assert_eq!(
                *rate_limiter.state.lock().unwrap(),
                State {
                    active_connection_count: 5,
                    expiry_times: std::collections::VecDeque::from([
                        dt_from_str("2022-01-02 03:04:02Z"),
                        dt_from_str("2022-01-02 03:04:03Z"),
                        dt_from_str("2022-01-02 03:04:04Z"),
                        dt_from_str("2022-01-01 12:00:05Z"),
                        dt_from_str("2022-01-01 12:00:05Z"),
                        dt_from_str("2022-01-01 12:00:05Z"),
                    ])
                }
            );
        }
    }

    #[cfg(feature = "tokio")]
    mod tokio_tests {
        use crate::AsyncRateLimiter;
        use crate::RateLimiter;

        use super::*;

        #[tokio::test]
        async fn test_permit_already_available() {
            let interval = chrono::Duration::milliseconds(100);
            let limiter = UnfairRateLimiter::<1>::new(interval);

            let start = chrono::Utc::now();
            let _permit = limiter.acquire_permit().await;
            let elapsed = chrono::Utc::now() - start;

            assert!(elapsed < interval);
        }

        #[tokio::test]
        async fn test_single_permit_cooldown() {
            let interval = chrono::Duration::milliseconds(100);
            let limiter = UnfairRateLimiter::<1>::new(interval);

            let previous_permit = limiter.try_acquire_permit().unwrap();
            drop(previous_permit);

            let start = chrono::Utc::now();
            let _permit = limiter.acquire_permit().await;
            let elapsed = chrono::Utc::now() - start;

            let cutoff = chrono::Duration::milliseconds(500);
            assert!(interval <= elapsed && elapsed < cutoff);
        }

        #[tokio::test]
        async fn test_multi_permit_cooldown() {
            let interval = chrono::Duration::milliseconds(100);
            let limiter = UnfairRateLimiter::<3>::new(interval);

            let previous_permit = limiter.try_acquire_permits(3).unwrap();
            drop(previous_permit);

            let start = chrono::Utc::now();
            let _permit = limiter.acquire_permits(3).await;
            let elapsed = chrono::Utc::now() - start;

            let cutoff = chrono::Duration::milliseconds(500);
            assert!(interval <= elapsed && elapsed < cutoff);
        }
    }
}
