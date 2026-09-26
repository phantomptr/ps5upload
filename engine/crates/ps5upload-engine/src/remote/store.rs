//! Saved connections, kept in `<data>/connections.json`, with each secret (password or SSH key)
//! encrypted by a per-install key in `<data>/connections.key`. Nothing outside this module ever
//! sees a secret except the connector that signs in with it; the API reports `has_secret` only.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Smb,
    Ftp,
    Ftps,
    Sftp,
}

pub fn default_port(p: &Protocol) -> u16 {
    match p {
        Protocol::Smb => 445,
        Protocol::Ftp | Protocol::Ftps => 21,
        Protocol::Sftp => 22,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Connection {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    /// SMB only: the share the paths live in.
    #[serde(default)]
    pub share: String,
    /// Empty = guest (SMB) or anonymous (FTP).
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub start_path: String,
    /// SFTP: the host-key fingerprint the user accepted ("SHA256:…").
    #[serde(default)]
    pub host_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Secret {
    None,
    Password {
        password: String,
    },
    Key {
        pem: String,
        passphrase: Option<String>,
    },
}

impl Secret {
    /// The strings that must never appear in anything shown or logged.
    pub fn sensitive(&self) -> Vec<&str> {
        match self {
            Secret::None => vec![],
            Secret::Password { password } => vec![password.as_str()],
            Secret::Key { pem, passphrase } => {
                let mut v = vec![pem.as_str()];
                if let Some(p) = passphrase {
                    v.push(p.as_str());
                }
                v
            }
        }
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ConnectionView {
    #[serde(flatten)]
    pub conn: Connection,
    pub has_secret: bool,
}

pub struct Store {
    dir: PathBuf,
    key: [u8; 32],
    state: Mutex<Vec<(Connection, Secret)>>,
}

/// `PS5UPLOAD_DATA_DIR`, else `~/.ps5upload`.
pub fn data_dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("PS5UPLOAD_DATA_DIR") {
        if !v.trim().is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    (!home.trim().is_empty()).then(|| PathBuf::from(home).join(".ps5upload"))
}

/// On disk: each secret is `base64(nonce || ciphertext)` of the JSON-encoded [`Secret`].
#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(flatten)]
    conn: Connection,
    #[serde(default)]
    secret: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct FileShape {
    version: u32,
    connections: Vec<Stored>,
}

impl Store {
    /// Load `<dir>/connections.json` (missing = empty) with the key in `<dir>/connections.key`
    /// (created on first use, readable by this user only).
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let key = load_or_create_key(&dir.join("connections.key"))?;
        let path = dir.join("connections.json");
        let mut state = Vec::new();
        if path.exists() {
            let shape: FileShape = serde_json::from_slice(&std::fs::read(&path)?)?;
            for st in shape.connections {
                // A secret that no longer decrypts (key replaced) is dropped, not guessed at:
                // the user re-enters it, and the connection itself survives.
                let secret = st
                    .secret
                    .as_deref()
                    .and_then(|blob| decrypt(&key, blob))
                    .unwrap_or(Secret::None);
                state.push((st.conn, secret));
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            key,
            state: Mutex::new(state),
        })
    }

    pub fn list(&self) -> Vec<ConnectionView> {
        self.lock().iter().map(|(c, s)| view(c, s)).collect()
    }

    pub fn get(&self, id: &str) -> Option<(Connection, Secret)> {
        self.lock().iter().find(|(c, _)| c.id == id).cloned()
    }

    pub fn add(&self, mut conn: Connection, secret: Secret) -> anyhow::Result<ConnectionView> {
        let mut st = self.lock();
        conn.id = new_id(&conn.name, &st);
        st.push((conn.clone(), secret.clone()));
        self.save(&st)?;
        Ok(view(&conn, &secret))
    }

    /// `None` keeps the stored secret.
    pub fn update(
        &self,
        id: &str,
        mut conn: Connection,
        secret: Option<Secret>,
    ) -> anyhow::Result<ConnectionView> {
        let mut st = self.lock();
        let slot = st
            .iter_mut()
            .find(|(c, _)| c.id == id)
            .ok_or_else(|| anyhow::anyhow!("no connection {id}"))?;
        conn.id = id.to_string();
        // A changed server or protocol invalidates the accepted host key.
        if slot.0.host != conn.host || slot.0.port != conn.port || slot.0.protocol != conn.protocol
        {
            conn.host_key = None;
        } else if conn.host_key.is_none() {
            conn.host_key = slot.0.host_key.clone();
        }
        slot.0 = conn.clone();
        if let Some(s) = secret {
            slot.1 = s;
        }
        let v = view(&slot.0, &slot.1);
        self.save(&st)?;
        Ok(v)
    }

    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        let mut st = self.lock();
        let before = st.len();
        st.retain(|(c, _)| c.id != id);
        if st.len() == before {
            anyhow::bail!("no connection {id}");
        }
        self.save(&st)
    }

    pub fn set_host_key(&self, id: &str, fingerprint: &str) -> anyhow::Result<()> {
        let mut st = self.lock();
        let slot = st
            .iter_mut()
            .find(|(c, _)| c.id == id)
            .ok_or_else(|| anyhow::anyhow!("no connection {id}"))?;
        slot.0.host_key = Some(fingerprint.to_string());
        self.save(&st)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(Connection, Secret)>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, st: &[(Connection, Secret)]) -> anyhow::Result<()> {
        let shape = FileShape {
            version: 1,
            connections: st
                .iter()
                .map(|(c, s)| {
                    Ok(Stored {
                        conn: c.clone(),
                        secret: match s {
                            Secret::None => None,
                            s => Some(encrypt(&self.key, s)?),
                        },
                    })
                })
                .collect::<anyhow::Result<_>>()?,
        };
        let path = self.dir.join("connections.json");
        let tmp = self.dir.join("connections.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&shape)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

fn view(c: &Connection, s: &Secret) -> ConnectionView {
    ConnectionView {
        conn: c.clone(),
        has_secret: !matches!(s, Secret::None),
    }
}

/// A readable, stable id: a slug of the name plus four hex digits.
fn new_id(name: &str, taken: &[(Connection, Secret)]) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        "server".to_string()
    } else {
        slug.chars().take(40).collect()
    };
    loop {
        let mut r = [0u8; 2];
        let _ = getrandom::fill(&mut r);
        let id = format!("{slug}-{:02x}{:02x}", r[0], r[1]);
        if !taken.iter().any(|(c, _)| c.id == id) {
            return id;
        }
    }
}

fn load_or_create_key(path: &Path) -> anyhow::Result<[u8; 32]> {
    if let Ok(bytes) = std::fs::read(path) {
        if let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice()) {
            return Ok(key);
        }
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| anyhow::anyhow!("reading OS randomness: {e}"))?;
    write_private(path, &key)?;
    Ok(key)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

fn encrypt(key: &[u8; 32], secret: &Secret) -> anyhow::Result<String> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use base64::Engine;
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("reading OS randomness: {e}"))?;
    let plain = serde_json::to_vec(secret)?;
    let sealed = cipher
        .encrypt(&Nonce::from(nonce), plain.as_slice())
        .map_err(|_| anyhow::anyhow!("could not encrypt the secret"))?;
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&sealed);
    Ok(base64::engine::general_purpose::STANDARD.encode(blob))
}

fn decrypt(key: &[u8; 32], blob: &str) -> Option<Secret> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .ok()?;
    if bytes.len() < 12 {
        return None;
    }
    let (nonce, sealed) = bytes.split_at(12);
    let nonce: [u8; 12] = nonce.try_into().ok()?;
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let plain = cipher.decrypt(&Nonce::from(nonce), sealed).ok()?;
    serde_json::from_slice(&plain).ok()
}

#[cfg(test)]
pub(crate) fn test_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("ps5upload-remote-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[cfg(test)]
pub(crate) fn conn(name: &str, protocol: Protocol) -> Connection {
    Connection {
        id: String::new(),
        name: name.into(),
        port: default_port(&protocol),
        protocol,
        host: "10.0.0.5".into(),
        share: "games".into(),
        user: "me".into(),
        start_path: String::new(),
        host_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pw(p: &str) -> Secret {
        Secret::Password { password: p.into() }
    }

    #[test]
    fn stores_connections_and_keeps_secrets_encrypted() {
        let dir = test_dir();
        let s = Store::open(&dir).unwrap();
        let v = s.add(conn("NAS", Protocol::Smb), pw("hunter2")).unwrap();
        assert!(v.has_secret);
        assert!(!v.conn.id.is_empty());
        let raw = std::fs::read_to_string(dir.join("connections.json")).unwrap();
        assert!(
            !raw.contains("hunter2"),
            "secret must not be stored in the clear"
        );
        assert!(raw.contains("NAS"));
        let s2 = Store::open(&dir).unwrap();
        assert_eq!(s2.get(&v.conn.id).unwrap().1, pw("hunter2"));
    }

    #[test]
    fn an_edit_without_a_secret_keeps_the_old_one() {
        let s = Store::open(&test_dir()).unwrap();
        let v = s.add(conn("NAS", Protocol::Smb), pw("pw")).unwrap();
        let mut c = v.conn.clone();
        c.name = "Home NAS".into();
        s.update(&c.id, c.clone(), None).unwrap();
        assert_eq!(s.get(&c.id).unwrap().1, pw("pw"));
        assert_eq!(s.list()[0].conn.name, "Home NAS");
        s.update(&c.id, c.clone(), Some(Secret::None)).unwrap();
        assert!(!s.list()[0].has_secret);
    }

    #[test]
    fn remove_and_host_key() {
        let s = Store::open(&test_dir()).unwrap();
        let a = s.add(conn("A", Protocol::Sftp), Secret::None).unwrap();
        let b = s.add(conn("B", Protocol::Ftp), Secret::None).unwrap();
        assert_ne!(a.conn.id, b.conn.id);
        s.set_host_key(&a.conn.id, "SHA256:abc").unwrap();
        assert_eq!(
            s.get(&a.conn.id).unwrap().0.host_key.as_deref(),
            Some("SHA256:abc")
        );
        s.remove(&a.conn.id).unwrap();
        assert!(s.get(&a.conn.id).is_none());
        assert_eq!(s.list().len(), 1);
        assert!(s.update("nope", conn("x", Protocol::Ftp), None).is_err());
    }

    #[test]
    fn a_wrong_key_cannot_read_the_secrets() {
        let dir = test_dir();
        let s = Store::open(&dir).unwrap();
        let v = s.add(conn("NAS", Protocol::Smb), pw("hunter2")).unwrap();
        std::fs::write(dir.join("connections.key"), [7u8; 32]).unwrap();
        let s2 = Store::open(&dir).unwrap();
        // The connection survives; its secret is gone rather than garbage.
        assert_eq!(s2.get(&v.conn.id).unwrap().1, Secret::None);
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir();
        Store::open(&dir).unwrap();
        let mode = std::fs::metadata(dir.join("connections.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
