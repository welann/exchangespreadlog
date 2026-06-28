use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct HeartbeatPolicy {
    pub interval: Duration,
}

impl HeartbeatPolicy {
    pub fn hyperliquid() -> Self {
        Self {
            interval: Duration::from_secs(30),
        }
    }

    pub fn lighter() -> Self {
        Self {
            interval: Duration::from_secs(60),
        }
    }
}
