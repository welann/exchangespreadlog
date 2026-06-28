use std::time::{SystemTime, UNIX_EPOCH};

pub fn unix_time_ns() -> i128 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    duration.as_nanos() as i128
}
