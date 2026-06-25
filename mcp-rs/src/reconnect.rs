//! Reconnect manager — exponential backoff + reconnection attempts.
//!
//! Extracted from serial_engine.rs to keep the engine focused on orchestration.

use std::time::{Duration, Instant};

/// Manages reconnection attempts with exponential backoff and auto-reset.
///
/// Backoff sequence: 1s -> 2s -> 4s -> 8s -> 16s, capped at 30s.
/// If more than 60s elapses since the last disconnect, the backoff
/// resets to 1s (stable-connection heuristic).
pub struct ReconnectManager {
    backoff: f64,
    last_disconnect: Option<Instant>,
    attempt_count: u32,
}

impl ReconnectManager {
    /// Create a new `ReconnectManager` with backoff initialized to 1s.
    pub fn new() -> Self {
        Self {
            backoff: 1.0,
            last_disconnect: None,
            attempt_count: 0,
        }
    }

    /// Return the next reconnect delay and advance the backoff sequence.
    /// Doubles the internal backoff (capped at 30s) and increments the
    /// attempt counter. Call this before each reconnection attempt.
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

    /// Reset backoff to 1s and clear the attempt counter.
    /// Call this after a successful reconnection.
    pub fn reset(&mut self) {
        self.backoff = 1.0;
        self.attempt_count = 0;
    }

    /// Return the current backoff value in seconds (for diagnostics/logging).
    pub fn current_backoff(&self) -> f64 {
        self.backoff
    }

    /// Return the number of reconnection attempts made since the last reset
    /// or successful reconnection.
    pub fn attempts(&self) -> u32 {
        self.attempt_count
    }
}

impl Default for ReconnectManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_backoff_is_1s() {
        let mut rm = ReconnectManager::new();
        let delay = rm.next_delay();
        assert_eq!(delay.as_secs(), 1);
    }

    #[test]
    fn test_backoff_doubles() {
        let mut rm = ReconnectManager::new();
        assert_eq!(rm.next_delay().as_secs(), 1);
        assert_eq!(rm.next_delay().as_secs(), 2);
        assert_eq!(rm.next_delay().as_secs(), 4);
        assert_eq!(rm.next_delay().as_secs(), 8);
        assert_eq!(rm.next_delay().as_secs(), 16);
    }

    #[test]
    fn test_backoff_caps_at_30s() {
        let mut rm = ReconnectManager::new();
        for _ in 0..10 {
            rm.next_delay();
        }
        let delay = rm.next_delay();
        assert_eq!(delay.as_secs(), 30);
    }

    #[test]
    fn test_reset_restores_1s() {
        let mut rm = ReconnectManager::new();
        rm.next_delay(); // 1s
        rm.next_delay(); // 2s
        rm.next_delay(); // 4s
        rm.reset();
        assert_eq!(rm.next_delay().as_secs(), 1);
    }

    #[test]
    fn test_attempts_counter() {
        let mut rm = ReconnectManager::new();
        assert_eq!(rm.attempts(), 0);
        rm.next_delay();
        assert_eq!(rm.attempts(), 1);
        rm.next_delay();
        assert_eq!(rm.attempts(), 2);
        rm.reset();
        assert_eq!(rm.attempts(), 0);
    }

    #[test]
    fn test_current_backoff() {
        let mut rm = ReconnectManager::new();
        assert!((rm.current_backoff() - 1.0).abs() < 0.01);
        rm.next_delay(); // backoff becomes 2.0
        assert!((rm.current_backoff() - 2.0).abs() < 0.01);
    }

    #[test]
    fn test_default_creates_new() {
        let rm = ReconnectManager::default();
        assert!((rm.current_backoff() - 1.0).abs() < 0.01);
        assert_eq!(rm.attempts(), 0);
    }
}
