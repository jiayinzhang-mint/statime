extern crate core;

use config::Config;
pub use metrics::exporter::main as metrics_exporter_main;
use std::path::Path;
use tracing::LogLevel;
use tracing_log::LogTracer;
use tracing_subscriber::util::SubscriberInitExt;

pub mod clock;
pub mod config;
pub mod metrics;
pub mod observer;
pub mod port;
pub mod socket;
pub mod tlvforwarder;
pub mod tracing;

pub fn initialize_logging_parse_config(path: &Path) -> Config {
    LogTracer::init().expect("Internal error: could not attach logger");

    // Early setup for logging
    let startup_tracing = crate::tracing::tracing_init(LogLevel::default());

    let config = ::tracing::subscriber::with_default(startup_tracing, || {
        Config::from_file(path).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        })
    });

    crate::tracing::tracing_init(config.loglevel).init();
    config
}
