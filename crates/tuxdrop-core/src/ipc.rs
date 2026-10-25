//! Local control channel between tuxdropd and the CLI where there is no
//! D-Bus session bus (macOS, Windows): one JSON request per connection, one
//! JSON response, each on a single line.
//!
//! The transport is a Unix socket in the data directory, or a named pipe on
//! Windows; both are private to the user.

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Same members as the D-Bus interface `org.tuxdrop.Daemon1`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    ListPeers,
    Announce,
    ListPending,
    Accept {
        id: String,
    },
    Reject {
        id: String,
    },
    Send {
        target: String,
        paths: Vec<String>,
    },
    Cancel {
        id: String,
    },
    /// Asks which device to send `paths` to (D-Bus `Share`).
    Share {
        paths: Vec<String>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Done,
    Peers(Vec<PeerEntry>),
    Pending(Vec<PendingEntry>),
    /// Transfer id returned by `Send`.
    Id(String),
    Error(String),
}

/// A device seen in the last 5 minutes (D-Bus `ListPeers`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PeerEntry {
    pub alias: String,
    /// Local name, empty if none.
    pub name: String,
    /// `ip:port`.
    pub addr: String,
    pub fingerprint: String,
    /// `https` or `http`.
    pub protocol: String,
    /// Seconds since last seen.
    pub age: u32,
}

/// An incoming transfer waiting for a decision (D-Bus `ListPending`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PendingEntry {
    pub id: String,
    pub alias: String,
    pub verified: bool,
    pub files: u32,
    pub size: u64,
}

/// Whether a daemon listens.
pub async fn running() -> io::Result<bool> {
    Ok(transport::connect().await?.is_some())
}

/// Sends `request` to the running daemon; `Ok(None)` if no daemon listens.
pub async fn call(request: &Request) -> io::Result<Option<Response>> {
    let Some(stream) = transport::connect().await? else {
        return Ok(None);
    };
    exchange(stream, request).await.map(Some)
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    request: &Request,
) -> io::Result<Response> {
    let mut stream = BufReader::new(stream);
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    stream.get_mut().write_all(line.as_bytes()).await?;
    let mut answer = String::new();
    stream.read_line(&mut answer).await?;
    Ok(serde_json::from_str(&answer)?)
}

/// Serves one connection: reads a request, answers with `handle`. A client
/// that closes without asking ([`running`]) gets no answer.
pub async fn serve_connection<S, F, Fut>(stream: S, handle: F) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(Request) -> Fut,
    Fut: Future<Output = Response>,
{
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    if stream.read_line(&mut line).await? == 0 {
        return Ok(());
    }
    let response = match serde_json::from_str(&line) {
        Ok(request) => handle(request).await,
        Err(e) => Response::Error(format!("bad request: {e}")),
    };
    let mut line = serde_json::to_string(&response)?;
    line.push('\n');
    stream.get_mut().write_all(line.as_bytes()).await?;
    stream.get_mut().shutdown().await
}

pub use transport::{Listener, Stream};

#[cfg(unix)]
mod transport {
    use std::io;
    use std::path::PathBuf;

    use tokio::net::{UnixListener, UnixStream};

    use crate::config::data_dir;

    pub type Stream = UnixStream;

    /// Named after the data directory, which the daemon and the CLI share.
    fn socket_path() -> PathBuf {
        use sha2::{Digest, Sha256};
        let dir = data_dir();
        let hash = Sha256::digest(dir.as_os_str().as_encoded_bytes());
        crate::sys::runtime_dir().join(format!("tuxdropd-{}.sock", hex::encode(&hash[..6])))
    }

    /// Where the daemon writes the socket path for the Finder share
    /// extension: sandboxed, it is given another temporary folder.
    #[cfg(target_os = "macos")]
    fn path_file() -> PathBuf {
        data_dir().join("control-socket")
    }

    pub async fn connect() -> io::Result<Option<UnixStream>> {
        match UnixStream::connect(socket_path()).await {
            Ok(stream) => Ok(Some(stream)),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    pub struct Listener {
        listener: UnixListener,
        path: PathBuf,
    }

    impl Listener {
        /// Fails with `AddrInUse` if another daemon is listening; a socket
        /// left by a daemon that crashed is replaced.
        pub async fn bind() -> io::Result<Self> {
            use std::os::unix::fs::PermissionsExt;
            let path = socket_path();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            if connect().await?.is_some() {
                return Err(io::ErrorKind::AddrInUse.into());
            }
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            #[cfg(target_os = "macos")]
            {
                std::fs::create_dir_all(data_dir())?;
                std::fs::write(path_file(), path.as_os_str().as_encoded_bytes())?;
            }
            Ok(Self { listener, path })
        }

        pub async fn accept(&mut self) -> io::Result<UnixStream> {
            Ok(self.listener.accept().await?.0)
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            #[cfg(target_os = "macos")]
            let _ = std::fs::remove_file(path_file());
        }
    }
}

#[cfg(windows)]
mod transport {
    use std::io;

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};

    pub type Stream = NamedPipeServer;

    fn pipe_name() -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        format!(r"\\.\pipe\tuxdrop-{user}")
    }

    pub async fn connect() -> io::Result<Option<NamedPipeClient>> {
        loop {
            match ClientOptions::new().open(pipe_name()) {
                Ok(client) => return Ok(Some(client)),
                Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                    return Ok(None);
                }
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub struct Listener {
        next: NamedPipeServer,
    }

    impl Listener {
        /// Fails if another daemon owns the pipe.
        pub async fn bind() -> io::Result<Self> {
            let next = ServerOptions::new()
                .first_pipe_instance(true)
                .create(pipe_name())
                .map_err(|e| match e.kind() {
                    io::ErrorKind::PermissionDenied => io::ErrorKind::AddrInUse.into(),
                    _ => e,
                })?;
            Ok(Self { next })
        }

        pub async fn accept(&mut self) -> io::Result<NamedPipeServer> {
            self.next.connect().await?;
            let fresh = ServerOptions::new().create(pipe_name())?;
            Ok(std::mem::replace(&mut self.next, fresh))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_responses_are_single_json_lines() {
        let request = Request::Send {
            target: "alpha".into(),
            paths: vec!["/tmp/a b.txt".into()],
        };
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(
            text,
            r#"{"method":"send","target":"alpha","paths":["/tmp/a b.txt"]}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        let text = serde_json::to_string(&Response::Id("x\ny".into())).unwrap();
        assert!(!text.contains('\n'));
    }

    #[tokio::test]
    async fn exchange_over_a_stream() {
        let (client, server) = tokio::io::duplex(1024);
        let served = tokio::spawn(serve_connection(server, async |request| match request {
            Request::Accept { id } => Response::Id(id),
            _ => Response::Error("unexpected".into()),
        }));
        let response = exchange(client, &Request::Accept { id: "42".into() })
            .await
            .unwrap();
        assert_eq!(response, Response::Id("42".into()));
        served.await.unwrap().unwrap();
    }
}
