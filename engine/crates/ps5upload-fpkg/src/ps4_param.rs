//! `sce_sys/param.json` for a PS4 game written as an image or a folder.
//!
//! ShadowMount+ reads `param.json`, and a PS4 game only has `param.sfo`: its image mounts and
//! installs, and then the console says the game couldn't be started (NP-103270-3, measured by
//! Alembic on a PS5 with ShadowMount+ 1.7beta8; not yet by us). The file is made from `param.sfo`
//! as SadykovIV/PS4pkg_to_ffpfsc makes it (GPL-3.0, `build_param_json`), by way of
//! fthdgn-gh/Alembic's `sfo::ps4_param_json` (GPL-3.0-or-later). Never for a package.

use std::collections::BTreeMap;

use ps5upload_pkg::{sfo_params, SfoValue};

use crate::source::{SourceFile, SourceTree};
use crate::Result;

/// `TITLE_00`..`TITLE_29` in the console's language order, as `appmeta` spells them.
const TITLE_LOCALES: [&str; 30] = [
    "ja-JP", "en-US", "fr-FR", "es-ES", "de-DE", "it-IT", "nl-NL", "pt-PT", "ru-RU", "ko-KR",
    "zh-Hant", "zh-Hans", "fi-FI", "sv-SE", "da-DK", "no-NO", "pl-PL", "pt-BR", "en-GB", "tr-TR",
    "es-419", "ar-AE", "fr-CA", "cs-CZ", "hu-HU", "el-GR", "ro-RO", "th-TH", "vi-VN", "id-ID",
];

fn quoted(s: &str) -> String {
    serde_json::Value::from(s).to_string()
}

/// One level of a sorted JSON object, at `indent` spaces.
fn object(entries: &BTreeMap<String, String>, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let body: Vec<String> = entries
        .iter()
        .map(|(k, v)| format!("{pad}{}: {v}", quoted(k)))
        .collect();
    format!("{{\n{}\n{}}}", body.join(",\n"), " ".repeat(indent - 2))
}

/// The `param.json` of a PS4 game, from its `param.sfo` values.
pub fn param_json(params: &[(String, SfoValue)], title_id: &str) -> String {
    let text = |key: &str| -> String {
        params
            .iter()
            .find_map(|(k, v)| match v {
                SfoValue::Text(t) if k == key => Some(t.trim().to_string()),
                _ => None,
            })
            .unwrap_or_default()
    };
    let numbered = |i: usize| text(&format!("TITLE_{i:02}"));

    // TITLE, else TITLE_01, else the first other TITLE_NN, else the IDs.
    let title = [text("TITLE"), text("TITLE_01")]
        .into_iter()
        .chain((0..30).filter(|i| *i != 1).map(numbered))
        .chain([text("CONTENT_ID"), text("TITLE_ID")])
        .find(|t| !t.is_empty())
        .unwrap_or_else(|| "Unknown Game".to_string());

    let mut localized = BTreeMap::new();
    for (i, locale) in TITLE_LOCALES.iter().enumerate() {
        let name = numbered(i);
        if !name.is_empty() {
            localized.insert((*locale).to_string(), name);
        }
    }
    let english = numbered(1);
    localized.insert(
        "en-US".to_string(),
        if english.is_empty() {
            title.clone()
        } else {
            english
        },
    );
    let mut parameters: BTreeMap<String, String> = localized
        .into_iter()
        .map(|(locale, name)| {
            let entry = BTreeMap::from([("titleName".to_string(), quoted(&name))]);
            (locale, object(&entry, 6))
        })
        .collect();
    parameters.insert("defaultLanguage".to_string(), quoted("en-US"));

    let mut top = BTreeMap::from([
        ("localizedParameters".to_string(), object(&parameters, 4)),
        ("titleId".to_string(), quoted(title_id)),
        ("titleName".to_string(), quoted(&title)),
    ]);
    for i in 1..=4 {
        let key = format!("USER_DEFINED_PARAM_{i}");
        let value = params.iter().find_map(|(k, v)| match v {
            SfoValue::Int(n) if *k == key && *n != 0 => Some(*n),
            _ => None,
        });
        if let Some(n) = value {
            top.insert(format!("userDefinedParam{i}"), n.to_string());
        }
    }
    format!("{}\n", object(&top, 2))
}

/// Four capitals and five digits.
fn is_title_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 9
        && b[..4].iter().all(u8::is_ascii_uppercase)
        && b[4..].iter().all(u8::is_ascii_digit)
}

/// A PS4 game with the `sce_sys/param.json` it lacks.
pub struct Ps4Params<'a> {
    inner: &'a mut dyn SourceTree,
    files: Vec<SourceFile>,
    path: String,
    json: Vec<u8>,
}

impl<'a> Ps4Params<'a> {
    /// `inner` with the file added, or `None` when it needs none: it has a `param.json`, or no
    /// `param.sfo` with a title ID, or that title ID is a PS5 game's. An unreadable `param.sfo`
    /// is also `None`: the build goes on as it did, and the file was never there to lose.
    pub fn wrap(inner: &'a mut dyn SourceTree) -> Result<Option<Self>> {
        let is = |f: &SourceFile, name: &str| f.path.eq_ignore_ascii_case(name);
        if inner.files().iter().any(|f| is(f, "sce_sys/param.json")) {
            return Ok(None);
        }
        let Some(sfo) = inner
            .files()
            .iter()
            .find(|f| is(f, "sce_sys/param.sfo"))
            .map(|f| f.path.clone())
        else {
            return Ok(None);
        };
        let bytes = inner.read(&sfo)?;
        let Ok(params) = sfo_params(&bytes) else {
            return Ok(None);
        };
        let title_id = params.iter().find_map(|(k, v)| match v {
            SfoValue::Text(t) if k == "TITLE_ID" => Some(t.trim().to_string()),
            _ => None,
        });
        let Some(title_id) = title_id.filter(|id| is_title_id(id) && !id.starts_with("PPSA"))
        else {
            return Ok(None);
        };
        let json = param_json(&params, &title_id).into_bytes();
        // Beside param.sfo, in its folder's own spelling: UFS tells the cases apart.
        let path = format!("{}param.json", &sfo[..sfo.len() - "param.sfo".len()]);
        let mut files = inner.files().to_vec();
        files.push(SourceFile {
            path: path.clone(),
            size: json.len() as u64,
        });
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Some(Self {
            inner,
            files,
            path,
            json,
        }))
    }
}

impl SourceTree for Ps4Params<'_> {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn empty_dirs(&self) -> &[String] {
        self.inner.empty_dirs()
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        if path == self.path {
            return Ok(self.json.clone());
        }
        self.inner.read(path)
    }
    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        if path == self.path {
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(self.json.len());
            let end = start.saturating_add(len).min(self.json.len());
            return Ok(self.json[start..end].to_vec());
        }
        self.inner.read_range(path, offset, len)
    }
    fn describe(&self) -> String {
        format!("{} (PS4 param.json added)", self.inner.describe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(k: &str, v: &str) -> (String, SfoValue) {
        (k.to_string(), SfoValue::Text(v.to_string()))
    }

    #[test]
    fn matches_what_ps4pkg_to_ffpfsc_writes() {
        // The expected text is Alembic's test of the same port (tests/test_ps4.cpp).
        let params = vec![
            text("TITLE", "Synthetic"),
            text("TITLE_00", "Japanese"),
            text("TITLE_01", " English Name "),
            text("TITLE_ID", "CUSA00000"),
            ("USER_DEFINED_PARAM_1".to_string(), SfoValue::Int(3)),
            ("USER_DEFINED_PARAM_2".to_string(), SfoValue::Int(0)),
        ];
        assert_eq!(
            param_json(&params, "CUSA00000"),
            "{\n  \"localizedParameters\": {\n    \"defaultLanguage\": \"en-US\",\n    \
             \"en-US\": {\n      \"titleName\": \"English Name\"\n    },\n    \
             \"ja-JP\": {\n      \"titleName\": \"Japanese\"\n    }\n  },\n  \
             \"titleId\": \"CUSA00000\",\n  \"titleName\": \"Synthetic\",\n  \
             \"userDefinedParam1\": 3\n}\n"
        );
    }

    #[test]
    fn a_title_falls_back_and_quotes_are_escaped() {
        let only_id = vec![text("TITLE_ID", "CUSA00000")];
        assert!(param_json(&only_id, "CUSA00000").contains("\"titleName\": \"CUSA00000\""));
        let quote = vec![text("TITLE_05", "A \"B\"")];
        let json = param_json(&quote, "CUSA00000");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["titleName"], "A \"B\"");
        assert_eq!(v["localizedParameters"]["it-IT"]["titleName"], "A \"B\"");
        assert_eq!(v["localizedParameters"]["en-US"]["titleName"], "A \"B\"");
    }

    /// A `param.sfo` with these text values.
    fn sfo(values: &[(&str, &str)]) -> Vec<u8> {
        let mut keys = Vec::new();
        let mut data = Vec::new();
        let mut table = Vec::new();
        for (k, v) in values {
            let raw = format!("{v}\0");
            table.extend_from_slice(&(keys.len() as u16).to_le_bytes());
            table.extend_from_slice(&0x0204u16.to_le_bytes());
            table.extend_from_slice(&(raw.len() as u32).to_le_bytes());
            table.extend_from_slice(&(raw.len() as u32).to_le_bytes());
            table.extend_from_slice(&(data.len() as u32).to_le_bytes());
            keys.extend_from_slice(k.as_bytes());
            keys.push(0);
            data.extend_from_slice(raw.as_bytes());
        }
        let keys_at = 20 + table.len();
        let mut out = b"\0PSF\x01\x01\0\0".to_vec();
        out.extend_from_slice(&(keys_at as u32).to_le_bytes());
        out.extend_from_slice(&((keys_at + keys.len()) as u32).to_le_bytes());
        out.extend_from_slice(&(values.len() as u32).to_le_bytes());
        out.extend(table);
        out.extend(keys);
        out.extend(data);
        out
    }

    struct Tree(Vec<(String, Vec<u8>)>, Vec<SourceFile>);

    fn tree(entries: &[(&str, Vec<u8>)]) -> Tree {
        let files = entries
            .iter()
            .map(|(p, b)| SourceFile {
                path: (*p).to_string(),
                size: b.len() as u64,
            })
            .collect();
        Tree(
            entries
                .iter()
                .map(|(p, b)| ((*p).to_string(), b.clone()))
                .collect(),
            files,
        )
    }

    impl SourceTree for Tree {
        fn files(&self) -> &[SourceFile] {
            &self.1
        }
        fn read(&mut self, path: &str) -> Result<Vec<u8>> {
            Ok(self
                .0
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, b)| b.clone())
                .unwrap_or_default())
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }

    #[test]
    fn a_ps4_game_gets_the_file_in_its_own_folders_spelling() {
        let mut t = tree(&[
            ("eboot.bin", vec![1]),
            (
                "SCE_SYS/param.sfo",
                sfo(&[("TITLE", "Game"), ("TITLE_ID", "CUSA12345")]),
            ),
        ]);
        let mut wrapped = Ps4Params::wrap(&mut t).unwrap().expect("a PS4 game");
        let listed: Vec<_> = wrapped.files().iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            listed,
            ["SCE_SYS/param.json", "SCE_SYS/param.sfo", "eboot.bin"]
        );
        let size = wrapped.files()[0].size as usize;
        let json = wrapped.read("SCE_SYS/param.json").unwrap();
        assert_eq!(json.len(), size);
        assert_eq!(
            wrapped.read_range("SCE_SYS/param.json", 0, 1).unwrap(),
            b"{"
        );
        assert_eq!(wrapped.read("eboot.bin").unwrap(), [1]);
        let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(v["titleId"], "CUSA12345");
    }

    #[test]
    fn other_games_are_left_alone() {
        let has_json = &[
            ("sce_sys/param.json", b"{}".to_vec()),
            ("sce_sys/param.sfo", sfo(&[("TITLE_ID", "CUSA12345")])),
        ];
        let ps5 = &[("sce_sys/param.sfo", sfo(&[("TITLE_ID", "PPSA12345")]))];
        let bad_id = &[("sce_sys/param.sfo", sfo(&[("TITLE_ID", "nope")]))];
        let bad_sfo = &[("sce_sys/param.sfo", b"rubbish".to_vec())];
        let none = &[("eboot.bin", vec![1])];
        let cases: [&[(&str, Vec<u8>)]; 5] = [has_json, ps5, bad_id, bad_sfo, none];
        for entries in cases {
            let mut t = tree(entries);
            assert!(Ps4Params::wrap(&mut t).unwrap().is_none());
        }
    }
}
