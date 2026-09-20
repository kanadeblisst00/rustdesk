use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

/// Created by credential verification, never deserialized from MCP arguments.
#[derive(Clone)]
pub struct CallContext {
    pub principal: String,
    pub credential: String,
    pub local: bool,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl CallContext {
    pub fn new(principal: String, credential: String, local: bool) -> Self {
        Self {
            principal,
            credential,
            local,
            deadline: Instant::now() + Duration::from_secs(60),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::SeqCst) || Instant::now() >= self.deadline {
            Err("Request cancelled or expired; observe before retrying an uncertain action".into())
        } else {
            Ok(())
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

pub(crate) struct CancelOnDrop(pub CallContext);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
