use clap::Parser;
use statime_linux::{
    initialize_logging_parse_config,
    observer::{json::JsonObserver, Observer as _},
    start_sync,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::time;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    /// Configuration file to use
    #[clap(long = "config", short = 'c', default_value = "./statime.toml")]
    config_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let config = initialize_logging_parse_config(
        &args
            .config_file
            .expect("could not determine config file path"),
    );

    let observer = JsonObserver::new();

    let obs = Arc::new(observer);

    let obs_clone_val = Arc::clone(&obs);
    let task_sync = tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(1));

        loop {
            let _ = interval.tick().await;
            let state = obs_clone_val.get_state().await;
            let state_str = serde_json::to_string(&state).unwrap();
            tracing::info!(state = state_str, "State");
        }
    });

    let obs_clone_sync = Arc::clone(&obs);
    start_sync(config, obs_clone_sync).await;

    let _ = tokio::join!(task_sync);
}
