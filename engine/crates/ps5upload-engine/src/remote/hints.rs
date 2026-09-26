//! Plain-language hints for the errors a person setting up a server actually meets.

pub fn hint_for(error: &str) -> Option<&'static str> {
    let e = error.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| e.contains(n));
    if has(&["0xc0000072", "account_disabled", "account disabled"]) {
        Some("The share's Guest account is disabled. Enable it on the server, or sign in with a user and password.")
    } else if has(&["0xc000015b", "logon_type_not_granted", "logon type"]) {
        Some("The server does not allow this account to sign in over the network. Check its network logon policy.")
    } else if has(&[
        "0xc0000022",
        "access denied",
        "access_denied",
        "accessdenied",
    ]) {
        Some("Access denied. Check the share's permissions and the folder's security permissions for this user.")
    } else if has(&["signing"]) {
        Some("The server requires SMB signing, which a guest cannot use. Sign in with a user and password.")
    } else if has(&["certificate", "tls", "handshake"]) {
        Some("The server's TLS certificate was not accepted.")
    } else if has(&[
        "refused",
        "timed out",
        "timeout",
        "unreachable",
        "no route",
        "can't reach",
        "failed to lookup",
        "resolve",
    ]) {
        Some("Check the server is on and this computer is on the same network.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_status_codes_become_plain_hints() {
        assert!(hint_for("STATUS_ACCOUNT_DISABLED (0xC0000072)")
            .unwrap()
            .contains("Guest account"));
        assert!(hint_for("status 0xc000015b")
            .unwrap()
            .contains("network logon"));
        assert!(hint_for("access denied 0xC0000022")
            .unwrap()
            .contains("permissions"));
        assert!(hint_for("the server requires signing")
            .unwrap()
            .contains("signing"));
        assert!(hint_for("connection refused")
            .unwrap()
            .contains("same network"));
        assert!(hint_for("Can't reach 10.0.0.5:445: timed out")
            .unwrap()
            .contains("same network"));
        assert!(hint_for("invalid peer certificate")
            .unwrap()
            .contains("certificate"));
        assert_eq!(hint_for("something else"), None);
    }
}
