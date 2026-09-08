use rustdesk_agent_mcp::queue::OperationQueue;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
};

pub(super) fn get(peer: &str, kind: &str) -> Arc<OperationQueue> {
    type Queues = HashMap<(String, String), Weak<OperationQueue>>;
    static QUEUES: OnceLock<Mutex<Queues>> = OnceLock::new();
    let mut queues = QUEUES.get_or_init(Default::default).lock().unwrap();
    queues.retain(|_, queue| queue.strong_count() > 0);
    let key = (peer.to_owned(), kind.to_owned());
    if let Some(queue) = queues.get(&key).and_then(Weak::upgrade) {
        return queue;
    }
    let queue = Arc::new(OperationQueue::default());
    queues.insert(key, Arc::downgrade(&queue));
    queue
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_slow_device_does_not_block_another_device_or_session_kind() {
        let first = get("device-a", "terminal");
        let _held = first.acquire(Duration::ZERO, || Ok(())).unwrap();
        assert!(get("device-a", "terminal")
            .acquire(Duration::ZERO, || Ok(()))
            .is_err());
        assert!(get("device-b", "terminal")
            .acquire(Duration::ZERO, || Ok(()))
            .is_ok());
        assert!(get("device-a", "files")
            .acquire(Duration::ZERO, || Ok(()))
            .is_ok());
    }
}
