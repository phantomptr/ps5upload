//! Failures the renderer words itself, in the user's language (`client/src/lib/shellError.ts`).
//!
//! A Tauri command's error is one string, and an English sentence built here reached the screen
//! as it was in all 21 languages. So a failure the user reads travels as a small JSON object:
//! a stable `code` (which picks the sentence) and the facts that go in it (`status`, `detail`).
//! The renderer's table turns it into text; the log and a bug report keep the object, which
//! says more than the sentence does. A string that is not such an object is shown as it is.

/// The error string for `code`, carrying `fields` (a JSON object, or null for none).
pub(crate) fn coded(code: &str, fields: serde_json::Value) -> String {
    let mut object = match fields {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    object.insert("code".into(), code.into());
    serde_json::Value::Object(object).to_string()
}

/// `code` with no facts to carry.
pub(crate) fn code_only(code: &str) -> String {
    coded(code, serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coded_error_is_json_the_renderer_can_read() {
        let text = coded(
            "update_http",
            serde_json::json!({ "status": 403, "url": "https://x/\"y\"" }),
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["code"], "update_http");
        assert_eq!(v["status"], 403);
        assert_eq!(v["url"], "https://x/\"y\"");
    }

    #[test]
    fn a_code_alone_has_only_its_code() {
        let v: serde_json::Value = serde_json::from_str(&code_only("update_no_bundle")).unwrap();
        assert_eq!(v, serde_json::json!({ "code": "update_no_bundle" }));
    }
}
