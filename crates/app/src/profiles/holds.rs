//! Which profiles' tokens running jobs hold: a job that sends a profile's token (a publish, an
//! update, a share, a job of the Tours page) takes a [`Hold`] on that profile before it reads
//! the token and keeps it until it ends, whichever way it ends (done, failed, cancelled,
//! panicked: the hold goes when it is dropped). A token a removal or a "Sign in again" let go
//! of is ended at its site only once no job holds its profile any more ([`Holds::wait_free`]),
//! so a job that is under way is never cut off half-way.
//!
//! Only profile ids are kept here, never a token or anything made from one.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

#[derive(Default)]
struct Inner {
    held: Mutex<HashMap<String, usize>>,
    freed: Condvar,
}

/// A register of holds. The app has one ([`global`]); tests make their own.
#[derive(Clone, Default)]
pub struct Holds(Arc<Inner>);

impl Holds {
    fn held(&self) -> MutexGuard<'_, HashMap<String, usize>> {
        self.0.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A hold on profile `id`'s token, for as long as the returned guard lives.
    pub fn hold(&self, id: &str) -> Hold {
        *self.held().entry(id.to_string()).or_insert(0) += 1;
        Hold { holds: self.clone(), id: id.to_string() }
    }

    /// Whether any job holds any profile's token now: a publish, an update, a share or a job of
    /// the Tours page is under way. An app update waits for it.
    pub fn any_held(&self) -> bool {
        self.held().values().any(|n| *n > 0)
    }

    #[cfg(test)]
    pub fn is_held(&self, id: &str) -> bool {
        self.held().get(id).is_some_and(|n| *n > 0)
    }

    /// Block until no job holds profile `id`. Never on the UI thread.
    pub fn wait_free(&self, id: &str) {
        let mut held = self.held();
        while held.get(id).is_some_and(|n| *n > 0) {
            held = self.0.freed.wait(held).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// One job's hold on a profile's token; let go when dropped.
pub struct Hold {
    holds: Holds,
    id: String,
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut held = self.holds.held();
        if let Some(n) = held.get_mut(&self.id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                held.remove(&self.id);
            }
        }
        drop(held);
        self.holds.0.freed.notify_all();
    }
}

/// The app's own register.
pub fn global() -> &'static Holds {
    static GLOBAL: OnceLock<Holds> = OnceLock::new();
    GLOBAL.get_or_init(Holds::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_hold_counts_until_every_guard_is_dropped_and_wakes_a_waiter() {
        let holds = Holds::default();
        let (a, b) = (holds.hold("pa"), holds.hold("pa"));
        assert!(holds.is_held("pa") && !holds.is_held("pb") && holds.any_held());
        let waiter = {
            let holds = holds.clone();
            std::thread::spawn(move || holds.wait_free("pa"))
        };
        drop(a);
        std::thread::sleep(Duration::from_millis(50));
        assert!(!waiter.is_finished(), "one job still holds it");
        drop(b);
        waiter.join().unwrap();
        assert!(!holds.is_held("pa") && !holds.any_held());
        holds.wait_free("pa");
    }

    #[test]
    fn a_panicking_job_lets_go_of_its_hold() {
        let holds = Holds::default();
        let h = holds.clone();
        let r = std::thread::spawn(move || {
            let _held = h.hold("pa");
            panic!("the job broke");
        })
        .join();
        assert!(r.is_err());
        assert!(!holds.is_held("pa"));
    }
}
