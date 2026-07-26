use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Backoff {
    initial: Duration,
    current: Duration,
    max: Duration,
}

impl Backoff {
    pub fn new(current: Duration, max: Duration) -> Self {
        Self {
            initial: current,
            current,
            max,
        }
    }

    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(self.max);
        delay
    }

    pub fn reset(&mut self) {
        self.current = self.initial;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        let initial = Duration::from_secs(1);
        Self {
            initial,
            current: initial,
            max: Duration::from_secs(30),
        }
    }
}
