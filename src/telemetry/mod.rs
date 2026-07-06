use tracing_subscriber::{EnvFilter, fmt};

pub fn init() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,exchangespreadlog=debug"));

    if let Err(err) = fmt().with_env_filter(filter).try_init() {
        let message = err.to_string();
        if !message.contains("global default") && !message.contains("set as global default") {
            eprintln!("failed to initialize tracing subscriber: {err}");
        }
    }
}
