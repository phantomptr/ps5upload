//! The rule that keeps Share to homebrew: PS5Upload never shares a commercial game, in any format
//! (folder, dump, mount image, package), nor its updates or add-ons. The engine applies it to
//! every app it is asked to share (and to every app a sender offers, on the receiving side),
//! whatever the app's page sent: it is not a UI check.
//!
//! The rule reads **how the app was built**, from its PARAM, never its Title ID or the publisher
//! code in its Content ID: homebrew borrows both (`PPSA99000`, `UP9000-FAKE77031_00-…`), and a
//! name proves nothing either way. A game that went through Sony's publishing tools carries
//! their marks; homebrew built with the open toolchains leaves them empty.
//!
//! **PS5 (`param.json`)**: retail when any of these is there:
//! * `sdkVersion` is not zero (`0x0400000000000000`; homebrew says `0x0000000000000000`);
//! * `requiredSystemSoftwareVersion` is not zero;
//! * `versionFileUri` points at `playstation.net` (the store's update server);
//! * an `asa` block (the store's signature) is present;
//! * `pubtools.submission` is true (it went through the store's submission).
//!
//! **PS4 (`param.sfo`)**: retail when any of these is there:
//! * `SYSTEM_VER` (the firmware the game needs) is not zero. The open PS4 toolchain writes 0, and
//!   so does every homebrew made from its samples;
//! * `PUBTOOLINFO` has an `sdk_ver=` that is not zero. Fake-package tools write `00000000` or
//!   leave it out.
//!
//! PS5Upload's own PS4 package writer (Convert › PS4 package) rewrites `PUBTOOLINFO` without
//! `sdk_ver` but keeps the game's `SYSTEM_VER`, so a converted retail game is still refused, and a
//! converted homebrew is shared as it was.
//!
//! A package (`.pkg`) or an image (`.exfat`, `.ffpkg`, `.ffpfs`, `.ffpfsc`) is judged by the PARAM
//! inside it, a folder by its `sce_sys`, an app on the console by its `sce_sys` and, when that is
//! not readable (a retail game's is encrypted), by the copy in `/user/appmeta/<Title ID>/`. An
//! update or add-on of a retail game carries the same build marks as the game, so it is refused
//! too. With both PARAMs, either one being retail is enough.
//!
//! What has no readable PARAM (an archive, an encrypted package, a broken folder) is refused:
//! without build information it can't be shown to be homebrew.

use serde_json::Value;

/// What an app's PARAM says about how it was built.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Build {
    /// The Title ID it names, for the message (`PPSA01234`), or empty.
    pub title_id: String,
    /// A param.json or param.sfo was read.
    pub read: bool,
    /// The store's build marks it carries, by name (`sdkVersion`, `SYSTEM_VER`, …): any one makes
    /// it retail.
    pub marks: Vec<&'static str>,
}

/// Why an app may not be shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A commercial game (or its update or add-on): the Title ID shown, or empty.
    Retail(String),
    /// No PARAM could be read.
    Unknown,
}

impl Refusal {
    /// What the person is told.
    pub(crate) fn message(&self) -> String {
        match self {
            Refusal::Retail(id) if id.is_empty() => {
                "This is a retail game. PS5Upload only shares homebrew.".to_string()
            }
            Refusal::Retail(id) => {
                format!("This is a retail game ({id}). PS5Upload only shares homebrew.")
            }
            Refusal::Unknown => {
                "PS5Upload can't confirm this is homebrew, so it can't share it.".to_string()
            }
        }
    }

    /// The Title ID it names, for the API (`refused.title_id`).
    pub(crate) fn title_id(&self) -> Option<&str> {
        match self {
            Refusal::Retail(id) if !id.is_empty() => Some(id),
            _ => None,
        }
    }
}

/// Whether a version word (`0x0400000000000000`, `08008000`, `0`) says anything: a digit other
/// than zero. Empty is nothing; text that is not a number counts as set (the safe side).
fn word_set(s: &str) -> bool {
    let t = s.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    t.chars().any(|c| c != '0')
}

/// A JSON value as a version word: a string, or a number.
fn value_set(v: Option<&Value>) -> bool {
    match v {
        Some(Value::String(s)) => word_set(s),
        Some(Value::Number(n)) => word_set(&n.to_string()),
        _ => false,
    }
}

/// A Title ID as the refusal names it (`PPSA01234`), from what the app said.
fn shown(title_id: &str) -> String {
    title_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .take(16)
        .collect()
}

/// The Title ID inside a Content ID (`UP9000-PPSA01234_00-…`).
fn title_of_content(content_id: &str) -> String {
    content_id
        .split_once('-')
        .map(|(_, rest)| rest.split(['_', '-']).next().unwrap_or(""))
        .map(shown)
        .unwrap_or_default()
}

impl Build {
    /// From a PS5 `param.json`. Not an object: nothing read.
    pub(crate) fn from_param_json(v: &Value) -> Build {
        let Some(o) = v.as_object() else {
            return Build::default();
        };
        let mut marks = Vec::new();
        if value_set(o.get("sdkVersion")) {
            marks.push("sdkVersion");
        }
        if value_set(o.get("requiredSystemSoftwareVersion")) {
            marks.push("requiredSystemSoftwareVersion");
        }
        if o.get("versionFileUri")
            .and_then(Value::as_str)
            .is_some_and(|u| u.to_ascii_lowercase().contains("playstation.net"))
        {
            marks.push("versionFileUri");
        }
        if o.contains_key("asa") {
            marks.push("asa");
        }
        // As read back from the package viewer's fields, `pubtools` may be its JSON text.
        let pubtools = match o.get("pubtools") {
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
            other => other.cloned(),
        };
        let submitted = pubtools.as_ref().and_then(|p| p.get("submission"));
        if matches!(submitted, Some(Value::Bool(true)))
            || submitted
                .and_then(Value::as_str)
                .is_some_and(|s| s.eq_ignore_ascii_case("true"))
        {
            marks.push("pubtools.submission");
        }
        let title_id = o
            .get("titleId")
            .and_then(Value::as_str)
            .map(shown)
            .filter(|t| !t.is_empty())
            .or_else(|| {
                o.get("contentId")
                    .and_then(Value::as_str)
                    .map(title_of_content)
            })
            .unwrap_or_default();
        Build {
            title_id,
            read: true,
            marks,
        }
    }

    /// From a param.json file's bytes (trailing NULs allowed). Not JSON: nothing read.
    pub(crate) fn from_param_json_bytes(b: &[u8]) -> Build {
        let end = b.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1);
        serde_json::from_slice::<Value>(&b[..end])
            .map(|v| Build::from_param_json(&v))
            .unwrap_or_default()
    }

    /// From a PS4 `param.sfo`'s fields, as text (an integer as `0x05050000` or decimal).
    pub(crate) fn from_sfo_fields<'a>(
        fields: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Build {
        let mut b = Build {
            read: true,
            ..Build::default()
        };
        let mut content = String::new();
        for (k, v) in fields {
            match k {
                "SYSTEM_VER" if word_set(v) => b.marks.push("SYSTEM_VER"),
                "PUBTOOLINFO" => {
                    let sdk = v
                        .split(',')
                        .find_map(|i| i.trim().strip_prefix("sdk_ver="))
                        .is_some_and(word_set);
                    if sdk {
                        b.marks.push("PUBTOOLINFO sdk_ver");
                    }
                }
                "TITLE_ID" => b.title_id = shown(v),
                "CONTENT_ID" => content = v.to_string(),
                _ => {}
            }
        }
        if b.title_id.is_empty() {
            b.title_id = title_of_content(&content);
        }
        b
    }

    /// From a param.sfo file's bytes. Not an SFO: nothing read.
    pub(crate) fn from_sfo_bytes(bytes: &[u8]) -> Build {
        use ps5upload_pkg::SfoValue;
        let Ok(params) = ps5upload_pkg::sfo_params(bytes) else {
            return Build::default();
        };
        let text: Vec<(String, String)> = params
            .into_iter()
            .map(|(k, v)| {
                let v = match v {
                    SfoValue::Text(t) => t,
                    SfoValue::Int(i) => format!("0x{i:08X}"),
                };
                (k, v)
            })
            .collect();
        Build::from_sfo_fields(text.iter().map(|(k, v)| (k.as_str(), v.as_str())))
    }

    /// From what the package viewer read of a package, image or folder: its PARAM's fields (a
    /// package may hold both a param.sfo and a param.json).
    pub(crate) fn from_inspection(g: &crate::inspect::GameInspection) -> Build {
        // An SFO's keys are upper case (`SYSTEM_VER`); a param.json's are not (`sdkVersion`).
        let sfo_key = |k: &str| {
            k.bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        };
        let (sfo, json): (Vec<_>, Vec<_>) = g.params.iter().partition(|p| sfo_key(&p.key));
        let mut out = Build::default();
        if !json.is_empty() {
            let obj: serde_json::Map<String, Value> = json
                .iter()
                .map(|p| {
                    let v = match p.value.trim_start().chars().next() {
                        Some('{') | Some('[') => serde_json::from_str(&p.value)
                            .unwrap_or_else(|_| Value::String(p.value.clone())),
                        _ if p.value == "true" => Value::Bool(true),
                        _ => Value::String(p.value.clone()),
                    };
                    (p.key.clone(), v)
                })
                .collect();
            out = out.merge(Build::from_param_json(&Value::Object(obj)));
        }
        if !sfo.is_empty() {
            out = out.merge(Build::from_sfo_fields(
                sfo.iter().map(|p| (p.key.as_str(), p.value.as_str())),
            ));
        }
        if out.title_id.is_empty() {
            out.title_id = shown(&g.identity.title_id);
        }
        out
    }

    /// Both PARAMs of one app: read when either was, retail when either is.
    pub(crate) fn merge(mut self, other: Build) -> Build {
        if self.title_id.is_empty() {
            self.title_id = other.title_id;
        }
        self.read |= other.read;
        for m in other.marks {
            if !self.marks.contains(&m) {
                self.marks.push(m);
            }
        }
        self
    }

    /// What the offer tells the receiving side (`{"read", "marks"}`).
    pub(crate) fn json(&self) -> Value {
        serde_json::json!({"read": self.read, "marks": self.marks})
    }

    /// The receiving side's reading of [`Build::json`]. None when the sender said nothing (an
    /// older PS5Upload, which applied its own, stricter rule before offering it).
    pub(crate) fn from_offer(v: &Value, title_id: Option<&str>) -> Option<Build> {
        let o = v.as_object()?;
        let marks = o
            .get("marks")
            .and_then(Value::as_array)
            .map(|a| {
                // Any mark at all is retail; the names are kept as a generic one.
                a.iter().take(16).map(|_| "sender").collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(Build {
            title_id: title_id.map(shown).unwrap_or_default(),
            read: o.get("read").and_then(Value::as_bool).unwrap_or(false),
            marks,
        })
    }
}

/// Whether an app built this way may be shared. See the module's notes for the rule.
pub(crate) fn check(b: &Build) -> Result<(), Refusal> {
    if !b.read {
        return Err(Refusal::Unknown);
    }
    if !b.marks.is_empty() {
        return Err(Refusal::Retail(b.title_id.clone()));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// Homebrew Playden, as its `/user/app/FAKE77031/sce_sys/param.json` says it.
    pub(crate) fn playden() -> Value {
        json!({
            "ageLevel": {"default": 0},
            "applicationCategoryType": 0,
            "applicationDrmType": "free",
            "attribute": 0,
            "contentId": "UP9000-FAKE77031_00-PLAYDENLAUNCHER1",
            "contentVersion": "01.000.000",
            "localizedParameters": {"defaultLanguage": "en-US", "en-US": {"titleName": "Playden"}},
            "masterVersion": "01.00",
            "pubtools": {"creationDate": "2026-10-01 12:00:00", "loudnessSnd0": "", "toolVersion": "2.00"},
            "requiredSystemSoftwareVersion": "0x0000000000000000",
            "sdkVersion": "0x0000000000000000",
            "titleId": "FAKE77031",
            "versionFileUri": ""
        })
    }

    pub(crate) fn prospero_store(title_id: &str, label: &str) -> Value {
        json!({
            "applicationCategoryType": 0,
            "applicationDrmType": "free",
            "contentId": format!("UP9000-{title_id}_00-{label}"),
            "contentVersion": "01.000.000",
            "localizedParameters": {"defaultLanguage": "en-US", "en-US": {"titleName": "ProsperoStore"}},
            "requiredSystemSoftwareVersion": "0x0000000000000000",
            "sdkVersion": "0x0000000000000000",
            "titleId": title_id,
            "versionFileUri": ""
        })
    }

    /// Retail ASTRO BOT, as its `/user/appmeta/PPSA21564/param.json` says it.
    pub(crate) fn astro_bot() -> Value {
        json!({
            "ageLevel": {"default": 3},
            "applicationCategoryType": 0,
            "applicationDrmType": "standard",
            "contentId": "UP9000-PPSA21564_00-0000000000000000",
            "contentVersion": "01.000.000",
            "localizedParameters": {"defaultLanguage": "en-US", "en-US": {"titleName": "ASTRO BOT"}},
            "masterVersion": "01.00",
            "pubtools": {"creationDate": "2024-08-01 10:00:00", "toolVersion": "2.79"},
            "requiredSystemSoftwareVersion": "0x1160000000000000",
            "sdkVersion": "0x0400000000000000",
            "titleId": "PPSA21564",
            "versionFileUri": "https://sgst.prod.dl.playstation.net/sgst/prod/00/PPSA21564_00/app/info/1/f_0/version.xml"
        })
    }

    /// Retail YouTube: free, with the store's signature and submission.
    pub(crate) fn youtube() -> Value {
        json!({
            "applicationCategoryType": 0,
            "applicationDrmType": "free",
            "asa": {"signature": "AAAA", "version": 1},
            "contentId": "UP4381-PPSA01650_00-YOUTUBEPS5000000",
            "contentVersion": "01.012.000",
            "pubtools": {"creationDate": "2025-05-01 09:00:00", "submission": true, "toolVersion": "2.70"},
            "requiredSystemSoftwareVersion": "0x1220000000000000",
            "sdkVersion": "0x0400000000000000",
            "titleId": "PPSA01650",
            "versionFileUri": "https://sgst.prod.dl.playstation.net/sgst/prod/00/PPSA01650_00/app/info/1/f_0/version.xml"
        })
    }

    /// Homebrew that has shared fine all along, same build fields as Playden.
    fn plain_homebrew(title_id: &str, cid_prefix: &str) -> Value {
        json!({
            "applicationDrmType": "free",
            "contentId": format!("{cid_prefix}-{title_id}_00-HOMEBREW00000000"),
            "requiredSystemSoftwareVersion": "0x0000000000000000",
            "sdkVersion": "0x0000000000000000",
            "titleId": title_id
        })
    }

    fn verdict(v: &Value) -> Result<(), Refusal> {
        check(&Build::from_param_json(v))
    }

    #[test]
    fn homebrew_that_borrows_a_store_publisher_or_title_id_is_shared() {
        assert_eq!(verdict(&playden()), Ok(()));
        assert_eq!(
            verdict(&prospero_store("PPSA99000", "PROSPEROSTORE000")),
            Ok(())
        );
        assert_eq!(
            verdict(&prospero_store("PPSA99008", "PROSPEROEDEN0000")),
            Ok(())
        );
        for (t, p) in [
            ("PLDM00001", "IV0000"),
            ("WKAL00001", "IV0000"),
            ("FAKE99260", "UP9000"),
            ("FAKE10101", "IV0000"),
        ] {
            assert_eq!(verdict(&plain_homebrew(t, p)), Ok(()), "{t}");
        }
    }

    #[test]
    fn retail_ps5_apps_are_refused_with_the_message() {
        let r = verdict(&astro_bot()).unwrap_err();
        assert_eq!(
            r.message(),
            "This is a retail game (PPSA21564). PS5Upload only shares homebrew."
        );
        assert_eq!(r.title_id(), Some("PPSA21564"));
        let b = Build::from_param_json(&astro_bot());
        assert_eq!(
            b.marks,
            [
                "sdkVersion",
                "requiredSystemSoftwareVersion",
                "versionFileUri"
            ]
        );
        // Free, but built and signed by the store.
        let b = Build::from_param_json(&youtube());
        assert!(b.marks.contains(&"asa"));
        assert!(b.marks.contains(&"pubtools.submission"));
        assert_eq!(check(&b), Err(Refusal::Retail("PPSA01650".into())));
    }

    #[test]
    fn each_mark_alone_makes_it_retail() {
        let base = playden();
        let with = |k: &str, v: Value| {
            let mut x = base.clone();
            x[k] = v;
            x
        };
        for v in [
            with("sdkVersion", json!("0x0400000000000000")),
            with("sdkVersion", json!(67108864)),
            with("sdkVersion", json!("garbage")),
            with("requiredSystemSoftwareVersion", json!("0x1160000000000000")),
            with(
                "versionFileUri",
                json!("https://sgst.prod.dl.PlayStation.net/x/version.xml"),
            ),
            with("asa", json!({})),
            with("asa", Value::Null),
            with(
                "pubtools",
                json!({"submission": true, "toolVersion": "2.00"}),
            ),
        ] {
            assert!(verdict(&v).is_err(), "{v}");
        }
        // Not marks: a zero word, an empty or other URI, submission false, the tool version.
        for v in [
            with("sdkVersion", json!("0x0")),
            with("sdkVersion", json!(0)),
            with("sdkVersion", json!("")),
            with("versionFileUri", json!("https://example.com/version.xml")),
            with(
                "pubtools",
                json!({"submission": false, "toolVersion": "2.79"}),
            ),
            with("applicationDrmType", json!("standard")),
        ] {
            assert_eq!(verdict(&v), Ok(()), "{v}");
        }
    }

    #[test]
    fn updates_and_add_ons_of_retail_carry_the_marks_and_are_refused() {
        let mut patch = astro_bot();
        patch["contentVersion"] = json!("01.003.000");
        patch["applicationCategoryType"] = json!(0);
        patch["contentId"] = json!("UP9000-PPSA21564_00-0000000000000000");
        assert!(verdict(&patch).is_err());
        let dlc = json!({
            "addcont": {"serviceIdForSharing": ["UP9000-PPSA21564_00"]},
            "applicationCategoryType": 65536,
            "contentId": "UP9000-PPSA21564_00-ASTROBOTDLC00001",
            "requiredSystemSoftwareVersion": "0x1160000000000000",
            "sdkVersion": "0x0400000000000000",
            "titleId": "PPSA21564"
        });
        assert!(verdict(&dlc).is_err());
    }

    #[test]
    fn nothing_readable_is_refused_as_unknown() {
        assert_eq!(check(&Build::default()), Err(Refusal::Unknown));
        assert_eq!(verdict(&json!("not an object")), Err(Refusal::Unknown));
        assert_eq!(
            check(&Build::from_param_json_bytes(b"\x7fELF encrypted")),
            Err(Refusal::Unknown)
        );
        assert_eq!(
            check(&Build::from_sfo_bytes(b"nope")),
            Err(Refusal::Unknown)
        );
        assert_eq!(
            Refusal::Unknown.message(),
            "PS5Upload can't confirm this is homebrew, so it can't share it."
        );
        assert_eq!(Refusal::Unknown.title_id(), None);
    }

    #[test]
    fn param_json_bytes_with_trailing_nuls_are_read() {
        let mut b = astro_bot().to_string().into_bytes();
        b.extend_from_slice(&[0; 64]);
        assert!(check(&Build::from_param_json_bytes(&b)).is_err());
        let b = playden().to_string().into_bytes();
        assert_eq!(check(&Build::from_param_json_bytes(&b)), Ok(()));
    }

    /// A PARAM.SFO with text and integer fields.
    pub(crate) fn sfo(pairs: &[(&str, SfoField)]) -> Vec<u8> {
        let mut keys = Vec::new();
        let mut data = Vec::new();
        let mut index = Vec::new();
        for (k, v) in pairs {
            let (fmt, val) = match v {
                SfoField::Text(t) => (0x0204u16, format!("{t}\0").into_bytes()),
                SfoField::Int(i) => (0x0404u16, i.to_le_bytes().to_vec()),
            };
            index.extend_from_slice(&(keys.len() as u16).to_le_bytes());
            index.extend_from_slice(&fmt.to_le_bytes());
            index.extend_from_slice(&(val.len() as u32).to_le_bytes());
            index.extend_from_slice(&(val.len() as u32).to_le_bytes());
            index.extend_from_slice(&(data.len() as u32).to_le_bytes());
            keys.extend_from_slice(k.as_bytes());
            keys.push(0);
            data.extend_from_slice(&val);
        }
        while keys.len() % 4 != 0 {
            keys.push(0);
        }
        let key_off = 0x14 + pairs.len() * 0x10;
        let mut out = b"\x00PSF".to_vec();
        out.extend_from_slice(&[1, 1, 0, 0]);
        out.extend_from_slice(&(key_off as u32).to_le_bytes());
        out.extend_from_slice(&((key_off + keys.len()) as u32).to_le_bytes());
        out.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
        out.extend_from_slice(&index);
        out.extend_from_slice(&keys);
        out.extend_from_slice(&data);
        out
    }

    #[derive(Clone, Copy)]
    pub(crate) enum SfoField {
        Text(&'static str),
        Int(u32),
    }
    use SfoField::{Int, Text};

    /// A retail PS4 game's PARAM.SFO.
    pub(crate) fn ps4_retail() -> Vec<u8> {
        sfo(&[
            ("APP_VER", Text("01.00")),
            ("ATTRIBUTE", Int(0)),
            ("CATEGORY", Text("gd")),
            ("CONTENT_ID", Text("UP0002-CUSA00341_00-GAMEGAMEGAMEGAME")),
            (
                "PUBTOOLINFO",
                Text("c_date=20141016,sdk_ver=01750001,st_type=digital50,img0_l0_size=4000"),
            ),
            ("PUBTOOLVER", Int(0x0271_0000)),
            ("SYSTEM_VER", Int(0x0175_0000)),
            ("TITLE", Text("A Retail Game")),
            ("TITLE_ID", Text("CUSA00341")),
            ("VERSION", Text("01.00")),
        ])
    }

    /// A PS4 homebrew as the open toolchain's samples make it.
    pub(crate) fn ps4_homebrew() -> Vec<u8> {
        sfo(&[
            ("APP_TYPE", Int(1)),
            ("APP_VER", Text("1.00")),
            ("ATTRIBUTE", Int(0)),
            ("CATEGORY", Text("gd")),
            ("CONTENT_ID", Text("IV0000-BREW00083_00-HELLOWORLD000000")),
            ("DOWNLOAD_DATA_SIZE", Int(0)),
            ("SYSTEM_VER", Int(0)),
            ("TITLE", Text("Hello World")),
            ("TITLE_ID", Text("BREW00083")),
            ("VERSION", Text("1.00")),
        ])
    }

    #[test]
    fn ps4_retail_is_refused_and_ps4_homebrew_is_shared() {
        let r = Build::from_sfo_bytes(&ps4_retail());
        assert_eq!(r.marks, ["PUBTOOLINFO sdk_ver", "SYSTEM_VER"]);
        assert_eq!(check(&r), Err(Refusal::Retail("CUSA00341".into())));
        assert_eq!(check(&Build::from_sfo_bytes(&ps4_homebrew())), Ok(()));
        // A homebrew with a CUSA Title ID and a store publisher code, but no build marks.
        let borrowed = sfo(&[
            ("CONTENT_ID", Text("UP0001-CUSA99999_00-HOMEBREWAPP00000")),
            ("PUBTOOLINFO", Text("c_date=20240101,sdk_ver=00000000")),
            ("SYSTEM_VER", Int(0)),
            ("TITLE_ID", Text("CUSA99999")),
        ]);
        assert_eq!(check(&Build::from_sfo_bytes(&borrowed)), Ok(()));
        // An update or add-on of a retail game keeps the game's marks.
        let patch = sfo(&[
            ("CATEGORY", Text("gp")),
            ("CONTENT_ID", Text("UP0002-CUSA00341_00-PATCH00000000001")),
            ("PUBTOOLINFO", Text("c_date=20150101,sdk_ver=02000001")),
            ("SYSTEM_VER", Int(0x0200_0000)),
        ]);
        let b = Build::from_sfo_bytes(&patch);
        assert_eq!(check(&b), Err(Refusal::Retail("CUSA00341".into())));
        let dlc = sfo(&[
            ("CATEGORY", Text("ac")),
            ("CONTENT_ID", Text("EP0002-CUSA00341_00-SEASONPASS000000")),
            ("PUBTOOLINFO", Text("c_date=20150101,sdk_ver=02000001")),
            ("TITLE_ID", Text("CUSA00341")),
        ]);
        assert!(check(&Build::from_sfo_bytes(&dlc)).is_err());
    }

    #[test]
    fn a_ps4_package_this_app_converted_keeps_its_verdict() {
        // Convert › PS4 package rewrites PUBTOOLINFO (no sdk_ver) and keeps SYSTEM_VER.
        let retail = sfo(&[
            ("CONTENT_ID", Text("UP0002-CUSA00341_00-GAMEGAMEGAMEGAME")),
            (
                "PUBTOOLINFO",
                Text("c_date=20141016,img0_l0_size=4000,img0_l1_size=0,img0_sc_ksize=512,img0_pc_ksize=832"),
            ),
            ("PUBTOOLVER", Int(0x0289_0000)),
            ("SYSTEM_VER", Int(0x0175_0000)),
            ("TITLE_ID", Text("CUSA00341")),
        ]);
        assert!(check(&Build::from_sfo_bytes(&retail)).is_err());
        let homebrew = sfo(&[
            ("CONTENT_ID", Text("IV0000-BREW00083_00-HELLOWORLD000000")),
            (
                "PUBTOOLINFO",
                Text("c_date=20261108,img0_l0_size=1,img0_l1_size=0,img0_sc_ksize=512,img0_pc_ksize=832"),
            ),
            ("PUBTOOLVER", Int(0x0289_0000)),
            ("SYSTEM_VER", Int(0)),
            ("TITLE_ID", Text("BREW00083")),
        ]);
        assert_eq!(check(&Build::from_sfo_bytes(&homebrew)), Ok(()));
    }

    #[test]
    fn both_params_merge_and_either_retail_wins() {
        let a = Build::from_param_json(&playden());
        let b = Build::from_sfo_bytes(&ps4_retail());
        let m = a.clone().merge(b);
        assert!(m.read);
        assert_eq!(m.title_id, "FAKE77031");
        assert!(check(&m).is_err());
        assert_eq!(check(&a.merge(Build::default())), Ok(()));
    }

    #[test]
    fn the_offer_carries_the_verdict_to_the_receiver() {
        let b = Build::from_param_json(&astro_bot());
        let back = Build::from_offer(&b.json(), Some("PPSA21564")).unwrap();
        assert!(check(&back).is_err());
        let b = Build::from_param_json(&playden());
        let back = Build::from_offer(&b.json(), Some("FAKE77031")).unwrap();
        assert_eq!(check(&back), Ok(()));
        let back = Build::from_offer(&json!({"read": false, "marks": []}), None).unwrap();
        assert_eq!(check(&back), Err(Refusal::Unknown));
        assert_eq!(Build::from_offer(&Value::Null, None), None);
    }

    #[test]
    fn a_retail_message_without_a_title_id_reads_well() {
        assert_eq!(
            Refusal::Retail(String::new()).message(),
            "This is a retail game. PS5Upload only shares homebrew."
        );
        assert_eq!(Refusal::Retail(String::new()).title_id(), None);
    }
}
