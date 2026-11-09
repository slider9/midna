//! The daemon's local socket. macOS (and other Unix) use std's Unix domain sockets; Windows 10
//! 1803+ has AF_UNIX sockets too, wrapped by `uds_windows` with the same API, so the socket is
//! still a path (`$MIDNA_HOME/midnad.sock`) and every connection is a full-duplex stream that
//! one thread can read while another writes.
#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};
