//! Error type shared by the library, mapped onto the kit error codes and exit codes.

/// Everything that can go wrong in `codegraph`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Bad arguments or config.
    #[error("{0}")]
    Usage(String),
    /// Nothing matched the query.
    #[error("{0}")]
    NotFound(String),
    /// Filesystem failure.
    #[error("{context}: {source}")]
    Io {
        /// What we were doing.
        context: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// SQLite failure.
    #[cfg(feature = "native")]
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    /// A tree-sitter query failed to compile (a bug in `queries/`).
    #[error("query for {lang}: {message}")]
    Query {
        /// Language whose query failed.
        lang: &'static str,
        /// Compiler message.
        message: String,
    },
    /// Bug or unexpected state.
    #[error("{0}")]
    Internal(String),
}

impl Error {
    /// Wrap an I/O error with context.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// The kit snake_case error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Usage(_) => "usage",
            Self::NotFound(_) => "not_found",
            Self::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied => {
                "permission_denied"
            }
            Self::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => "not_found",
            Self::Io { .. } => "io",
            #[cfg(feature = "native")]
            Self::Db(_) => "database",
            Self::Query { .. } | Self::Internal(_) => "internal",
        }
    }

    /// The kit process exit code.
    pub fn exit_code(&self) -> u8 {
        match self.code() {
            "usage" => 2,
            "not_found" => 3,
            "permission_denied" => 4,
            _ => 1,
        }
    }
}

/// Crate result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_exit_codes() {
        let cases = [
            (Error::Usage(String::new()), "usage", 2),
            (Error::NotFound(String::new()), "not_found", 3),
            (Error::Internal(String::new()), "internal", 1),
            (
                Error::io("x", std::io::Error::from(std::io::ErrorKind::NotFound)),
                "not_found",
                3,
            ),
            (
                Error::io(
                    "x",
                    std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                ),
                "permission_denied",
                4,
            ),
        ];
        for (err, code, exit) in cases {
            assert_eq!(err.code(), code, "{err:?}");
            assert_eq!(err.exit_code(), exit, "{err:?}");
        }
    }
}
