//! A title and cover for a game that cannot name itself (only its DLC, or nothing but an
//! archive's Game ID): PROSPEROPatches for PS5, ORBISPatches for PS4, as PS Game Library does.
//! Best effort; a network failure is never taken to mean "no such game".
//!
//! Scans run every minute, so what the sites answered is remembered in the index: a game a site
//! does not know is not asked about again for a day, a site that fails is left alone for an hour,
//! and one scan spends a bounded time asking. What is not asked now is asked by a later scan.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A game a site did not know is asked about again after this long.
pub const MISS_RETRY_SECS: i64 = 86_400;
/// A site that failed to answer is asked again after this long.
pub const DOWN_RETRY_SECS: i64 = 3_600;
/// One request's limit.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
/// What one scan spends asking, in all.
const SCAN_BUDGET: Duration = Duration::from_secs(60);

/// What the sites last answered, kept in the index with the scanner's caches. Times are Unix
/// seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OnlineMemory {
    /// Game ID: when a site last said it has no name for it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub misses: BTreeMap<String, i64>,
    /// Site: when it last failed to answer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub down: BTreeMap<String, i64>,
}

impl OnlineMemory {
    pub fn is_empty(&self) -> bool {
        self.misses.is_empty() && self.down.is_empty()
    }
}

/// `(title, cover_url)`.
pub type TitlePage = (Option<String>, Option<String>);

/// The lookups of one scan.
pub struct Lookups<'a> {
    pub memory: OnlineMemory,
    now: i64,
    started: Instant,
    budget: Duration,
    fetch: &'a dyn Fn(&str, Duration) -> Result<TitlePage, String>,
}

/// Within `secs` of `at`. A clock set back counts as past it.
fn recent(now: i64, at: i64, secs: i64) -> bool {
    now >= at && now - at < secs
}

impl<'a> Lookups<'a> {
    pub fn new(
        memory: OnlineMemory,
        now: i64,
        fetch: &'a dyn Fn(&str, Duration) -> Result<TitlePage, String>,
    ) -> Self {
        Self {
            memory,
            now,
            started: Instant::now(),
            budget: SCAN_BUDGET,
            fetch,
        }
    }

    /// What the site says of `game_id`, or `None` when it is not worth asking now or it did not
    /// answer.
    pub fn ask(&mut self, game_id: &str) -> Option<TitlePage> {
        let (host, _) = site_of(game_id);
        if self
            .memory
            .misses
            .get(game_id)
            .is_some_and(|at| recent(self.now, *at, MISS_RETRY_SECS))
            || self
                .memory
                .down
                .get(host)
                .is_some_and(|at| recent(self.now, *at, DOWN_RETRY_SECS))
        {
            return None;
        }
        let left = self.budget.saturating_sub(self.started.elapsed());
        if left.is_zero() {
            return None;
        }
        match (self.fetch)(game_id, left.min(REQUEST_TIMEOUT)) {
            Ok(page) => {
                self.memory.down.remove(host);
                if page.0.is_some() {
                    self.memory.misses.remove(game_id);
                } else {
                    self.memory.misses.insert(game_id.to_string(), self.now);
                }
                Some(page)
            }
            Err(e) => {
                crate::log_info!("collection: {host} did not answer: {e}");
                self.memory.down.insert(host.to_string(), self.now);
                None
            }
        }
    }

    /// What to keep for the next scan: only the games still in the collection.
    pub fn finish(mut self, in_collection: impl Fn(&str) -> bool) -> OnlineMemory {
        self.memory.misses.retain(|id, _| in_collection(id));
        let now = self.now;
        self.memory
            .down
            .retain(|_, at| recent(now, *at, DOWN_RETRY_SECS));
        self.memory
    }
}

/// `(site, its image host's name)` for a game.
fn site_of(game_id: &str) -> (&'static str, &'static str) {
    if game_id.starts_with("CU") {
        ("orbispatches.com", "orbispatches")
    } else {
        ("prosperopatches.com", "prosperopatches")
    }
}

/// `(title, cover_url)` from a title page, or `Err` when the page could not be fetched.
pub fn fetch(game_id: &str, timeout: Duration) -> Result<TitlePage, String> {
    let (host, cdn) = site_of(game_id);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(&format!("https://{host}/{game_id}"))
        .header("User-Agent", "Mozilla/5.0 ps5upload-collection/1.0")
        .call()
        .map_err(|e| e.to_string())?;
    if resp.status() == 404 {
        return Ok((None, None));
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let html = resp
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok((title_from(&html, game_id), cover_from(&html, game_id, cdn)))
}

/// The page title, without the Game ID prefix and the site suffix. The site's own generic
/// title (what an unknown ID gets) is not a game's name.
pub fn title_from(html: &str, game_id: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title>")? + "<title>".len();
    let end = start + lower[start..].find("</title>")?;
    let mut t = html[start..end]
        .trim()
        .replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&quot;", "\"");
    if t.to_ascii_uppercase()
        .starts_with(&game_id.to_ascii_uppercase())
    {
        t = t[game_id.len()..]
            .trim_start_matches([':', ' '])
            .trim()
            .to_string();
    }
    for sep in [" | ", " - ", " – ", " — "] {
        if let Some(i) = t.rfind(sep) {
            let tail = t[i + sep.len()..].to_ascii_lowercase();
            if tail.starts_with("orbispatches") || tail.starts_with("prosperopatches") {
                t.truncate(i);
            }
        }
    }
    let t = t.trim().to_string();
    let lt = t.to_ascii_lowercase();
    if t.is_empty()
        || t.eq_ignore_ascii_case(game_id)
        || lt.starts_with("prosperopatches")
        || lt.starts_with("orbispatches")
        || lt.contains("game tracker")
    {
        return None;
    }
    Some(t)
}

pub fn cover_from(html: &str, game_id: &str, cdn: &str) -> Option<String> {
    let needle = format!("https://cdn.{cdn}.com/titles/{game_id}_");
    let at = html.find(&needle)?;
    let rest = &html[at..];
    let end = rest.find(['"', '\'', ' ', ')']).unwrap_or(rest.len());
    let url = &rest[..end];
    url.contains("/icon0.").then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const NOW: i64 = 1_000_000;

    #[test]
    fn a_game_the_site_does_not_know_is_not_asked_about_again_for_a_day() {
        let asked = RefCell::new(Vec::new());
        let fetch = |id: &str, _: Duration| -> Result<TitlePage, String> {
            asked.borrow_mut().push(id.to_string());
            Ok((None, None))
        };
        let mut l = Lookups::new(OnlineMemory::default(), NOW, &fetch);
        assert_eq!(l.ask("PPSA00001"), Some((None, None)));
        assert_eq!(l.ask("PPSA00001"), None);
        let memory = l.finish(|_| true);
        assert_eq!(asked.borrow().len(), 1);

        // The next scan, moments later: still not asked.
        let mut l = Lookups::new(memory.clone(), NOW + 60, &fetch);
        assert_eq!(l.ask("PPSA00001"), None);
        assert_eq!(asked.borrow().len(), 1);
        // A day later it is, and a name found is no miss any more.
        let named =
            |_: &str, _: Duration| -> Result<TitlePage, String> { Ok((Some("Game".into()), None)) };
        let mut l = Lookups::new(memory, NOW + MISS_RETRY_SECS, &named);
        assert!(l.ask("PPSA00001").is_some());
        assert!(l.memory.misses.is_empty());
    }

    #[test]
    fn a_site_that_fails_is_left_alone_and_the_other_is_still_asked() {
        let asked = RefCell::new(Vec::new());
        let fetch = |id: &str, _: Duration| -> Result<TitlePage, String> {
            asked.borrow_mut().push(id.to_string());
            if id.starts_with("CU") {
                Err("connection refused".into())
            } else {
                Ok((Some("Named".into()), None))
            }
        };
        let mut l = Lookups::new(OnlineMemory::default(), NOW, &fetch);
        assert_eq!(l.ask("CUSA00001"), None);
        assert_eq!(l.ask("CUSA00002"), None, "one failure is enough");
        assert!(l.ask("PPSA00001").is_some());
        assert_eq!(*asked.borrow(), ["CUSA00001", "PPSA00001"]);
        // A failure says nothing about the game: it is no miss.
        assert!(l.memory.misses.is_empty());
        let memory = l.finish(|_| true);
        assert!(memory.down.contains_key("orbispatches.com"));

        let mut l = Lookups::new(memory.clone(), NOW + 60, &fetch);
        assert_eq!(l.ask("CUSA00003"), None);
        assert_eq!(asked.borrow().len(), 2);
        // An hour later, it is tried again.
        let mut l = Lookups::new(memory, NOW + DOWN_RETRY_SECS, &fetch);
        assert_eq!(l.ask("CUSA00003"), None);
        assert_eq!(asked.borrow().len(), 3);
    }

    #[test]
    fn a_scan_stops_asking_when_its_time_is_spent() {
        let asked = RefCell::new(0);
        let fetch = |_: &str, limit: Duration| -> Result<TitlePage, String> {
            assert!(limit <= REQUEST_TIMEOUT);
            *asked.borrow_mut() += 1;
            Ok((None, None))
        };
        let mut l = Lookups::new(OnlineMemory::default(), NOW, &fetch);
        l.budget = Duration::ZERO;
        assert_eq!(l.ask("PPSA00001"), None);
        assert_eq!(*asked.borrow(), 0);
        // Not asked is not a miss: a later scan asks.
        assert!(l.memory.misses.is_empty());
    }

    #[test]
    fn only_the_games_still_there_are_remembered() {
        let fetch = |_: &str, _: Duration| -> Result<TitlePage, String> { Ok((None, None)) };
        let mut l = Lookups::new(OnlineMemory::default(), NOW, &fetch);
        l.ask("PPSA00001");
        l.ask("PPSA00002");
        let memory = l.finish(|id| id == "PPSA00002");
        assert_eq!(memory.misses.keys().collect::<Vec<_>>(), ["PPSA00002"]);
    }

    #[test]
    fn titles_lose_the_id_and_the_site_name() {
        assert_eq!(
            title_from(
                "<title>PPSA01467: Marvel's Spider-Man Remastered | PROSPEROPatches.com</title>",
                "PPSA01467"
            ),
            Some("Marvel's Spider-Man Remastered".into())
        );
        assert_eq!(
            title_from(
                "<TITLE> CUSA00900: Bloodborne&amp;Co - ORBISPatches.com </TITLE>",
                "CUSA00900"
            ),
            Some("Bloodborne&Co".into())
        );
    }

    #[test]
    fn the_sites_own_page_title_is_not_a_game() {
        assert_eq!(
            title_from(
                "<title>PROSPEROPatches.com - PlayStation 5 Game Tracker</title>",
                "NPXS39041"
            ),
            None
        );
        assert_eq!(title_from("<title>PPSA01234</title>", "PPSA01234"), None);
    }

    #[test]
    fn covers_are_the_titles_icon_on_the_cdn() {
        let html =
            r#"<meta content="https://cdn.prosperopatches.com/titles/PPSA01467_ab12/icon0.webp">"#;
        assert_eq!(
            cover_from(html, "PPSA01467", "prosperopatches").as_deref(),
            Some("https://cdn.prosperopatches.com/titles/PPSA01467_ab12/icon0.webp")
        );
        assert_eq!(cover_from(html, "PPSA00001", "prosperopatches"), None);
    }
}
