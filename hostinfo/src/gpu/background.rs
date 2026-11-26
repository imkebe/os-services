//! A value that a slow tool gives, refreshed on a thread of its own: the
//! sample that asks takes the latest value and never waits for the tool.

use std::{
    sync::{Mutex, PoisonError},
    thread,
    time::{Duration, Instant},
};

pub struct Background<T> {
    state: Mutex<State<T>>,
}

struct State<T> {
    value: Option<T>,
    /// When the last refresh started.
    started: Option<Instant>,
    running: bool,
}

impl<T: Clone + Send + 'static> Background<T> {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(State {
                value: None,
                started: None,
                running: false,
            }),
        }
    }

    /// The latest value, None before the first refresh ends. Starts a
    /// refresh when none runs and the last started `every` ago or longer.
    pub fn get(&'static self, every: Duration, refresh: fn() -> Option<T>) -> Option<T> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let due = state
            .started
            .is_none_or(|started| started.elapsed() >= every);
        if due && !state.running {
            state.running = true;
            state.started = Some(Instant::now());
            let spawned = thread::Builder::new()
                .name("hostinfo-gpu".into())
                .spawn(move || {
                    let value = refresh();
                    let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                    state.value = value;
                    state.running = false;
                });
            state.running = spawned.is_ok();
        }
        state.value.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshes_on_its_own_thread_and_not_more_often_than_asked() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static RUNS: AtomicU32 = AtomicU32::new(0);
        static VALUE: Background<u32> = Background::new();
        fn refresh() -> Option<u32> {
            Some(RUNS.fetch_add(1, Ordering::SeqCst) + 1)
        }
        let every = Duration::from_secs(60);
        assert_eq!(VALUE.get(every, refresh), None);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut value = None;
        while value.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
            value = VALUE.get(every, refresh);
        }
        assert_eq!(value, Some(1));
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);
    }
}
