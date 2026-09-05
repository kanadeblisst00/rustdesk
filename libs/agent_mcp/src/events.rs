use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
struct State {
    sequence: u64,
    events: VecDeque<Value>,
}

#[derive(Default)]
pub struct Events {
    state: Mutex<State>,
    changed: Condvar,
}

impl Events {
    pub fn push(&self, kind: &str, data: Value) {
        let mut state = self.state.lock().unwrap();
        state.sequence += 1;
        let sequence = state.sequence;
        state
            .events
            .push_back(json!({"cursor":sequence,"type":kind,"data":data}));
        while state.events.len() > 256 {
            state.events.pop_front();
        }
        self.changed.notify_all();
    }

    pub fn cursor(&self) -> u64 {
        self.state.lock().unwrap().sequence
    }

    pub fn read(
        &self,
        after: u64,
        kind: Option<&str>,
        timeout: Duration,
        enabled: impl Fn() -> bool,
    ) -> Result<Value, String> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        loop {
            // Authorization can inspect desktop locks also held by an event producer.
            drop(state);
            if !enabled() {
                return Err("MCP authorization revoked".into());
            }
            state = self.state.lock().unwrap();
            if after > state.sequence {
                return Err("Event cursor is ahead of this session".into());
            }
            let events: Vec<_> = state
                .events
                .iter()
                .filter(|e| {
                    e["cursor"].as_u64().unwrap_or(0) > after
                        && kind.map_or(true, |k| e["type"] == k)
                })
                .cloned()
                .collect();
            if !events.is_empty() || Instant::now() >= deadline {
                let truncated = state
                    .events
                    .front()
                    .and_then(|e| e["cursor"].as_u64())
                    .is_some_and(|n| after.saturating_add(1) < n);
                return Ok(
                    json!({"timed_out":events.is_empty(),"events":events,"next_cursor":state.sequence,"truncated":truncated}),
                );
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(100));
            state = self.changed.wait_timeout(state, wait).unwrap().0;
        }
    }
}

#[derive(Default)]
pub struct ByteLog {
    data: VecDeque<u8>,
    end: u64,
}

impl ByteLog {
    pub fn append(&mut self, data: &[u8]) {
        self.end = self.end.saturating_add(data.len() as u64);
        let data = &data[data.len().saturating_sub(1024 * 1024)..];
        let excess = (self.data.len() + data.len()).saturating_sub(1024 * 1024);
        self.data.drain(..excess);
        self.data.extend(data.iter().copied());
    }

    pub fn read(&self, cursor: u64, limit: usize) -> Result<(Vec<u8>, u64, bool), String> {
        if cursor > self.end {
            return Err("Terminal cursor is ahead of the output".into());
        }
        let start = self.end - self.data.len() as u64;
        let actual = cursor.max(start);
        let data: Vec<_> = self
            .data
            .iter()
            .skip((actual - start) as usize)
            .take(limit)
            .copied()
            .collect();
        let next = actual + data.len() as u64;
        Ok((data, next, cursor < start))
    }
}
