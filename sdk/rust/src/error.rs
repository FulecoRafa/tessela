use std::fmt;

/// What a method reports to its caller: `{"id":..,"error":"<message>"}`.
///
/// Converts with `?` from any `std::error::Error`, and from `&str` or
/// `String` (`Err("no such device".into())`).
pub struct Error(String);

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn msg(message: impl fmt::Display) -> Self {
        Self(message.to_string())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

// Like anyhow, `Error` is not a std error itself, so this blanket impl does
// not collide with `From<T> for T`. Strings and every std error qualify.
impl<E: Into<Box<dyn std::error::Error + Send + Sync>>> From<E> for Error {
    fn from(e: E) -> Self {
        Self(e.into().to_string())
    }
}
