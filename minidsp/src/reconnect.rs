//! Auto-reconnection with exponential backoff.

use std::time::Duration;

/// Reconnection configuration.
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Base delay between reconnection attempts.
    pub base_delay: Duration,
    /// Maximum delay between attempts.
    pub max_delay: Duration,
    /// Multiplier for exponential backoff.
    pub multiplier: f64,
    /// Maximum number of consecutive retries before giving up (0 = infinite).
    pub max_retries: u32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            multiplier: 2.0,
            max_retries: 0, // infinite
        }
    }
}

/// Calculate the delay for a given retry attempt.
pub fn backoff_delay(config: &ReconnectConfig, attempt: u32) -> Duration {
    let delay_ms = config.base_delay.as_millis() as f64 * config.multiplier.powi(attempt as i32);
    let capped = delay_ms.min(config.max_delay.as_millis() as f64);
    Duration::from_millis(capped as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_increases_exponentially() {
        let config = ReconnectConfig {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            multiplier: 2.0,
            max_retries: 0,
        };
        assert_eq!(backoff_delay(&config, 0).as_millis(), 100);
        assert_eq!(backoff_delay(&config, 1).as_millis(), 200);
        assert_eq!(backoff_delay(&config, 2).as_millis(), 400);
        assert_eq!(backoff_delay(&config, 3).as_millis(), 800);
    }

    #[test]
    fn backoff_caps_at_max() {
        let config = ReconnectConfig {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
            multiplier: 2.0,
            max_retries: 0,
        };
        assert_eq!(backoff_delay(&config, 10).as_millis(), 1000); // capped at 1s
        assert_eq!(backoff_delay(&config, 20).as_millis(), 1000);
    }
}
