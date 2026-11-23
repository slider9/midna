//! The keeper, `midna-pty.exe`: one small process per Windows terminal that owns its
//! pseudoconsole, its shell and the shell's job, so midnad can be replaced (an update, a
//! restart, a crash) while the terminal keeps running. midnad keeps the screens, state and
//! socket; a new midnad attaches to each keeper and carries on reading and writing the same
//! pipes.
//!
//! - [`proto`]: protocol 1, the frames on a keeper's control pipe (every platform, for tests).
//! - [`names`]: a keeper's pipe name and its record in `MIDNA_HOME\pty`.
//! - [`client`] (Windows): starting a keeper and talking to one, for midnad.
//! - `keeper` (Windows): the keeper process itself, run by `src/main.rs`.
pub mod names;
pub mod proto;

#[cfg(windows)]
pub mod client;
#[cfg(windows)]
mod console;
#[cfg(windows)]
pub mod keeper;
#[cfg(windows)]
pub mod shell;
#[cfg(windows)]
mod win;
