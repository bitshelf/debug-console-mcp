//! Reconnect manager — exponential backoff + reconnection attempts.
//!
//! Extracted from serial_engine.rs to keep the engine focused on orchestration.

use std::time::{Duration, Instant};

/// Manages reconnection attempts with exponential backoff.
/// Sequence: 1s → 2s → 4s → 8s → 16s → cap at 30s.
/// Resets to 1s after 60s of stable connection.
pub struct ReconnectManager {
    backoff: f64,
    last_disconnect: Option<Instant>,
    attempt_count: u32,
}

impl ReconnectManager {
    pub fn new() -> Self {
        Self {
            backoff: 1.0,
            last_disconnect: None,
            attempt_count: 0,
        }
    }

    /// Return the next reconnect delay. Call before each reconnect attempt.
    pub fn next_delay(&mut self) -> Duration {
        let now = Instant::now();
        if let Some(last) = self.last_disconnect {
            if (now - last).as_secs() > 60 {
                self.backoff = 1.0;
                self.attempt_count = 0;
            }
        }
        let delay = self.backoff;
        self.backoff = (delay * 2.0).min(30.0);
        self.last_disconnect = Some(now);
        self.attempt_count += 1;
        Duration::from_secs_f64(delay)
    }

    /// Reset backoff after successful reconnection.
    pub fn reset(&mut self) {
        self.backoff = 1.0;
        self.attempt_count = 0;
    }

    /// Current backoff value (for logging).
    pub fn current_backoff(&self) -> f64 {
        self.backoff
    }

    /// How many reconnection attempts have been made.
    pub fn attempts(&self) -> u32 {
        self.attempt_count
    }
}

impl Default for ReconnectManager {
    fn default() -> Self {
        Self::new()
    }
}
