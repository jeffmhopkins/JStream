//! A minimal string error with context, so the spike needs no `anyhow` (R-RS-003).

use std::fmt;

pub struct Error(String);

impl Error {
    pub fn msg(msg: impl fmt::Display) -> Self {
        Self(msg.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<E: std::error::Error> From<E> for Error {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Prefixes an error (or a `None`) with what was being attempted.
pub trait Context<T> {
    fn context(self, what: impl fmt::Display) -> Result<T>;

    fn with_context<D: fmt::Display>(self, what: impl FnOnce() -> D) -> Result<T>
    where
        Self: Sized,
    {
        self.context(what())
    }
}

impl<T, E: fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context(self, what: impl fmt::Display) -> Result<T> {
        self.map_err(|e| Error(format!("{what}: {e}")))
    }
}

impl<T> Context<T> for Option<T> {
    fn context(self, what: impl fmt::Display) -> Result<T> {
        self.ok_or_else(|| Error(what.to_string()))
    }
}

macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::err::Error::msg(format!($($arg)*)))
    };
}
pub(crate) use bail;
