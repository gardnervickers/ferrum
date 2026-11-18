use std::fmt;

/// Everything that can go wrong talking to LinuxCNC.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// This build has no NML backend (no LinuxCNC headers were found, or
    /// not Linux). See the crate docs.
    NoBackend,
    /// The NML channels could not be opened; usually LinuxCNC isn't running.
    Connect(String),
    /// The shim and this crate disagree on the status layout: rebuild both.
    AbiMismatch { c: usize, rust: usize },
    /// No status has arrived yet, or the status channel went away.
    NoStatus,
    /// Writing a command to the command channel failed.
    Send(&'static str),
    /// A string argument has a NUL byte in it.
    Nul,
    /// The command was not taken or not finished in time.
    Timeout,
    /// Task reported the command as failed (RCS_ERROR).
    Failed,
    /// A HAL call failed.
    Hal(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoBackend => f.write_str("built without the LinuxCNC NML backend"),
            Error::Connect(e) => write!(f, "cannot connect to LinuxCNC: {e}"),
            Error::AbiMismatch { c, rust } => write!(f, "status layout mismatch: {c} bytes in C, {rust} in Rust"),
            Error::NoStatus => f.write_str("LinuxCNC status channel is not available"),
            Error::Send(what) => write!(f, "failed to send {what} to LinuxCNC"),
            Error::Nul => f.write_str("string argument contains a NUL byte"),
            Error::Timeout => f.write_str("timed out waiting for LinuxCNC"),
            Error::Failed => f.write_str("LinuxCNC reported the command as failed"),
            Error::Hal(e) => write!(f, "HAL: {e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// For callers that keep errors as text.
impl From<Error> for String {
    fn from(e: Error) -> String {
        e.to_string()
    }
}
