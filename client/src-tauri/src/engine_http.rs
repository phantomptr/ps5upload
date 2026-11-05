//! HTTP client pieces shared by the desktop sidecar (`engine.rs`) and the
//! mobile in-process engine (`engine_mobile.rs`): the command proxies in
//! `commands/` call these whichever of the two is compiled in.

/// HTTP client builder for every request to the engine.
///
/// The engine is on this machine (or a LAN host the user typed in) and is
/// never behind a proxy, but reqwest honours `http_proxy` / `ALL_PROXY` from
/// the environment and does NOT exempt 127.0.0.1. With such a variable set
/// (a VPN/proxy tool exports one, or the app was launched from a shell that
/// has one) every engine request went to the proxy and failed while the
/// engine sat idle — "engine did not become ready" on a healthy engine, with
/// not one request in its log. Reproduced on reqwest 0.12.28: a dead
/// `http_proxy` gives `Connection refused`, `ALL_PROXY=socks5://…` gives
/// `unsupported scheme socks5`. Internet fetches (updates, payload
/// downloads, artwork) build their own clients and keep honouring the proxy.
pub fn engine_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_proxy()
}

/// The text of an engine error body for a message. The engine sends what used to be a plain-text
/// refusal (a malformed request, a guard) as `{"error": .., "code": ..}`; that reads as its text,
/// as it always did. Any other body stays as it is: the renderer matches tokens in it.
pub fn plain_error_text(body: &str) -> String {
    if let Ok(serde_json::Value::Object(o)) = serde_json::from_str(body) {
        if o.len() == 2 && o.get("code").is_some_and(|c| c.is_string()) {
            if let Some(text) = o.get("error").and_then(|e| e.as_str()) {
                return text.to_string();
            }
        }
    }
    body.to_string()
}

/// `e` followed by each underlying cause. reqwest's Display stops at "error
/// sending request for url (…)", which hides whether the connection was
/// refused, timed out, or was sent to a proxy — the one detail a bug report
/// about an unreachable engine needs.
pub fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::plain_error_text;

    #[test]
    fn a_normalized_refusal_reads_as_its_text_and_anything_else_stays_raw() {
        assert_eq!(
            plain_error_text(r#"{"error":"Missing field `path`","code":"unprocessable_entity"}"#),
            "Missing field `path`"
        );
        for raw in [
            "plain text",
            "",
            r#"{"error":"x"}"#,
            r#"{"error":"x","reason":"console_rest_mode"}"#,
            r#"{"error":"x","code":"a","reason":"b"}"#,
        ] {
            assert_eq!(plain_error_text(raw), raw);
        }
    }
}
