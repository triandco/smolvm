//! Cross-platform AF_UNIX (Unix-domain socket) support.
//!
//! The agent control channel, the vsock-port bridges, and the fork control
//! socket all talk over `AF_UNIX` stream sockets. `std` exposes
//! `std::os::unix::net::{UnixStream, UnixListener}` only on Unix, but AF_UNIX
//! is available on Windows 10 1809+ as well. `socket2` provides a portable
//! `Domain::UNIX` socket on every supported platform, so this module wraps it
//! into a small stream/listener API the rest of the host code shares.
//!
//! [`UdsStream`] implements [`std::io::Read`]/[`Write`] (via `socket2::Socket`)
//! and the timeout/clone/shutdown helpers the agent client relies on. On Unix
//! the raw fd is exposed for the interactive-terminal `poll()` loop; that loop
//! is itself Unix-only.

use socket2::{Domain, SockAddr, Socket, Type};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::Path;
use std::time::Duration;

/// A connected AF_UNIX stream socket.
#[derive(Debug)]
pub struct UdsStream {
    inner: Socket,
}

impl UdsStream {
    /// Connect to the AF_UNIX socket at `path`.
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let sock = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        connect_unix(&sock, path.as_ref())?;
        Ok(Self { inner: sock })
    }

    /// Connect with a deadline, including a full listener backlog.
    pub fn connect_timeout(path: impl AsRef<Path>, timeout: Duration) -> io::Result<Self> {
        let sock = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        connect_unix_timeout(&sock, path.as_ref(), timeout)?;
        Ok(Self { inner: sock })
    }

    /// Wrap an already-connected `socket2::Socket` (e.g. one returned by
    /// [`UdsListener::accept`]).
    pub fn from_socket(inner: Socket) -> Self {
        Self { inner }
    }

    /// Create an unnamed, connected pair of AF_UNIX stream sockets.
    ///
    /// Backed by `socketpair(2)`; Unix-only (Windows has no socketpair for
    /// AF_UNIX). Used by the agent-client unit tests.
    #[cfg(unix)]
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = Socket::pair(Domain::UNIX, Type::STREAM, None)?;
        Ok((Self { inner: a }, Self { inner: b }))
    }

    /// Create a connected pair of stream sockets (Windows test helper).
    ///
    /// Windows has no AF_UNIX `socketpair`, so this returns a loopback TCP pair.
    /// The agent-client unit tests only push bytes through a connected bidi
    /// stream, so the transport is interchangeable here.
    #[cfg(windows)]
    pub fn pair() -> io::Result<(Self, Self)> {
        use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
        let addr = listener.local_addr()?;
        let a = TcpStream::connect(addr)?;
        let (b, _) = listener.accept()?;
        Ok((
            Self {
                inner: Socket::from(a),
            },
            Self {
                inner: Socket::from(b),
            },
        ))
    }

    /// Set the read timeout. `None` disables the timeout (blocking).
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.inner.set_read_timeout(dur)
    }

    /// Get the current read timeout, if any.
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        self.inner.read_timeout()
    }

    /// Set the write timeout. `None` disables the timeout (blocking).
    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.inner.set_write_timeout(dur)
    }

    /// Clone the underlying socket handle (a new fd/handle referring to the
    /// same connection), mirroring `UnixStream::try_clone`.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            inner: self.inner.try_clone()?,
        })
    }

    /// Shut down the read, write, or both halves of the connection.
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.inner.shutdown(how)
    }

    /// Borrow the underlying `socket2::Socket`.
    pub fn as_socket(&self) -> &Socket {
        &self.inner
    }

    /// Raw Windows `SOCKET` handle for the interactive-terminal poll loop.
    ///
    /// Returned as a `u64` (the width of a `SOCKET`) so callers can hand it to
    /// the WinSock APIs the poll loop uses.
    #[cfg(windows)]
    pub fn raw_socket(&self) -> u64 {
        use std::os::windows::io::AsRawSocket;
        self.inner.as_raw_socket()
    }
}

#[cfg(unix)]
impl std::os::unix::io::AsRawFd for UdsStream {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.inner.as_raw_fd()
    }
}

impl Read for UdsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.inner).read(buf)
    }
}

impl Read for &UdsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.inner).read(buf)
    }
}

impl Write for UdsStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.inner).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&self.inner).flush()
    }
}

impl Write for &UdsStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.inner).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&self.inner).flush()
    }
}

/// A listening AF_UNIX socket.
#[derive(Debug)]
pub struct UdsListener {
    inner: Socket,
}

impl UdsListener {
    /// Bind a listening AF_UNIX socket at `path`.
    ///
    /// The caller is responsible for removing any stale socket file first
    /// (matching `UnixListener::bind` semantics, which also fail on EADDRINUSE).
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let sock = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        bind_unix(&sock, path.as_ref())?;
        sock.listen(128)?;
        Ok(Self { inner: sock })
    }

    /// Accept a single incoming connection.
    pub fn accept(&self) -> io::Result<UdsStream> {
        let (sock, _addr) = self.inner.accept()?;
        Ok(UdsStream::from_socket(sock))
    }

    /// Iterate over incoming connections, mirroring `UnixListener::incoming`.
    pub fn incoming(&self) -> Incoming<'_> {
        Incoming { listener: self }
    }
}

/// Iterator over incoming connections to a [`UdsListener`].
pub struct Incoming<'a> {
    listener: &'a UdsListener,
}

impl Iterator for Incoming<'_> {
    type Item = io::Result<UdsStream>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.listener.accept())
    }
}

/// Bind `sock` to the AF_UNIX path `path`.
///
/// `SockAddr::unix` refuses a path that does not fit `sun_path` (103 bytes on
/// macOS). The macOS kernel takes an address up to `SOCK_MAXADDRLEN` (255
/// bytes), so a longer path gets an address built by hand. A sandboxed app
/// needs this: its container path alone uses most of the 103 bytes.
pub fn bind_unix(sock: &Socket, path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if let Some(addr) = long_path::address(path)? {
        return long_path::bind(sock, &addr);
    }
    sock.bind(&SockAddr::unix(path)?)
}

/// Connect `sock` to the AF_UNIX path `path`. See [`bind_unix`] for long paths.
pub fn connect_unix(sock: &Socket, path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if let Some(addr) = long_path::address(path)? {
        return long_path::connect(sock, &addr);
    }
    sock.connect(&SockAddr::unix(path)?)
}

/// Connect `sock` to the AF_UNIX path `path` with a deadline. See
/// [`bind_unix`] for long paths.
pub fn connect_unix_timeout(sock: &Socket, path: &Path, timeout: Duration) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if let Some(addr) = long_path::address(path)? {
        return long_path::connect_timeout(sock, &addr, timeout);
    }
    sock.connect_timeout(&SockAddr::unix(path)?, timeout)
}

#[cfg(target_os = "macos")]
mod long_path {
    use socket2::Socket;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// The longest path that fits `sockaddr_un.sun_path` with its NUL.
    const SUN_PATH_MAX: usize = 103;
    /// `sun_len` and `sun_family` come before the path.
    const PATH_OFFSET: usize = 2;

    /// A `sockaddr_un` longer than the struct, as the macOS kernel accepts it.
    pub struct Address {
        bytes: [u8; libc::SOCK_MAXADDRLEN as usize],
        len: libc::socklen_t,
    }

    /// The hand-built address for `path`, or `None` when `SockAddr::unix` takes it.
    pub fn address(path: &Path) -> io::Result<Option<Address>> {
        let path = path.as_os_str().as_bytes();
        if path.len() <= SUN_PATH_MAX {
            return Ok(None);
        }
        let len = PATH_OFFSET + path.len() + 1;
        if len > libc::SOCK_MAXADDRLEN as usize || path.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "AF_UNIX path is longer than SOCK_MAXADDRLEN or contains a NUL byte",
            ));
        }
        let mut bytes = [0; libc::SOCK_MAXADDRLEN as usize];
        bytes[0] = len as u8;
        bytes[1] = libc::AF_UNIX as u8;
        bytes[PATH_OFFSET..PATH_OFFSET + path.len()].copy_from_slice(path);
        Ok(Some(Address {
            bytes,
            len: len as libc::socklen_t,
        }))
    }

    pub fn bind(sock: &Socket, addr: &Address) -> io::Result<()> {
        // SAFETY: `addr.bytes` holds `addr.len` initialized bytes of a sockaddr_un.
        let rc = unsafe { libc::bind(sock.as_raw_fd(), addr.bytes.as_ptr().cast(), addr.len) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn connect(sock: &Socket, addr: &Address) -> io::Result<()> {
        // SAFETY: `addr.bytes` holds `addr.len` initialized bytes of a sockaddr_un.
        let rc = unsafe { libc::connect(sock.as_raw_fd(), addr.bytes.as_ptr().cast(), addr.len) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Retries a full listener backlog (`EAGAIN`) until `timeout`, as
    /// `Socket::connect_timeout` waits for a connect in progress.
    pub fn connect_timeout(sock: &Socket, addr: &Address, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            match connect(sock, addr) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, e));
                }
                result => return result,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::{UdsListener, UdsStream};
        use std::io::{Read, Write};
        use std::time::Duration;

        #[test]
        fn a_path_longer_than_sun_path_binds_and_connects() {
            let dir = tempfile::tempdir().unwrap();
            let deep = dir.path().join("d".repeat(120));
            std::fs::create_dir_all(&deep).unwrap();
            let path = deep.join("agent.sock");
            assert!(path.as_os_str().len() > super::SUN_PATH_MAX);

            let listener = UdsListener::bind(&path).unwrap();
            let mut client = UdsStream::connect_timeout(&path, Duration::from_secs(1)).unwrap();
            let mut server = listener.accept().unwrap();
            client.write_all(b"ok").unwrap();
            let mut reply = [0; 2];
            server.read_exact(&mut reply).unwrap();
            assert_eq!(&reply, b"ok");
        }

        #[test]
        fn a_path_past_sockmaxaddrlen_is_refused() {
            let path = std::path::PathBuf::from(format!("/{}", "x".repeat(300)));
            assert!(super::address(&path).is_err());
        }
    }
}
