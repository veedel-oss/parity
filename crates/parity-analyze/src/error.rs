use std::fmt;

// An error that ends the run with exit code 2. The message is meant for the
// user and says what to do where possible.
#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error(s.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

macro_rules! bail {
    ($($t:tt)*) => { return Err($crate::error::Error(format!($($t)*))) };
}
pub(crate) use bail;
