//! Every route the engine serves, by method and path: what `ps5upload://catalog` answers, and what
//! `api_request` can reach (the routes under `/api/`). A test below keeps this the same as the
//! router: a route added without being listed here fails it, and so does one removed.
//!
//! The description of each route's fields belongs to the OpenAPI catalog; this is the list an
//! agent needs to know what to ask for.

use serde_json::{json, Value};

/// Routes that hand out the keys that sign in to a console's user (Remote Play and wake): the app's
/// own. They are not in [`ROUTES`], and `api_request` refuses them: a model's context, its logs and
/// whoever reads them are no place for those keys. The curated tools use `/api/consoles`, which says
/// only whether a console has keys.
pub(crate) const HOLD_CONSOLE_KEYS: &[&str] = &[
    "/api/consoles/document",
    "/api/settings-backup/get",
    // Any helper call, by number and with a base64 body: every document, PSN sign-in files.
    "/api/mgmt/call",
    // Mints a launch token each time it is asked. No tool has a use for one.
    "/api/ava1/identity",
];

/// Fields that are a key wherever they appear in an answer.
const KEY_FIELDS: &[&str] = &[
    "credential",
    "regist_key",
    "rp_key",
    "wake_credential",
    "wake_regist_key",
    "wake_rp_key",
    "launch_token",
];

/// Where an answer reports a failure in words, which can quote the URL or request it failed on.
const REPORT_FIELDS: &[&str] = &[
    "error",
    "error_detail",
    "error_reason",
    "detail",
    "message",
    "msg",
    "reason",
];

/// Takes every key out of an answer. Pairing still works and stores them: the route has done its
/// job by now, this is only what a tool gets to see. A drive's password hash (`lock`) goes too,
/// since it can be cracked offline, leaving that it has one. A key in a route's own words, such as
/// a finished `/api/ops` body, goes the same way. Returns whether anything went.
pub(crate) fn redact_keys(value: &mut Value) -> bool {
    let mut any = false;
    match value {
        Value::Object(map) => {
            for field in KEY_FIELDS {
                any |= map.remove(*field).is_some();
            }
            if map.get("lock").is_some_and(|l| l.get("hash").is_some()) {
                map.remove("lock");
                map.insert("has_password".to_string(), json!(true));
                any = true;
            }
            for (name, v) in map.iter_mut() {
                if REPORT_FIELDS.contains(&name.as_str()) {
                    any |= ps5upload_core::secret_text::value_without_secrets(v);
                } else {
                    any |= redact_keys(v);
                }
            }
        }
        Value::Array(items) => {
            for v in items {
                any |= redact_keys(v);
            }
        }
        _ => {}
    }
    any
}

/// A tailnet's sign-in link (`login_url`, while a node waits for a sign-in): whoever opens it
/// first signs that node into *their* tailnet. An agent that read it could put this computer, or
/// the PS5, on a tailnet it controls and reach the console from there. So it never reaches an
/// agent, whichever route answered with it (`/api/tailnet`, `/api/ps5/tailnet`, GET or POST); the
/// app's own HTTP calls go around the MCP server and still get it to show the link and QR code.
const SIGN_IN_FIELDS: &[&str] = &["login_url"];

/// Fields of a live invite that let someone in: Play Together's invite key, short code and links,
/// and the phone link's QR address (`#t=<key>`, which lets a phone on the LAN in without the
/// host's Allow) and internet link. Taken out of the passive reads (`GET /api/join`, `GET
/// /api/ps5/rp/pad-link`: `together_status`, `api_request`), which an agent that may only look can
/// call too. Making an invite (`together_invite`, a POST) still answers with its link: sharing it
/// is what the user asked the agent for.
const INVITE_FIELDS: &[&str] = &["key", "k", "code", "code_display", "url", "urls"];

/// The passive reads whose answers hold live invites ([`INVITE_FIELDS`]).
const INVITE_READS: &[&str] = &["/api/join", "/api/ps5/rp/pad-link"];

/// What a Share link is made of: the link itself, the home network's, and the room it names
/// (share/mod.rs: whoever has the room can ask the sender). Taken out of every read of
/// `/api/share` (`share_status`, `api_request`), the receiving side's too; making a share
/// (`share_create`, a POST) answers with its link, which is what the user asked for.
const SHARE_LINK_FIELDS: &[&str] = &["url", "lan_url", "room", "link"];

/// A read of `/api/share` or anything below it.
fn is_share_read(method: &str, plain: &str) -> bool {
    method.eq_ignore_ascii_case("GET")
        && (plain == "/api/share" || plain.starts_with("/api/share/"))
}

/// Whether the answer to `method path` may hold something [`redact_for_agents`] takes out, so
/// only those answers are parsed.
pub(crate) fn may_hold_agent_secrets(method: &str, path: &str) -> bool {
    let plain = plain_path(path);
    plain.contains("tailnet")
        || (method.eq_ignore_ascii_case("GET") && INVITE_READS.contains(&plain.as_str()))
        || is_share_read(method, &plain)
}

/// What an agent may not read in an in-process answer (`McpState::api_call`): a tailnet sign-in
/// link anywhere ([`SIGN_IN_FIELDS`]), and in the passive invite reads the invite's key, code and
/// links ([`INVITE_FIELDS`]; `invite.hidden` and `link_hidden` say they were there). What is left
/// says whether it is open, when it expires, and who is in or asking. Returns whether anything
/// went.
pub(crate) fn redact_for_agents(method: &str, path: &str, value: &mut Value) -> bool {
    fn strip(value: &mut Value, fields: &[&str]) -> bool {
        let mut any = false;
        match value {
            Value::Object(map) => {
                for f in fields {
                    any |= map.remove(*f).is_some();
                }
                for v in map.values_mut() {
                    any |= strip(v, fields);
                }
            }
            Value::Array(items) => {
                for v in items {
                    any |= strip(v, fields);
                }
            }
            _ => {}
        }
        any
    }
    let mut any = strip(value, SIGN_IN_FIELDS);
    let plain = plain_path(path);
    if is_share_read(method, &plain) && strip(value, SHARE_LINK_FIELDS) {
        if let Value::Object(map) = value {
            map.insert("link_hidden".to_string(), json!(true));
        }
        any = true;
    }
    if method.eq_ignore_ascii_case("GET") && INVITE_READS.contains(&plain.as_str()) {
        let Value::Object(map) = value else {
            return any;
        };
        if plain == "/api/join" {
            // Play Together: the invite and its internet link. The guests' list keeps no key.
            if let Some(inv) = map.get_mut("invite").filter(|v| v.is_object()) {
                if strip(inv, INVITE_FIELDS) {
                    inv["hidden"] = json!(true);
                    any = true;
                }
            }
        } else {
            // The phone link: its QR addresses at the top, and the internet link.
            let mut gone = false;
            for f in ["url", "urls"] {
                gone |= map.remove(f).is_some();
            }
            if let Some(net) = map.get_mut("internet") {
                gone |= strip(net, INVITE_FIELDS);
            }
            if gone {
                map.insert("link_hidden".to_string(), json!(true));
                any = true;
            }
        }
    }
    any
}

/// Percent-escapes undone and letters lowered, `.` and repeated or trailing slashes gone.
fn plain_path(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(b) => {
                decoded.push(b);
                i += 3;
            }
            None => {
                decoded.push(bytes[i]);
                i += 1;
            }
        }
    }
    let text = String::from_utf8_lossy(&decoded).to_ascii_lowercase();
    let mut segments: Vec<&str> = Vec::new();
    for segment in text.trim().split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            name => segments.push(name),
        }
    }
    format!("/{}", segments.join("/"))
}

/// Every string in a request's JSON.
fn any_text(value: &Value, found: &dyn Fn(&str) -> bool) -> bool {
    match value {
        Value::String(text) => found(text),
        Value::Array(items) => items.iter().any(|v| any_text(v, found)),
        Value::Object(map) => map.values().any(|v| any_text(v, found)),
        _ => false,
    }
}

/// On the console, what no tool reads: the copy of each user's PSN sign-in files
/// (payload/include/profile.h), and the helper's AVA1 identity and saved shares' key, which a
/// helper of this version refuses by itself (payload/src/path_policy.c). By the part below the
/// drive, since `/user/data` is `/data` under another name.
const KEY_FOLDERS: &[&str] = &["/ps5upload/np-backup", "/ps5upload/ava", "/ps5upload/smb"];

/// A console document that holds keys: its backup slots (that console's), `state` and `consoles`
/// (every console on the roster), `drives` (Mesh Drive password hashes).
fn is_key_document(plain_name: &str) -> bool {
    let name = plain_name.trim_start_matches('/');
    name.starts_with("backup-") || matches!(name, "state" | "consoles" | "drives")
}

/// This call reaches a console document that holds keys, by its name or by the helper's file for
/// it. The `backups` index and the other documents stay readable: through `/api/mesh/doc` only the
/// name counts, elsewhere only the file path.
pub(crate) fn reaches_a_key_document(
    path: &str,
    query: &[(&str, String)],
    body: Option<&Value>,
) -> bool {
    if plain_path(path) == "/api/mesh/doc" {
        let doc = |name: &str| is_key_document(&plain_path(name));
        return query.iter().any(|(k, v)| *k == "name" && doc(v))
            || body
                .and_then(|b| b.get("name"))
                .and_then(Value::as_str)
                .is_some_and(doc);
    }
    // A shell reads anything on the console, whatever its command says: MCP.md says so, and the
    // tool asks for the user's confirmation. Refusing one spelling would only look like a guard.
    if plain_path(path) == "/api/ps5/shell/run" {
        return false;
    }
    let file = |text: &str| {
        let plain = plain_path(text);
        plain
            .rsplit_once("/mesh/v1/doc-")
            .is_some_and(|(_, name)| is_key_document(name))
            || KEY_FOLDERS
                .iter()
                .any(|d| plain.ends_with(d) || plain.contains(&format!("{d}/")))
    };
    query.iter().any(|(_, v)| file(v)) || body.is_some_and(|b| any_text(b, &file))
}

/// `path` names one of [`HOLD_CONSOLE_KEYS`], however it is written: percent-escapes, case, repeated
/// or trailing slashes.
pub(crate) fn holds_console_keys(path: &str) -> bool {
    HOLD_CONSOLE_KEYS.contains(&plain_path(path).as_str())
}

/// `path` names the switch that decides whether agents can change things (`access.rs`), however it
/// is written: percent-escapes, case, repeated or trailing slashes, `.` and `..`.
pub(crate) fn reaches_agent_access(path: &str) -> bool {
    let plain = plain_path(path);
    let route = super::access::ROUTE;
    plain == route
        || plain
            .strip_prefix(route)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `(method, path)`, sorted by path, then method.
pub(crate) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/"),
    ("POST", "/api/7z/inspect"),
    ("POST", "/api/7z/inspect/stream"),
    ("GET", "/api/activity"),
    ("POST", "/api/activity/label"),
    ("DELETE", "/api/ampr/profile"),
    ("GET", "/api/ampr/profile"),
    ("GET", "/api/ampr/trace"),
    ("POST", "/api/ampr/trace/collect"),
    ("POST", "/api/ampr/trace/generate"),
    ("POST", "/api/ampr/trace/start"),
    ("POST", "/api/ampr/trace/stop"),
    ("GET", "/api/ava1/console"),
    ("GET", "/api/ava1/pairing"),
    ("POST", "/api/ava1/pairing/allow"),
    ("POST", "/api/ava1/pairing/cancel"),
    ("POST", "/api/ava1/pairing/confirm"),
    ("POST", "/api/ava1/pairing/forget"),
    ("POST", "/api/ava1/pairing/start"),
    ("POST", "/api/ava1/same-console"),
    ("GET", "/api/backport/pack"),
    ("POST", "/api/backport/pack/import"),
    ("POST", "/api/bug-report/bundle"),
    ("DELETE", "/api/cache/artwork"),
    ("GET", "/api/cache/artwork"),
    ("GET", "/api/collection/check"),
    ("POST", "/api/collection/check"),
    ("POST", "/api/collection/check/cancel"),
    ("GET", "/api/collection/console"),
    ("GET", "/api/collection/export"),
    ("GET", "/api/collection/games/{id}"),
    ("GET", "/api/collection/games/{id}/cover"),
    ("GET", "/api/collection/junk"),
    ("POST", "/api/collection/junk/cancel"),
    ("POST", "/api/collection/junk/clean"),
    ("POST", "/api/collection/junk/scan"),
    ("GET", "/api/collection/library"),
    ("GET", "/api/collection/macos"),
    ("PUT", "/api/collection/macos/finder"),
    ("POST", "/api/collection/macos/spotlight"),
    ("POST", "/api/collection/organize/apply"),
    ("POST", "/api/collection/organize/plan"),
    ("POST", "/api/collection/organize/plan/cancel"),
    ("POST", "/api/collection/organize/plan/start"),
    ("GET", "/api/collection/organize/plan/status"),
    ("POST", "/api/collection/organize/revert"),
    ("GET", "/api/collection/organize/runs"),
    ("GET", "/api/collection/scan"),
    ("POST", "/api/collection/scan"),
    ("POST", "/api/collection/scan/cancel"),
    ("GET", "/api/collection/settings"),
    ("PUT", "/api/collection/settings"),
    ("POST", "/api/collection/trash/apply"),
    ("POST", "/api/collection/trash/preview"),
    ("POST", "/api/console-backup/backup"),
    ("POST", "/api/console-backup/cancel"),
    ("POST", "/api/console-backup/inspect"),
    ("POST", "/api/console-backup/restore"),
    ("POST", "/api/console-backup/scan"),
    ("GET", "/api/console-backup/status"),
    ("POST", "/api/console-snapshots/forget"),
    ("POST", "/api/console-snapshots/keep"),
    ("GET", "/api/consoles"),
    ("PUT", "/api/consoles"),
    ("PATCH", "/api/consoles/settings"),
    ("GET", "/api/debug/crash"),
    ("GET", "/api/diag/features"),
    ("GET", "/api/diag/log-level"),
    ("POST", "/api/diag/log-level"),
    ("POST", "/api/discover"),
    ("GET", "/api/drive"),
    ("POST", "/api/drive/approve"),
    ("POST", "/api/drive/check"),
    ("POST", "/api/drive/confirm"),
    ("GET", "/api/drive/conflicts"),
    ("POST", "/api/drive/create"),
    ("POST", "/api/drive/delete"),
    ("POST", "/api/drive/join"),
    ("POST", "/api/drive/leave"),
    ("POST", "/api/drive/link"),
    ("GET", "/api/drive/local"),
    ("POST", "/api/drive/password"),
    ("POST", "/api/drive/pause"),
    ("POST", "/api/drive/resolve"),
    ("POST", "/api/drive/sync"),
    ("POST", "/api/drive/unlink"),
    ("GET", "/api/engine-logs"),
    ("GET", "/api/engine/update"),
    ("GET", "/api/event-journal"),
    ("GET", "/api/events"),
    ("POST", "/api/exfat/build"),
    ("GET", "/api/fakelibs/corpus"),
    ("POST", "/api/fakelibs/import"),
    ("POST", "/api/fakelibs/scan"),
    ("GET", "/api/fakelibs/scan/{id}"),
    ("DELETE", "/api/fakelibs/set/{id}"),
    ("POST", "/api/ffpkg/extract"),
    ("POST", "/api/ffpkg/inspect"),
    ("POST", "/api/files/upload"),
    ("DELETE", "/api/files/upload/{id}"),
    ("POST", "/api/folder/build"),
    ("POST", "/api/fpkg/ampr-emu"),
    ("POST", "/api/fpkg/ampr-index"),
    ("POST", "/api/fpkg/build"),
    ("POST", "/api/fpkg/delete"),
    ("POST", "/api/fpkg/estimate"),
    ("POST", "/api/fpkg/extract"),
    ("POST", "/api/fpkg/extract/cleanup"),
    ("POST", "/api/fpkg/inspect"),
    ("POST", "/api/game/inspect"),
    ("GET", "/api/game/inspect/files"),
    ("GET", "/api/game/inspect/image"),
    ("GET", "/api/games/installed"),
    ("GET", "/api/games/{id}"),
    ("POST", "/api/games/{id}/refresh"),
    ("POST", "/api/host-net/allow-firewall"),
    ("POST", "/api/host-net/open-settings"),
    ("POST", "/api/image/browse"),
    ("GET", "/api/jobs"),
    ("GET", "/api/jobs/summaries"),
    ("GET", "/api/jobs/{id}"),
    ("POST", "/api/jobs/{id}/cancel"),
    ("GET", "/api/jobs/{id}/files"),
    ("POST", "/api/jobs/{id}/pause"),
    ("POST", "/api/jobs/{id}/resume"),
    ("GET", "/api/jobs/{id}/summary"),
    ("GET", "/api/join"),
    ("POST", "/api/join/close"),
    ("POST", "/api/join/decide"),
    ("POST", "/api/join/invite"),
    ("POST", "/api/join/kick"),
    ("POST", "/api/join/open"),
    ("POST", "/api/join/playing"),
    ("GET", "/api/join/profiles"),
    ("GET", "/api/join/relay"),
    ("POST", "/api/join/settings"),
    ("GET", "/api/join/signal"),
    ("GET", "/api/join/ws"),
    ("POST", "/api/link/download"),
    ("POST", "/api/link/probe"),
    ("GET", "/api/local/game-icon"),
    ("POST", "/api/local/image/attach"),
    ("POST", "/api/local/image/detach"),
    ("GET", "/api/local/image/status"),
    ("GET", "/api/local/inspect-folder"),
    ("GET", "/api/local/list-dir"),
    ("GET", "/api/local/path-kind"),
    ("GET", "/api/local/storage-roots"),
    ("GET", "/api/logs/level"),
    ("PUT", "/api/logs/level"),
    ("GET", "/api/logs/recent"),
    ("POST", "/api/mesh/activity"),
    ("POST", "/api/mesh/announce"),
    ("POST", "/api/mesh/device"),
    ("GET", "/api/mesh/doc"),
    ("POST", "/api/mesh/doc"),
    ("POST", "/api/mesh/lock"),
    ("GET", "/api/mesh/me"),
    ("POST", "/api/mesh/me"),
    ("POST", "/api/mesh/settings"),
    ("GET", "/api/mesh/state"),
    ("GET", "/api/metrics"),
    ("GET", "/api/net/doctor"),
    ("GET", "/api/net/doctor/reply"),
    ("GET", "/api/openapi.json"),
    ("GET", "/api/ops"),
    ("GET", "/api/ops/{id}"),
    ("POST", "/api/ops/{id}/cancel"),
    ("GET", "/api/pad/ws"),
    ("POST", "/api/payload/send"),
    ("POST", "/api/payload/send-check"),
    ("GET", "/api/payloads"),
    ("GET", "/api/payloads/pm-sync"),
    ("PUT", "/api/payloads/pm-sync"),
    ("POST", "/api/payloads/pm-sync/run"),
    ("POST", "/api/payloads/repos"),
    ("DELETE", "/api/payloads/repos/{id}"),
    ("GET", "/api/payloads/updates"),
    ("POST", "/api/payloads/updates/apply"),
    ("POST", "/api/payloads/updates/check"),
    ("GET", "/api/payloads/updates/settings"),
    ("PUT", "/api/payloads/updates/settings"),
    ("POST", "/api/payloads/{id}/download"),
    ("POST", "/api/payloads/{id}/download/cancel"),
    ("GET", "/api/payloads/{id}/releases"),
    ("GET", "/api/payloads/{id}/releases/latest"),
    ("POST", "/api/payloads/{id}/rollback"),
    ("POST", "/api/pkg/console-probe"),
    ("POST", "/api/pkg/install"),
    ("GET", "/api/pkg/install/history"),
    ("GET", "/api/pkg/install/preflight"),
    ("GET", "/api/pkg/install/sessions"),
    ("GET", "/api/pkg/install/status"),
    ("POST", "/api/pkg/install/stop"),
    ("GET", "/api/pkg/installed"),
    ("GET", "/api/pkg/links"),
    ("POST", "/api/pkg/links"),
    ("POST", "/api/pkg/links/open"),
    ("POST", "/api/pkg/links/stop"),
    ("POST", "/api/pkg/parse-split"),
    ("POST", "/api/pkg/payload-restore"),
    ("POST", "/api/pkg/remote/download/cancel"),
    ("POST", "/api/pkg/remote/download/start"),
    ("GET", "/api/pkg/remote/download/status"),
    ("POST", "/api/pkg/remote/probe"),
    ("POST", "/api/pkg/upload"),
    ("DELETE", "/api/pkg/upload/{id}"),
    ("POST", "/api/profile/activate"),
    ("POST", "/api/profile/avatar"),
    ("GET", "/api/profile/avatar/current"),
    ("POST", "/api/profile/avatar/preview"),
    ("GET", "/api/profile/info"),
    ("POST", "/api/profile/local-username"),
    ("POST", "/api/profile/np/backup"),
    ("POST", "/api/profile/np/restore"),
    ("POST", "/api/profile/np/state"),
    ("POST", "/api/profile/username"),
    ("GET", "/api/ps5/activity/db-query"),
    ("GET", "/api/ps5/activity/get"),
    ("POST", "/api/ps5/activity/reset"),
    ("GET", "/api/ps5/app-icon"),
    ("POST", "/api/ps5/app/launch"),
    ("POST", "/api/ps5/app/lifecycle"),
    ("POST", "/api/ps5/app/register"),
    ("POST", "/api/ps5/app/unregister"),
    ("GET", "/api/ps5/appdb"),
    ("GET", "/api/ps5/appinfo"),
    ("POST", "/api/ps5/appmeta/heal"),
    ("GET", "/api/ps5/apps/hidden"),
    ("POST", "/api/ps5/apps/hidden"),
    ("GET", "/api/ps5/apps/installed"),
    ("POST", "/api/ps5/backup/delete"),
    ("GET", "/api/ps5/backup/list"),
    ("POST", "/api/ps5/backup/restore"),
    ("POST", "/api/ps5/backup/snapshot"),
    ("POST", "/api/ps5/cheats/delete"),
    ("POST", "/api/ps5/cheats/engine-set"),
    ("GET", "/api/ps5/cheats/get"),
    ("POST", "/api/ps5/cheats/import"),
    ("GET", "/api/ps5/cheats/list"),
    ("POST", "/api/ps5/cheats/reload"),
    ("POST", "/api/ps5/cheats/repos/download"),
    ("GET", "/api/ps5/cheats/repos/list"),
    ("GET", "/api/ps5/cheats/repos/search"),
    ("GET", "/api/ps5/cheats/status"),
    ("POST", "/api/ps5/cheats/toggle"),
    ("POST", "/api/ps5/cleanup"),
    ("GET", "/api/ps5/cleanup/apply/{id}"),
    ("POST", "/api/ps5/cleanup/cancel"),
    ("POST", "/api/ps5/cleanup/scan"),
    ("GET", "/api/ps5/cleanup/scan/{id}"),
    ("POST", "/api/ps5/cleanup/scan/{id}/apply"),
    ("GET", "/api/ps5/companions"),
    ("GET", "/api/ps5/connect-timings"),
    ("GET", "/api/ps5/console-shares"),
    ("POST", "/api/ps5/console-shares/remove"),
    ("POST", "/api/ps5/console-shares/save"),
    ("GET", "/api/ps5/console-tasks"),
    ("POST", "/api/ps5/console-tasks/cancel"),
    ("POST", "/api/ps5/disk-usage"),
    ("GET", "/api/ps5/disk-usage/{id}"),
    ("POST", "/api/ps5/disk-usage/{id}/cancel"),
    ("POST", "/api/ps5/dump"),
    ("POST", "/api/ps5/elfldr/ensure"),
    ("GET", "/api/ps5/elfldr/health"),
    ("GET", "/api/ps5/fan-boost"),
    ("POST", "/api/ps5/fan-boost"),
    ("POST", "/api/ps5/files/handoff/put"),
    ("POST", "/api/ps5/files/handoff/take"),
    ("GET", "/api/ps5/files/prefs"),
    ("POST", "/api/ps5/files/prefs/set"),
    ("GET", "/api/ps5/focus"),
    ("POST", "/api/ps5/fs/chmod"),
    ("POST", "/api/ps5/fs/copy"),
    ("POST", "/api/ps5/fs/delete"),
    ("POST", "/api/ps5/fs/extract"),
    ("POST", "/api/ps5/fs/hash"),
    ("POST", "/api/ps5/fs/mkdir"),
    ("POST", "/api/ps5/fs/mount"),
    ("POST", "/api/ps5/fs/move"),
    ("POST", "/api/ps5/fs/op-cancel"),
    ("GET", "/api/ps5/fs/op-status"),
    ("POST", "/api/ps5/fs/read-preview"),
    ("POST", "/api/ps5/fs/stat"),
    ("POST", "/api/ps5/fs/unmount"),
    ("POST", "/api/ps5/fs/unpack"),
    ("POST", "/api/ps5/fs/write-bytes"),
    ("GET", "/api/ps5/fw-spoof/status"),
    ("GET", "/api/ps5/game-icon"),
    ("GET", "/api/ps5/game-meta"),
    ("POST", "/api/ps5/health/fix"),
    ("GET", "/api/ps5/health/junk"),
    ("GET", "/api/ps5/health/scan"),
    ("GET", "/api/ps5/helper-log-ftp"),
    ("POST", "/api/ps5/helper/replace-copy"),
    ("POST", "/api/ps5/helper/restore-previous"),
    ("POST", "/api/ps5/helper/stop"),
    ("POST", "/api/ps5/helper/store-in-pldmgr"),
    ("GET", "/api/ps5/hw/drive-sensors"),
    ("POST", "/api/ps5/hw/fan-threshold"),
    ("GET", "/api/ps5/hw/fps"),
    ("GET", "/api/ps5/hw/fps/capture"),
    ("GET", "/api/ps5/hw/fps/capture/csv"),
    ("POST", "/api/ps5/hw/fps/capture/start"),
    ("POST", "/api/ps5/hw/fps/capture/stop"),
    ("GET", "/api/ps5/hw/info"),
    ("GET", "/api/ps5/hw/power"),
    ("GET", "/api/ps5/hw/storage"),
    ("GET", "/api/ps5/hw/temps"),
    ("GET", "/api/ps5/klog"),
    ("GET", "/api/ps5/list-dir"),
    ("POST", "/api/ps5/minion/tile"),
    ("GET", "/api/ps5/net/interfaces"),
    ("GET", "/api/ps5/net/ports"),
    ("POST", "/api/ps5/notif/clear"),
    ("GET", "/api/ps5/notif/list"),
    ("POST", "/api/ps5/payload/load-console"),
    ("POST", "/api/ps5/peripheral"),
    ("GET", "/api/ps5/pkg/metadata"),
    ("GET", "/api/ps5/pkg/scan-external"),
    ("GET", "/api/ps5/pldmgr"),
    ("POST", "/api/ps5/pldmgr/autoload"),
    ("POST", "/api/ps5/pldmgr/delete"),
    ("POST", "/api/ps5/pldmgr/launch"),
    ("POST", "/api/ps5/pldmgr/store"),
    ("POST", "/api/ps5/pldmgr/upload"),
    ("GET", "/api/ps5/port-check"),
    ("POST", "/api/ps5/power/control"),
    ("GET", "/api/ps5/power/ddp-status"),
    ("POST", "/api/ps5/power/pair"),
    ("GET", "/api/ps5/power/telemetry"),
    ("POST", "/api/ps5/power/wake"),
    ("POST", "/api/ps5/power/wake-login"),
    ("POST", "/api/ps5/power/wake-login/passcode"),
    ("GET", "/api/ps5/proc/list"),
    ("POST", "/api/ps5/process/kill"),
    ("GET", "/api/ps5/process/list"),
    ("GET", "/api/ps5/process/{pid}/modules"),
    ("GET", "/api/ps5/readiness"),
    ("POST", "/api/ps5/remoteplay/cancel"),
    ("POST", "/api/ps5/remoteplay/enable"),
    ("GET", "/api/ps5/remoteplay/readiness"),
    ("POST", "/api/ps5/remoteplay/request"),
    ("GET", "/api/ps5/remoteplay/status"),
    ("GET", "/api/ps5/rp/controller"),
    ("GET", "/api/ps5/rp/frame"),
    ("POST", "/api/ps5/rp/input"),
    ("POST", "/api/ps5/rp/input/release"),
    ("POST", "/api/ps5/rp/keyboard"),
    ("GET", "/api/ps5/rp/live.ts"),
    ("GET", "/api/ps5/rp/live/status"),
    ("POST", "/api/ps5/rp/mic"),
    ("GET", "/api/ps5/rp/pad-link"),
    ("POST", "/api/ps5/rp/pad-link"),
    ("POST", "/api/ps5/rp/pad-link/control"),
    ("POST", "/api/ps5/rp/pad-link/decide"),
    ("POST", "/api/ps5/rp/pad-link/internet"),
    ("POST", "/api/ps5/rp/pad-link/kick"),
    ("GET", "/api/ps5/rp/pad-relay"),
    ("GET", "/api/ps5/rp/pair/attempts"),
    ("POST", "/api/ps5/rp/passcode"),
    ("POST", "/api/ps5/rp/picture"),
    ("GET", "/api/ps5/rp/replay"),
    ("GET", "/api/ps5/rp/session"),
    ("POST", "/api/ps5/rp/settings"),
    ("POST", "/api/ps5/rp/start"),
    ("POST", "/api/ps5/rp/stop"),
    ("GET", "/api/ps5/rp/ws"),
    ("POST", "/api/ps5/saves/close"),
    ("GET", "/api/ps5/saves/list"),
    ("POST", "/api/ps5/saves/open"),
    ("GET", "/api/ps5/screenshots/list"),
    ("POST", "/api/ps5/sdk/patch"),
    ("POST", "/api/ps5/sdk/restore"),
    ("GET", "/api/ps5/sdk/scan"),
    ("POST", "/api/ps5/shell/run"),
    ("POST", "/api/ps5/smp-meta/control"),
    ("GET", "/api/ps5/smp-meta/stats"),
    ("GET", "/api/ps5/smp/checkout"),
    ("POST", "/api/ps5/smp/checkout/begin"),
    ("POST", "/api/ps5/smp/checkout/finish"),
    ("GET", "/api/ps5/smp/image-rw"),
    ("POST", "/api/ps5/smp/image-rw/begin"),
    ("POST", "/api/ps5/smp/image-rw/finish"),
    ("POST", "/api/ps5/smp/rescan"),
    ("GET", "/api/ps5/smp/status"),
    ("GET", "/api/ps5/status"),
    ("GET", "/api/ps5/syslog/tail"),
    ("GET", "/api/ps5/tailnet"),
    ("POST", "/api/ps5/tailnet"),
    ("GET", "/api/ps5/time/get"),
    ("POST", "/api/ps5/time/sync"),
    ("GET", "/api/ps5/title-sdk-pair"),
    ("POST", "/api/ps5/toast"),
    ("POST", "/api/ps5/usb/eject"),
    ("POST", "/api/ps5/users/create"),
    ("POST", "/api/ps5/users/delete"),
    ("GET", "/api/ps5/videos/list"),
    ("GET", "/api/ps5/vinput"),
    ("PUT", "/api/ps5/vinput"),
    ("GET", "/api/ps5/vinput/caps"),
    ("POST", "/api/ps5/vinput/report"),
    ("GET", "/api/ps5/vinput/ws"),
    ("GET", "/api/ps5/volumes"),
    ("GET", "/api/queue"),
    ("POST", "/api/queue"),
    ("POST", "/api/queue/hold"),
    ("POST", "/api/queue/release"),
    ("GET", "/api/queue/{id}"),
    ("POST", "/api/queue/{id}/move"),
    ("POST", "/api/queue/{id}/pause"),
    ("POST", "/api/queue/{id}/remove"),
    ("POST", "/api/queue/{id}/resume"),
    ("POST", "/api/rar/inspect"),
    ("POST", "/api/rar/packages"),
    ("GET", "/api/remote/connections"),
    ("POST", "/api/remote/connections"),
    ("DELETE", "/api/remote/connections/{id}"),
    ("PUT", "/api/remote/connections/{id}"),
    ("POST", "/api/remote/connections/{id}/host-key"),
    ("GET", "/api/remote/connections/{id}/shares"),
    ("POST", "/api/remote/connections/{id}/test"),
    ("POST", "/api/remote/fetch"),
    ("POST", "/api/remote/fetch/cleanup"),
    ("POST", "/api/remote/inspect-folder"),
    ("POST", "/api/remote/list"),
    ("POST", "/api/remote/shares"),
    ("POST", "/api/remote/test"),
    ("GET", "/api/save-backup"),
    ("PUT", "/api/save-backup"),
    ("POST", "/api/save-backup/copy"),
    ("POST", "/api/save-backup/copy-console"),
    ("GET", "/api/save-backup/copy-console/targets"),
    ("POST", "/api/save-backup/import-ps4"),
    ("GET", "/api/save-backup/latest"),
    ("GET", "/api/save-backup/list"),
    ("GET", "/api/save-backup/note"),
    ("POST", "/api/save-backup/now"),
    ("GET", "/api/save-backup/ps2"),
    ("POST", "/api/save-backup/restore"),
    ("POST", "/api/save-backup/restore-file"),
    ("POST", "/api/save-backup/restore-to-user"),
    ("POST", "/api/save-backup/run"),
    ("GET", "/api/save-backup/server"),
    ("PUT", "/api/save-backup/server"),
    ("GET", "/api/save-backup/server/list"),
    ("POST", "/api/save-backup/server/restore"),
    ("POST", "/api/save-backup/server/run"),
    ("POST", "/api/save-backup/share"),
    ("POST", "/api/save-backup/snapshot"),
    ("GET", "/api/save-backup/switch"),
    ("POST", "/api/save-backup/use-shared"),
    ("GET", "/api/settings"),
    ("PATCH", "/api/settings"),
    ("GET", "/api/settings-backup"),
    ("POST", "/api/settings-backup"),
    ("GET", "/api/share"),
    ("POST", "/api/share"),
    ("POST", "/api/share/check"),
    ("POST", "/api/share/receive"),
    ("DELETE", "/api/share/receive/{id}"),
    ("GET", "/api/share/receive/{id}"),
    ("POST", "/api/share/receive/{id}/accept"),
    ("GET", "/api/share/receive/{id}/data/{item}"),
    ("POST", "/api/share/receive/{id}/use-save"),
    ("GET", "/api/share/ws"),
    ("DELETE", "/api/share/{id}"),
    ("GET", "/api/share/{id}"),
    ("POST", "/api/share/{id}/decide"),
    ("GET", "/api/shared/{name}"),
    ("PUT", "/api/shared/{name}"),
    ("GET", "/api/shared/{name}/schema"),
    ("POST", "/api/speed-test/cleanup"),
    ("POST", "/api/speed-test/prepare"),
    ("POST", "/api/stream/destinations"),
    ("DELETE", "/api/stream/destinations/{id}"),
    ("POST", "/api/stream/destinations/{id}"),
    ("POST", "/api/stream/start"),
    ("GET", "/api/stream/status"),
    ("POST", "/api/stream/stop"),
    ("POST", "/api/stream/tools/ffmpeg/install"),
    ("GET", "/api/tailnet"),
    ("POST", "/api/tailnet"),
    ("GET", "/api/tailnet/check"),
    ("GET", "/api/tailnet/consoles"),
    ("POST", "/api/tailnet/consoles/add"),
    ("GET", "/api/tailnet/route"),
    ("GET", "/api/tailnet/via"),
    ("POST", "/api/tar/inspect"),
    ("POST", "/api/tar/inspect/stream"),
    ("POST", "/api/transfer/7z"),
    ("POST", "/api/transfer/dir"),
    ("POST", "/api/transfer/dir-diff-preview"),
    ("POST", "/api/transfer/dir-reconcile"),
    ("POST", "/api/transfer/download"),
    ("POST", "/api/transfer/download-zip"),
    ("POST", "/api/transfer/file"),
    ("POST", "/api/transfer/file-list"),
    ("POST", "/api/transfer/ps5-to-ps5"),
    ("GET", "/api/transfer/ps5-to-ps5/remote"),
    ("POST", "/api/transfer/ps5-to-ps5/test"),
    ("POST", "/api/transfer/rar"),
    ("POST", "/api/transfer/tar"),
    ("POST", "/api/transfer/zip"),
    ("GET", "/api/version"),
    ("GET", "/api/workshop/jobs"),
    ("GET", "/api/workshop/settings"),
    ("POST", "/api/workshop/settings"),
    ("POST", "/api/zip/inspect"),
    ("POST", "/api/zip/inspect/stream"),
    ("GET", "/join"),
    ("GET", "/pad"),
    ("GET", "/pkg-host/link/{file}"),
    ("GET", "/pkg-host/{session}/{filename}"),
    ("GET", "/rp-player"),
    ("GET", "/share"),
];

/// The catalog as `ps5upload://catalog` serves it. Pages for people and for the console
/// (`/`, `/pad`, `/pkg-host/...`) are in the list, but `api_request` only calls `/api/` routes.
pub(crate) fn as_json() -> Value {
    let routes: Vec<Value> = ROUTES
        .iter()
        .map(|(method, path)| json!({ "method": method, "path": path }))
        .collect();
    json!({
        "count": routes.len(),
        "how_to_call": "Use the api_request tool with a route's method and path. Most routes name \
                        a console as `addr`: in the query for a GET, in the body for a POST. A \
                        {name} in a path is a value to fill in.",
        "routes": routes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// One function of a source file, up to its closing brace at the start of a line.
    fn function<'a>(source: &'a str, signature: &str) -> &'a str {
        let start = source
            .find(signature)
            .unwrap_or_else(|| panic!("{signature} is gone: this test must follow it"));
        let rest = &source[start..];
        let end = rest.find("\n}\n").expect("the function has an end");
        &rest[..end]
    }

    /// The `.route(path, method(handler).method(handler))` calls in `source`. Only names called at
    /// the top level of the call count: `map.get(key)` inside a handler closure is deeper.
    fn routes_in(source: &str) -> Vec<(String, String)> {
        const CALL: &str = ".route(";
        const MGMT_ROUTE: &str = "ps5upload_core::mgmt_proxy::ROUTE";
        // The reverse-proxy prefix's own root, which is `/` again.
        const PREFIX_ROOT: &str = "&format!(\"{base}/\")";
        const PREFIX_ROOT_OLD: &str = "&format!(\"{}/\", base)";
        let bytes = source.as_bytes();
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(found) = source[from..].find(CALL) {
            let mut at = from + found + CALL.len();
            while bytes[at].is_ascii_whitespace() {
                at += 1;
            }
            let rest = &source[at..];
            let (path, after_path) = if let Some(literal) = rest.strip_prefix('"') {
                let end = literal.find('"').expect("the path ends");
                (&literal[..end], at + 1 + end + 1)
            } else if rest.starts_with(MGMT_ROUTE) {
                (ps5upload_core::mgmt_proxy::ROUTE, at + MGMT_ROUTE.len())
            } else if rest.starts_with(PREFIX_ROOT) || rest.starts_with(PREFIX_ROOT_OLD) {
                from = at + PREFIX_ROOT.len();
                continue;
            } else {
                panic!("add this route's path to the scan: {}", &rest[..40]);
            };
            let mut depth = 1;
            let mut in_string = false;
            let mut methods = 0;
            let mut p = after_path;
            while depth > 0 {
                let byte = bytes[p];
                if in_string {
                    match byte {
                        b'\\' => p += 1,
                        b'"' => in_string = false,
                        _ => {}
                    }
                } else {
                    match byte {
                        b'"' => in_string = true,
                        b'(' => {
                            let name_start = source[..p]
                                .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
                                .map_or(0, |i| i + 1);
                            let name = &source[name_start..p];
                            if depth == 1
                                && matches!(name, "get" | "post" | "put" | "delete" | "patch")
                            {
                                out.push((name.to_ascii_uppercase(), path.to_string()));
                                methods += 1;
                            }
                            depth += 1;
                        }
                        b')' => depth -= 1,
                        _ => {}
                    }
                }
                p += 1;
            }
            assert!(methods > 0, "no method found for {path}");
            from = p;
        }
        out
    }

    /// Every router the engine mounts, as `build_app_and_mcp` in `lib.rs` merges them.
    fn mounted_routes() -> Vec<(String, String)> {
        let lib = function(include_str!("../lib.rs"), "fn build_app_and_mcp(");
        let pkg_install = include_str!("../pkg_install.rs");
        let mut routes = routes_in(lib);
        routes.extend(routes_in(function(pkg_install, "pub fn api_router(")));
        routes.extend(routes_in(function(pkg_install, "pub fn host_router(")));
        routes.extend(routes_in(function(
            include_str!("../remote_download.rs"),
            "pub fn router(",
        )));
        routes.extend(routes_in(function(
            include_str!("../browser_payload.rs"),
            "pub fn router<",
        )));
        routes.extend(routes_in(function(
            include_str!("../payloads_api.rs"),
            "pub fn router<",
        )));
        routes.extend(routes_in(function(
            include_str!("../payloads_api.rs"),
            "fn router_with<",
        )));
        routes.extend(routes_in(function(
            include_str!("../payload_updates_api.rs"),
            "pub(crate) fn routes_with<",
        )));
        routes.extend(routes_in(function(
            include_str!("../console_tools_api.rs"),
            "pub fn router<",
        )));
        routes.extend(routes_in(function(
            include_str!("../queue_api.rs"),
            "pub fn router<",
        )));
        routes.extend(routes_in(function(
            include_str!("../ops.rs"),
            "pub(crate) fn router(",
        )));
        routes
    }

    #[test]
    fn the_catalog_is_exactly_what_the_router_mounts() {
        let mounted: BTreeSet<(String, String)> = mounted_routes()
            .into_iter()
            .filter(|(_, path)| !HOLD_CONSOLE_KEYS.contains(&path.as_str()))
            .collect();
        let listed: BTreeSet<(String, String)> = ROUTES
            .iter()
            .map(|(method, path)| (method.to_string(), path.to_string()))
            .collect();
        let missing: Vec<_> = mounted.difference(&listed).collect();
        assert!(
            missing.is_empty(),
            "routes missing from mcp/catalog_routes.rs: {missing:?}"
        );
        let stale: Vec<_> = listed.difference(&mounted).collect();
        assert!(
            stale.is_empty(),
            "routes in mcp/catalog_routes.rs that nothing mounts: {stale:?}"
        );
    }

    #[test]
    fn every_router_the_engine_merges_is_read_by_the_test_above() {
        // A new sub-router would otherwise add routes the test above never sees.
        let lib = function(include_str!("../lib.rs"), "fn build_app_and_mcp(");
        let merged: BTreeSet<&str> = lib
            .match_indices(".merge(")
            .map(|(at, call)| {
                let rest = lib[at + call.len()..].trim_start();
                // A call, or a router already made (`ops_router`).
                let end = rest.find(['(', ')']).expect("a merge names something");
                &rest[..end]
            })
            .collect();
        let known = BTreeSet::from([
            "browser_payload::router",
            "console_tools_api::router",
            "mcp::router",
            "ops_router",
            "payloads_api::router",
            "pkg_install::api_router",
            "pkg_install::host_router",
            "queue_api::router",
            "remote_download::router",
        ]);
        assert_eq!(merged, known);
    }

    #[test]
    fn the_routes_with_console_keys_are_mounted_and_not_listed() {
        let mounted = mounted_routes();
        for secret in HOLD_CONSOLE_KEYS {
            assert!(
                mounted.iter().any(|(_, path)| path == secret),
                "{secret} moved: this list must follow it"
            );
            assert!(
                !ROUTES.iter().any(|(_, path)| path == secret),
                "{secret} must stay out of the catalog"
            );
            assert!(holds_console_keys(secret));
        }
        for written in [
            "/api/consoles/%64ocument",
            "/API/Consoles/Document",
            "/api//consoles/document/",
            "/api/settings-backup/%67et",
        ] {
            assert!(holds_console_keys(written), "{written}");
        }
        for fine in [
            "/api/consoles",
            "/api/settings-backup",
            "/api/shared/consoles",
        ] {
            assert!(!holds_console_keys(fine), "{fine}");
        }
    }

    /// `api/openapi.json` is generated from the source, and read back here with a different scan,
    /// so a route that one of them misses fails. Regenerate: `cargo run -p ps5upload-apidoc`.
    #[test]
    fn the_openapi_file_documents_exactly_what_the_router_mounts() {
        let mounted: BTreeSet<(String, String)> = mounted_routes()
            .into_iter()
            .filter(|(_, path)| !HOLD_CONSOLE_KEYS.contains(&path.as_str()))
            .chain(
                // mcp::router, which the catalog leaves out: it is not an API route to call.
                ["GET", "POST", "DELETE"]
                    .iter()
                    .map(|method| (method.to_string(), "/mcp".to_string())),
            )
            .chain(
                // The owner's switch for agents, mounted beside /mcp so no tool can reach it.
                ["GET", "PUT"]
                    .iter()
                    .map(|method| (method.to_string(), super::super::access::ROUTE.to_string())),
            )
            .collect();
        let documented: BTreeSet<(String, String)> =
            crate::openapi::operations().into_iter().collect();
        let missing: Vec<_> = mounted.difference(&documented).collect();
        assert!(
            missing.is_empty(),
            "routes missing from api/openapi.json, run `cargo run -p ps5upload-apidoc` in engine/: \
             {missing:?}"
        );
        let stale: Vec<_> = documented.difference(&mounted).collect();
        assert!(
            stale.is_empty(),
            "routes in api/openapi.json that nothing mounts, run `cargo run -p ps5upload-apidoc` in \
             engine/: {stale:?}"
        );
    }

    #[test]
    fn the_openapi_file_keeps_the_routes_with_console_keys_out() {
        let text = crate::openapi::document();
        for secret in HOLD_CONSOLE_KEYS {
            assert!(!text.contains(&format!("\"{secret}\"")), "{secret}");
        }
    }

    #[test]
    fn the_helper_route_is_named_as_core_names_it() {
        assert!(HOLD_CONSOLE_KEYS.contains(&ps5upload_core::mgmt_proxy::ROUTE));
    }

    #[test]
    fn keys_leave_an_answer_wherever_they_are() {
        let mut answer = json!({
            "credential": "c", "regist_key": "r", "rp_key": "k", "account_id": "42",
            "body": {"wake_rp_key": "k", "name": "ok"},
            "drives": [{"name": "d", "lock": {"alg": "argon2id", "hash": "h", "salt": "s"}}],
        });
        assert!(redact_keys(&mut answer));
        let text = answer.to_string();
        for gone in ["\"c\"", "\"r\"", "\"k\"", "argon2id"] {
            assert!(!text.contains(gone), "{text}");
        }
        assert_eq!(answer["account_id"], "42");
        assert_eq!(answer["body"]["name"], "ok");
        assert_eq!(answer["drives"][0]["has_password"], true);
        let mut plain = json!({"name": "x", "lock": {"holder": "a"}});
        assert!(!redact_keys(&mut plain));
        assert_eq!(plain["lock"]["holder"], "a");
    }

    #[test]
    fn a_failure_in_words_loses_what_it_quotes() {
        let mut job = json!({
            "jobs": [{
                "status": "failed",
                "error": "fetch https://me:hunter2@nas/a.pkg?token=abc123 failed",
                "error_detail": "password=hunter2",
                "path": "/data/token=kept.txt",
            }],
            "message": {"why": "Bearer abcdef0123456789"},
        });
        assert!(redact_keys(&mut job));
        let text = job.to_string();
        for secret in ["hunter2", "abc123", "abcdef0123456789"] {
            assert!(!text.contains(secret), "{text}");
        }
        // A path is the user's, not a report.
        assert_eq!(job["jobs"][0]["path"], "/data/token=kept.txt");
        assert!(!redact_keys(&mut job));
    }

    #[test]
    fn a_document_with_keys_is_refused_however_it_is_written_and_only_those() {
        let doc = |path: &str, name: &str| {
            reaches_a_key_document(path, &[("name", name.to_string())], None)
        };
        for (path, name) in [
            ("/api/mesh/doc", "backup-01"),
            ("/API/Mesh/Doc", "BACKUP-01"),
            ("/api//mesh/doc/", "backup-01"),
            ("/api/mesh/%64oc", "%62ackup-01"),
            ("/api/mesh/doc", " backup-16 "),
            ("/api/mesh/doc", "state"),
            ("/api/mesh/doc", "Consoles"),
            ("/api/mesh/doc", "drives"),
        ] {
            assert!(doc(path, name), "{path} {name}");
        }
        for name in ["backups", "prefs", "my-backup-01", "shared"] {
            assert!(!doc("/api/mesh/doc", name), "{name}");
        }
        assert!(!doc("/api/ps5/status", "backup-01"));
        let put = json!({"addr": "a", "name": "backup-02", "base": 1, "doc": {}});
        assert!(reaches_a_key_document("/api/mesh/doc", &[], Some(&put)));
        for file in [
            "/data/ps5upload/mesh/v1/doc-backup-01",
            "/data//ps5upload/MESH/./v1/doc-backup-01",
            "/data/ps5upload/mesh/v1/doc-%62ackup-01",
            "/data/ps5upload/mesh/x/../v1/doc-backup-01",
            "/data/ps5upload/mesh/v1/doc-state",
        ] {
            let body = json!({"addr": "a", "req": {"path": file}});
            assert!(
                reaches_a_key_document("/api/ps5/fs/read-preview", &[], Some(&body)),
                "{file}"
            );
        }
        let shell = json!({"cmd": "cat /data/ps5upload/mesh/v1/doc-backup-01"});
        assert!(!reaches_a_key_document(
            "/api/ps5/shell/run",
            &[],
            Some(&shell)
        ));
        for file in [
            "/user/data/ps5upload/np-backup/0x00000001.txt",
            "/user/data/ps5upload/mesh/v1/doc-consoles",
            "/data/ps5upload/ava/identity",
            "/user/data/ps5upload/ava",
            "/user/data/ps5upload/smb/shares.key",
        ] {
            let body = json!({"addr": "a", "path": file});
            assert!(
                reaches_a_key_document("/api/ps5/fs/read-preview", &[], Some(&body)),
                "{file}"
            );
        }
        for file in [
            "/user/data/ps5upload/pkg_temp/a.pkg",
            "/user/home/1/savedata/x",
            "/data/ps5upload/stderr.log",
            "/data/ps5upload/avatars/a.png",
        ] {
            let body = json!({"addr": "a", "path": file});
            assert!(
                !reaches_a_key_document("/api/ps5/fs/read-preview", &[], Some(&body)),
                "{file}"
            );
        }
        let np = json!({"path": "/data/ps5upload/np-backup/0x00000001.txt"});
        assert!(reaches_a_key_document(
            "/api/ps5/fs/read-preview",
            &[],
            Some(&np)
        ));
        let other = json!({"path": "/data/ps5upload/mesh/v1/doc-prefs"});
        assert!(!reaches_a_key_document(
            "/api/ps5/fs/read-preview",
            &[],
            Some(&other)
        ));
    }

    #[test]
    fn the_scan_reads_each_way_a_route_is_written() {
        let source = r#"
            .route("/one", get(a))
            .route(
                "/two",
                get(b).post(c),
            )
            .route("/three", axum::routing::put(d).delete(e))
            .route("/four", post(move |x: State<S>| { map.get(x).post(y) }).layer(z(1)))
            .route(
                "/api/mgmt/call",
                post(handler),
            )
            .route(&format!("{}/", base), get(ui_handler))
        "#;
        let found: Vec<String> = routes_in(source)
            .into_iter()
            .map(|(method, path)| format!("{method} {path}"))
            .collect();
        assert_eq!(
            found,
            [
                "GET /one",
                "GET /two",
                "POST /two",
                "PUT /three",
                "DELETE /three",
                "POST /four",
                "POST /api/mgmt/call",
            ]
        );
    }

    #[test]
    fn the_list_is_sorted_without_repeats() {
        assert!(
            ROUTES
                .windows(2)
                .all(|pair| (pair[0].1, pair[0].0) < (pair[1].1, pair[1].0)),
            "keep ROUTES sorted by path, then method"
        );
    }

    #[test]
    fn the_catalog_serves_every_route() {
        let catalog = as_json();
        assert_eq!(catalog["count"], ROUTES.len());
        assert_eq!(catalog["routes"].as_array().unwrap().len(), ROUTES.len());
    }

    /// A tailnet sign-in link never reaches an agent, from any route or method: whoever opens it
    /// first could sign the node into a tailnet of their own.
    #[test]
    fn a_tailnet_sign_in_link_never_reaches_an_agent() {
        for (method, path) in [
            ("GET", "/api/tailnet"),
            ("POST", "/api/tailnet"),
            ("GET", "/api/ps5/tailnet"),
            ("POST", "/api/PS5//tailnet/"),
        ] {
            assert!(may_hold_agent_secrets(method, path), "{method} {path}");
            let mut answer = json!({
                "ok": true, "state": "needs-login", "login_url": "https://login.example/a/1",
                "peers": [{"name": "pc", "login_url": "https://login.example/a/2"}],
            });
            assert!(redact_for_agents(method, path, &mut answer));
            assert!(
                !answer.to_string().contains("login.example"),
                "a sign-in link is left"
            );
            assert_eq!(answer["state"], "needs-login");
        }
        assert!(!may_hold_agent_secrets("GET", "/api/ps5/volumes"));
    }

    /// The passive reads of Play Together and the phone link keep whether an invite is open, when
    /// it expires and who is in, never what lets someone in. Making an invite (a POST) answers with
    /// its link: that is what the user asked for.
    #[test]
    fn live_invites_leave_the_passive_reads_only() {
        let join = || {
            json!({
                "open": true,
                "guests": [{"id": 1, "name": "Sam", "player": 2}],
                "invite": {
                    "key": "k1", "code": "ABCD12", "code_display": "ABC-D12",
                    "url": "http://10.0.0.2:3000/join#i=k1", "urls": ["http://10.0.0.2:3000/join#i=k1"],
                    "expires_in_s": 1700,
                    "share": {"url": "https://ps5upload.phantomptr.com/join/#room=r&k=k2", "works": "anywhere"},
                    "internet": {"url": "https://ps5upload.phantomptr.com/join/#room=r&k=k2", "room": "r", "state": "open"},
                },
            })
        };
        let mut a = join();
        assert!(may_hold_agent_secrets("GET", "/api/join"));
        assert!(redact_for_agents("GET", "/api/join", &mut a));
        let text = a.to_string();
        let gone = ["k1", "k2", "ABCD12", "ABC-D12", "#i="];
        assert!(
            gone.iter().all(|g| !text.contains(g)),
            "the invite still lets someone in"
        );
        assert_eq!(a["invite"]["expires_in_s"], 1700);
        assert_eq!(a["invite"]["hidden"], true);
        assert_eq!(a["invite"]["internet"]["state"], "open");
        // The one link goes too; where it works stays.
        assert_eq!(a["invite"]["share"]["works"], "anywhere");
        assert_eq!(a["guests"][0]["name"], "Sam");
        // together_invite and together_open (POSTs) are not touched
        for path in ["/api/join/invite", "/api/join/open"] {
            let mut b = join();
            assert!(!may_hold_agent_secrets("POST", path));
            assert!(!redact_for_agents("POST", path, &mut b));
            assert_eq!(b, join());
        }

        let pad = || {
            json!({
                "open": true,
                "url": "http://10.0.0.2:3000/pad#t=deadbeef",
                "urls": ["http://10.0.0.2:3000/pad#t=deadbeef"],
                "internet": {"url": "https://ps5upload.phantomptr.com/pad/#room=r&k=cafe", "state": "open"},
                "pads": [{"id": 3, "name": "iphone"}],
                "pending": [],
            })
        };
        let mut p = pad();
        assert!(redact_for_agents("GET", "/api/ps5/rp/pad-link", &mut p));
        let text = p.to_string();
        assert!(
            !text.contains("deadbeef") && !text.contains("cafe"),
            "the phone link still lets someone in"
        );
        assert_eq!(
            (&p["open"], &p["link_hidden"]),
            (&json!(true), &json!(true))
        );
        assert_eq!(p["pads"][0]["id"], 3);
        let mut p = pad();
        assert!(!redact_for_agents("POST", "/api/ps5/rp/pad-link", &mut p));
        // a closed link has nothing to hide
        let mut closed = json!({"open": false, "url": null, "urls": [], "pads": []});
        assert!(redact_for_agents(
            "GET",
            "/api/ps5/rp/pad-link",
            &mut closed
        ));
        assert!(closed.get("url").is_none());
    }

    /// A Share link lets whoever has it ask for the files: the reads (share_status, api_request)
    /// keep what is shared and who asked, never the link or its room. Making one answers with it.
    #[test]
    fn share_links_leave_every_read_of_api_share() {
        let shares = || {
            json!({
                "shares": [{
                    "id": "s1",
                    "state": "open",
                    "url": "https://ps5upload.phantomptr.com/share/#room=ROOMROOMROOM&fp=AA&hub=h",
                    "lan_url": "http://10.0.0.2:3000/share#room=ROOMROOMROOM",
                    "room": "ROOMROOMROOM",
                    "items": [{"id": "i1", "name": "My App", "size": 5}],
                    "requests": [{"request": 1, "name": "Alex", "state": "asking"}],
                }],
                "receives": [{"id": "r1", "state": "done", "from": "Sam"}],
                "history": [],
            })
        };
        for path in [
            "/api/share",
            "/api/share/s1",
            "/API/share/s1/",
            "/api/share/receive/r1",
        ] {
            let mut v = shares();
            assert!(may_hold_agent_secrets("GET", path), "{path}");
            assert!(redact_for_agents("GET", path, &mut v), "{path}");
            let text = v.to_string();
            assert!(!text.contains("ROOMROOMROOM"), "{path}: the link is left");
            assert_eq!(v["link_hidden"], true);
            assert_eq!(v["shares"][0]["requests"][0]["name"], "Alex");
            assert_eq!(v["shares"][0]["items"][0]["name"], "My App");
        }
        // share_create answers with the link: the user asked for it.
        let mut made = json!({"id": "s1", "url": "https://x/share/#room=ROOMROOMROOM"});
        assert!(!redact_for_agents("POST", "/api/share", &mut made));
        assert!(made["url"].as_str().unwrap().contains("ROOMROOMROOM"));
    }
}
