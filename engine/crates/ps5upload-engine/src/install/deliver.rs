//! Source → delivery decision (spec 2 §2): how a requested package reaches
//! Sony's installer. The engine owns this, never the client. Pure; no I/O.

use serde::{Deserialize, Serialize};

/// The request's `source` object: exactly one variant. Externally tagged
/// (serde default) over a one-field object gives exactly-one-variant
/// semantics and rejects `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    ConsolePath(String),
    HostFile(String),
    Remote { connection: String, path: String },
    Url(String),
}

impl Source {
    /// Stable string for history/logging.
    pub fn kind(&self) -> &'static str {
        match self {
            Source::ConsolePath(_) => "console_path",
            Source::HostFile(_) => "host_file",
            Source::Remote { .. } => "remote",
            Source::Url(_) => "url",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The daemon serves an on-console file over 127.0.0.1 and installs it.
    Loopback,
    /// The engine pkg-host serves the bytes; the daemon installs the http URL.
    Stream,
}

/// A console-local file loopback-installs; every other source streams.
pub fn decide_delivery(source: &Source) -> Delivery {
    match source {
        Source::ConsolePath(_) => Delivery::Loopback,
        _ => Delivery::Stream,
    }
}

/// The console's installer refuses a URL over this many bytes (127 accepted,
/// 128 refused — verified 0x80A30003). Over the limit, the engine hands the
/// console a short alias that redirects to the real URL.
pub const INSTALL_URL_LIMIT: usize = 127;

pub fn needs_short_alias(url: &str) -> bool {
    url.len() > INSTALL_URL_LIMIT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_path_is_the_only_loopback_source() {
        assert_eq!(
            decide_delivery(&Source::ConsolePath("/user/data/x.pkg".into())),
            Delivery::Loopback
        );
        assert_eq!(
            decide_delivery(&Source::HostFile("/tmp/x.pkg".into())),
            Delivery::Stream
        );
        assert_eq!(
            decide_delivery(&Source::Remote {
                connection: "nas".into(),
                path: "g/x.pkg".into()
            }),
            Delivery::Stream
        );
        assert_eq!(
            decide_delivery(&Source::Url("http://h/x.pkg".into())),
            Delivery::Stream
        );
    }

    #[test]
    fn source_kind_strings_match_history() {
        assert_eq!(Source::ConsolePath("p".into()).kind(), "console_path");
        assert_eq!(Source::HostFile("p".into()).kind(), "host_file");
        assert_eq!(
            Source::Remote {
                connection: "c".into(),
                path: "p".into()
            }
            .kind(),
            "remote"
        );
        assert_eq!(Source::Url("u".into()).kind(), "url");
    }

    #[test]
    fn short_alias_only_over_the_limit() {
        assert!(!needs_short_alias(&"h".repeat(127)));
        assert!(needs_short_alias(&"h".repeat(128)));
    }

    #[test]
    fn source_deserializes_from_the_request_shape() {
        let s: Source = serde_json::from_str(r#"{"console_path":"/user/data/x.pkg"}"#).unwrap();
        assert!(matches!(s, Source::ConsolePath(_)));
        let r: Source =
            serde_json::from_str(r#"{"remote":{"connection":"nas","path":"g/x.pkg"}}"#).unwrap();
        assert!(matches!(r, Source::Remote { .. }));
        assert!(serde_json::from_str::<Source>(r#"{}"#).is_err());
    }
}
