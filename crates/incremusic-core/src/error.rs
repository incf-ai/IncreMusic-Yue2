use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    IoPlain(#[from] std::io::Error),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("request timed out")]
    Timeout,
    #[error("bad response: {0}")]
    BadResponse(String),
    #[error("ffmpeg: {0}")]
    Ffmpeg(String),
    #[error("metadata: {0}")]
    Metadata(String),
    #[error("invalid name: {0}")]
    InvalidName(String),
    #[error("library: {0}")]
    Library(String),
    #[error("launcher: {0}")]
    Launcher(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    /// 4xx: a parameter error, never retried (§5.3).
    pub fn is_client_error(&self) -> bool {
        matches!(self, Error::Http { status, .. } if (400..500).contains(status))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| Error::io(path, e))
    }
}
