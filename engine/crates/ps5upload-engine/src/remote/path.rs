//! `remote://<connection-id>/<path>` — how a file on a saved server travels through the app.
//! Only the engine interprets it; everything else passes it along as a string.

use super::RemoteError;

const SCHEME: &str = "remote://";

#[derive(Clone, Debug, PartialEq)]
pub struct RemotePath {
    pub connection_id: String,
    /// Normalised: a leading `/`, no empty, `.` or `..` segments.
    pub path: String,
}

pub fn is_remote(s: &str) -> bool {
    s.starts_with(SCHEME)
}

pub fn parse(s: &str) -> Result<RemotePath, RemoteError> {
    let rest = s
        .strip_prefix(SCHEME)
        .ok_or_else(|| RemoteError::BadPath(format!("not a remote path: {s}")))?;
    let (id, raw_path) = rest.split_once('/').unwrap_or((rest, ""));
    if !valid_id(id) {
        return Err(RemoteError::BadPath(format!("bad connection id in {s}")));
    }
    let mut segments = Vec::new();
    for raw in raw_path.split('/') {
        let seg = percent_decode(raw)?;
        match seg.as_str() {
            "" | "." => continue,
            ".." => return Err(RemoteError::BadPath(format!("{s} leaves its folder"))),
            _ => {}
        }
        check_segment(&seg)?;
        segments.push(seg);
    }
    Ok(RemotePath {
        connection_id: id.to_string(),
        path: format!("/{}", segments.join("/")),
    })
}

/// `remote://id/path`. The path is kept readable (spaces as they are); `parse` accepts both
/// that and a percent-encoded form.
pub fn format(id: &str, path: &str) -> String {
    format!("{SCHEME}{id}/{}", path.trim_start_matches('/'))
}

/// `base` + one entry name, for walking a tree. The name must be a single plain segment.
pub fn join(base: &str, name: &str) -> Result<String, RemoteError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(RemoteError::BadPath(format!("bad name {name:?}")));
    }
    check_segment(name)?;
    let base = base.trim_end_matches('/');
    Ok(format!("{base}/{name}"))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn check_segment(seg: &str) -> Result<(), RemoteError> {
    if seg.contains(['/', '\\', '\0']) {
        return Err(RemoteError::BadPath(format!("bad path segment {seg:?}")));
    }
    Ok(())
}

fn percent_decode(raw: &str) -> Result<String, RemoteError> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| RemoteError::BadPath(format!("bad escape in {raw}")))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| RemoteError::BadPath(format!("{raw} is not UTF-8")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalises() {
        let p = parse("remote://nas-1/games//ps5/./Minecraft.pkg").unwrap();
        assert_eq!(p.connection_id, "nas-1");
        assert_eq!(p.path, "/games/ps5/Minecraft.pkg");
        assert_eq!(parse("remote://nas-1").unwrap().path, "/");
        assert_eq!(parse("remote://nas-1/").unwrap().path, "/");
    }

    #[test]
    fn rejects_every_escape() {
        for bad in [
            "remote://nas-1/../etc",
            "remote://nas-1/games/../../x",
            "remote://nas-1/%2e%2e/x",
            "remote://nas-1/a%2fb",
            "remote://nas-1/a\\b",
            "remote:///games",
            "remote://a b/x",
            "smb://nas/share",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(join("/games", "../x").is_err());
        assert!(join("/games", "a/b").is_err());
        assert!(join("/games", "..").is_err());
        assert_eq!(join("/games", "a.pkg").unwrap(), "/games/a.pkg");
        assert_eq!(join("/", "a.pkg").unwrap(), "/a.pkg");
    }

    #[test]
    fn formats_round_trip() {
        assert_eq!(
            format("nas-1", "/games/a b.pkg"),
            "remote://nas-1/games/a b.pkg"
        );
        let s = format("nas-1", "/games/a b.pkg");
        assert_eq!(parse(&s).unwrap().path, "/games/a b.pkg");
        assert_eq!(parse("remote://nas-1/a%20b").unwrap().path, "/a b");
    }
}
