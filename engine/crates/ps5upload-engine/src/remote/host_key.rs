//! Trust on first use, for SFTP host keys and FTPS certificates alike: the first connection
//! shows the server's fingerprint for the user to accept; after that only that key is trusted.

use super::RemoteError;

/// Proceed only when `presented` is the accepted fingerprint.
pub fn check_host_key(stored: Option<&str>, presented: &str) -> Result<(), RemoteError> {
    match stored {
        Some(s) if s == presented => Ok(()),
        Some(_) => Err(RemoteError::HostKey {
            fingerprint: presented.to_string(),
            changed: true,
        }),
        None => Err(RemoteError::HostKey {
            fingerprint: presented.to_string(),
            changed: false,
        }),
    }
}

/// "SHA256:<unpadded base64>" of `bytes` (a certificate or public key), as OpenSSH prints it.
pub fn fingerprint(bytes: &[u8]) -> String {
    use base64::Engine;
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusts_only_the_accepted_key() {
        assert!(matches!(
            check_host_key(None, "SHA256:abc"),
            Err(RemoteError::HostKey { changed: false, ref fingerprint }) if fingerprint == "SHA256:abc"
        ));
        assert!(check_host_key(Some("SHA256:abc"), "SHA256:abc").is_ok());
        assert!(matches!(
            check_host_key(Some("SHA256:old"), "SHA256:new"),
            Err(RemoteError::HostKey { changed: true, ref fingerprint }) if fingerprint == "SHA256:new"
        ));
    }

    #[test]
    fn fingerprints_like_openssh() {
        // sha256("") = e3b0c442…; base64 without padding.
        assert_eq!(
            fingerprint(b""),
            "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU"
        );
    }
}
