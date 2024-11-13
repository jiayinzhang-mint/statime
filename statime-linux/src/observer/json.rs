use super::{ObservableInstanceState, Observer};
use crate::{
    config::Config,
    metrics::exporter::{ObservableState, ProgramData},
};
use async_trait::async_trait;
use std::{
    fs::Permissions,
    os::unix::prelude::PermissionsExt,
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::{io::AsyncWriteExt, net::UnixStream, sync::watch::Receiver};

pub struct JsonObserver {
    state: Arc<Mutex<ObservableInstanceState>>,
}

#[async_trait]
impl Observer for JsonObserver {
    async fn observe(
        &self,
        config: &Config,
        instance_state_receiver: Receiver<ObservableInstanceState>,
    ) -> std::io::Result<()> {
        let start_time = Instant::now();

        let path = match config.observability.observation_path {
            Some(ref path) => path,
            None => return Ok(()),
        };

        // this binary needs to run as root to be able to adjust the system clock.
        // by default, the socket inherits root permissions, but the client should not
        // need elevated permissions to read from the socket. So we explicitly set
        // the permissions
        let permissions: std::fs::Permissions =
            PermissionsExt::from_mode(config.observability.observation_permissions);

        let peers_listener = create_unix_socket_with_permissions(path, permissions)?;

        loop {
            let (mut stream, _addr) = peers_listener.accept().await?;

            let state = instance_state_receiver.borrow().to_owned();

            let observe = ObservableState {
                program: ProgramData::with_uptime(start_time.elapsed().as_secs_f64()),
                instance: state.clone(),
            };

            match self.state.lock() {
                Ok(mut _state) => *_state = instance_state_receiver.borrow().to_owned(),
                Err(err) => {
                    tracing::error!(err=?err, "Failed to lock state");
                }
            }

            write_json(&mut stream, &observe).await?;
        }
    }

    async fn get_state(&self) -> ObservableInstanceState {
        match self.state.lock() {
            Ok(_state) => _state.clone(),
            Err(e) => {
                tracing::error!(err=?e, "Failed to lock state");
                ObservableInstanceState::default()
            }
        }
    }
}

impl JsonObserver {
    pub fn new() -> JsonObserver {
        JsonObserver {
            state: Arc::new(Mutex::new(ObservableInstanceState::default())),
        }
    }
}

fn other_error<T>(msg: String) -> std::io::Result<T> {
    use std::io::{Error, ErrorKind};
    Err(Error::new(ErrorKind::Other, msg))
}

pub fn create_unix_socket_with_permissions(
    path: &Path,
    permissions: Permissions,
) -> std::io::Result<tokio::net::UnixListener> {
    let listener = create_unix_socket(path)?;

    std::fs::set_permissions(path, permissions)?;

    Ok(listener)
}

fn create_unix_socket(path: &Path) -> std::io::Result<tokio::net::UnixListener> {
    // must unlink path before the bind below (otherwise we get "address already in
    // use")
    if path.exists() {
        use std::os::unix::fs::FileTypeExt;

        let meta = std::fs::metadata(path)?;
        if !meta.file_type().is_socket() {
            return other_error(format!("path {path:?} exists but is not a socket"));
        }

        std::fs::remove_file(path)?;
    }

    // OS errors are terrible; let's try to do better
    let error = match tokio::net::UnixListener::bind(path) {
        Ok(listener) => return Ok(listener),
        Err(e) => e,
    };

    // we don create parent directories
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            let msg = format!(
                r"Could not create observe socket at {:?} because its parent directory does not exist",
                &path
            );
            return other_error(msg);
        }
    }

    // otherwise, just forward the OS error
    let msg = format!(
        "Could not create observe socket at {:?}: {:?}",
        &path, error
    );

    other_error(msg)
}

pub async fn write_json<T>(stream: &mut UnixStream, value: &T) -> std::io::Result<()>
where
    T: serde::Serialize,
{
    let bytes = serde_json::to_vec(value).unwrap();
    stream.write_all(&bytes).await
}