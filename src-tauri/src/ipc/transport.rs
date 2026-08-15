//! Cross-platform local IPC transport.
//!
//! Unix uses a private domain socket. Windows uses a byte-mode named pipe partitioned by
//! the current user identity and AskHuman config directory. The daemon protocol above this
//! module remains NDJSON on every platform.

use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub type OwnedReadHalf = tokio::io::ReadHalf<Stream>;
pub type OwnedWriteHalf = tokio::io::WriteHalf<Stream>;

/// Human-readable local endpoint included in daemon metadata and diagnostics.
pub fn socket_path() -> PathBuf {
    endpoint_path("daemon")
}

/// Human-readable endpoint for one local IPC role.
pub fn endpoint_path(role: &str) -> PathBuf {
    #[cfg(unix)]
    {
        if role == "gui-host" {
            crate::paths::gui_host_sock()
        } else {
            crate::paths::config_dir().join(format!("{role}.sock"))
        }
    }
    #[cfg(windows)]
    {
        PathBuf::from(windows_endpoint(role))
    }
    #[cfg(not(any(unix, windows)))]
    {
        crate::paths::config_dir().join("daemon.unsupported")
    }
}

pub struct Stream {
    inner: StreamInner,
}

impl Stream {
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        tokio::io::split(self)
    }
}

#[cfg(unix)]
enum StreamInner {
    Unix(tokio::net::UnixStream),
}

#[cfg(windows)]
enum StreamInner {
    Client(tokio::net::windows::named_pipe::NamedPipeClient),
    Server(tokio::net::windows::named_pipe::NamedPipeServer),
}

#[cfg(not(any(unix, windows)))]
enum StreamInner {}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        #[cfg(unix)]
        match &mut self.get_mut().inner {
            StreamInner::Unix(stream) => Pin::new(stream).poll_read(cx, buf),
        }
        #[cfg(windows)]
        match &mut self.get_mut().inner {
            StreamInner::Client(stream) => Pin::new(stream).poll_read(cx, buf),
            StreamInner::Server(stream) => Pin::new(stream).poll_read(cx, buf),
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (self, cx, buf);
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "local IPC is unsupported on this platform",
            )))
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        #[cfg(unix)]
        match &mut self.get_mut().inner {
            StreamInner::Unix(stream) => Pin::new(stream).poll_write(cx, buf),
        }
        #[cfg(windows)]
        match &mut self.get_mut().inner {
            StreamInner::Client(stream) => Pin::new(stream).poll_write(cx, buf),
            StreamInner::Server(stream) => Pin::new(stream).poll_write(cx, buf),
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (self, cx, buf);
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "local IPC is unsupported on this platform",
            )))
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        #[cfg(unix)]
        match &mut self.get_mut().inner {
            StreamInner::Unix(stream) => Pin::new(stream).poll_flush(cx),
        }
        #[cfg(windows)]
        match &mut self.get_mut().inner {
            StreamInner::Client(stream) => Pin::new(stream).poll_flush(cx),
            StreamInner::Server(stream) => Pin::new(stream).poll_flush(cx),
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (self, cx);
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "local IPC is unsupported on this platform",
            )))
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        #[cfg(unix)]
        match &mut self.get_mut().inner {
            StreamInner::Unix(stream) => Pin::new(stream).poll_shutdown(cx),
        }
        #[cfg(windows)]
        match &mut self.get_mut().inner {
            StreamInner::Client(stream) => Pin::new(stream).poll_shutdown(cx),
            StreamInner::Server(stream) => Pin::new(stream).poll_shutdown(cx),
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (self, cx);
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "local IPC is unsupported on this platform",
            )))
        }
    }
}

#[cfg(unix)]
pub struct Listener(tokio::net::UnixListener);

#[cfg(windows)]
pub struct Listener {
    endpoint: String,
    next: tokio::sync::Mutex<Option<tokio::net::windows::named_pipe::NamedPipeServer>>,
}

#[cfg(not(any(unix, windows)))]
pub struct Listener;

impl Listener {
    pub async fn accept(&self) -> io::Result<(Stream, ())> {
        #[cfg(unix)]
        {
            let (stream, _) = self.0.accept().await?;
            Ok((
                Stream {
                    inner: StreamInner::Unix(stream),
                },
                (),
            ))
        }
        #[cfg(windows)]
        {
            let server = {
                let mut slot = self.next.lock().await;
                slot.take().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotConnected,
                        "named pipe listener unavailable",
                    )
                })?
            };
            if let Err(error) = server.connect().await {
                let mut slot = self.next.lock().await;
                *slot = Some(create_windows_server(&self.endpoint, false)?);
                return Err(error);
            }
            // Pre-create the next instance before handing the connected pipe to a task.
            let replacement = create_windows_server(&self.endpoint, false)?;
            *self.next.lock().await = Some(replacement);
            Ok((
                Stream {
                    inner: StreamInner::Server(server),
                },
                (),
            ))
        }
        #[cfg(not(any(unix, windows)))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "local IPC is unsupported on this platform",
        ))
    }
}

pub async fn connect() -> io::Result<Stream> {
    connect_role("daemon").await
}

/// Connect to a local IPC service by role.
pub async fn connect_role(role: &str) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(endpoint_path(role))
            .await
            .map(|stream| Stream {
                inner: StreamInner::Unix(stream),
            })
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;

        let endpoint = windows_endpoint(role);
        for attempt in 0..40u64 {
            match ClientOptions::new().open(&endpoint) {
                Ok(stream) => {
                    return Ok(Stream {
                        inner: StreamInner::Client(stream),
                    })
                }
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10 + attempt * 5)).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "named pipe remained busy",
        ))
    }
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "local IPC is unsupported on this platform",
    ))
}

pub fn bind() -> io::Result<Listener> {
    bind_role("daemon")
}

/// Bind a local IPC service by role.
pub fn bind_role(role: &str) -> io::Result<Listener> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = endpoint_path(role);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path)?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        Ok(Listener(listener))
    }
    #[cfg(windows)]
    {
        let endpoint = windows_endpoint(role);
        let first = create_windows_server(&endpoint, true)?;
        Ok(Listener {
            endpoint,
            next: tokio::sync::Mutex::new(Some(first)),
        })
    }
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "local IPC is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn create_windows_server(
    endpoint: &str,
    first: bool,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use tokio::net::windows::named_pipe::ServerOptions;

    ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .create(endpoint)
}

#[cfg(windows)]
pub(crate) fn windows_endpoint(role: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    let identity = format!(
        "{}\\{}|{}|{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default(),
        std::env::var("SESSIONNAME").unwrap_or_default(),
        crate::paths::config_dir().to_string_lossy()
    );
    let digest = Sha256::digest(identity.as_bytes());
    let mut suffix = String::with_capacity(24);
    for byte in &digest[..12] {
        write!(&mut suffix, "{byte:02x}").expect("writing to String cannot fail");
    }
    format!(r"\\.\pipe\AskHuman-{suffix}-{role}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn endpoint_is_partitioned_by_config_dir() {
        let path = super::socket_path();
        assert!(!path.as_os_str().is_empty());
        assert!(path.to_string_lossy().contains("daemon"));
        assert!(super::endpoint_path("gui-host")
            .to_string_lossy()
            .contains("gui-host"));
    }
}
