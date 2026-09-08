use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
struct State {
    active: bool,
    next: u64,
    waiting: VecDeque<u64>,
}

#[derive(Default)]
pub struct OperationQueue {
    state: Mutex<State>,
    changed: Condvar,
}

pub struct Permit<'a>(&'a OperationQueue);

impl OperationQueue {
    pub fn acquire(
        &self,
        timeout: Duration,
        mut check: impl FnMut() -> Result<(), String>,
    ) -> Result<Permit<'_>, String> {
        let deadline = Instant::now() + timeout;
        let ticket = {
            let mut state = self.state.lock().unwrap();
            if state.waiting.len() >= 8 {
                return Err(
                    "Session action queue is full (8 waiting calls); wait for earlier results"
                        .into(),
                );
            }
            let ticket = state.next;
            state.next = state.next.wrapping_add(1);
            state.waiting.push_back(ticket);
            ticket
        };
        loop {
            // Authorization may take desktop locks; never hold the queue lock while checking it.
            let checked = check();
            let mut state = self.state.lock().unwrap();
            if let Err(error) = checked {
                state.waiting.retain(|v| *v != ticket);
                self.changed.notify_all();
                return Err(error);
            }
            if !state.active && state.waiting.front() == Some(&ticket) {
                state.waiting.pop_front();
                state.active = true;
                return Ok(Permit(self));
            }
            if Instant::now() >= deadline {
                state.waiting.retain(|v| *v != ticket);
                self.changed.notify_all();
                return Err(
                    "Timed out waiting for the session action queue; this call did not execute"
                        .into(),
                );
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(25));
            drop(self.changed.wait_timeout(state, wait).unwrap());
        }
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().active = false;
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};

    #[test]
    fn serializes_fifo_and_cancelled_waiters_do_not_block_followers() {
        let queue = Arc::new(OperationQueue::default());
        let first = queue.acquire(Duration::ZERO, || Ok(())).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let mut threads = Vec::new();
        for index in 0..3 {
            let queue = queue.clone();
            let ready = ready_tx.clone();
            let done = done_tx.clone();
            threads.push(std::thread::spawn(move || {
                let mut notified = false;
                let permit = queue.acquire(Duration::from_secs(2), || {
                    if !notified {
                        ready.send(()).unwrap();
                        notified = true;
                    }
                    if index == 1 {
                        Err("revoked".into())
                    } else {
                        Ok(())
                    }
                });
                if index == 1 {
                    assert!(permit.is_err());
                } else {
                    let _permit = permit.unwrap();
                    done.send(index).unwrap();
                }
            }));
            ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        assert!(done_rx.try_recv().is_err());
        drop(first);
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(queue.acquire(Duration::ZERO, || Ok(())).is_ok());
    }

    #[test]
    fn timeout_and_rejection_release_their_queue_slots() {
        let queue = OperationQueue::default();
        let first = queue.acquire(Duration::ZERO, || Ok(())).unwrap();
        assert!(queue
            .acquire(Duration::ZERO, || Ok(()))
            .err()
            .unwrap()
            .contains("did not execute"));
        assert!(queue
            .acquire(Duration::ZERO, || Err("revoked".into()))
            .is_err());
        assert!(queue.state.lock().unwrap().waiting.is_empty());
        drop(first);
        assert!(queue.acquire(Duration::ZERO, || Ok(())).is_ok());
    }
}
