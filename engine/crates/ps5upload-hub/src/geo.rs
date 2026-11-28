//! Coarse location for room cards and latency estimates, with no address kept.
//!
//! An optional IP → country table (`HUB_GEO_FILE`: CSV lines `start,end,CC`, the format of the
//! CC BY 4.0 "IP to Country Lite" download) is held in memory as range starts only: about 6 bytes
//! an IPv4 range and 10 an IPv6 one (IPv6 at /64 granularity, which is all [`IpKey`] keeps). A
//! lookup gives a country; the hub turns it into one of 12 coarse regions at once and keeps only
//! that (the address itself is never stored with a card). Without the file there is no country,
//! and regions come from the hosts' and viewers' own time-zone regions (sent by the apps).
//!
//! Estimates are region pairs (the design's fallback table): same region about 30 ms, neighbours
//! 70, same continent 100, otherwise 160.

use std::net::IpAddr;

use crate::limits::IpKey;

/// The 12 coarse regions (fixed list, shared with the apps).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Region {
    NaWest,
    NaEast,
    CentralAm,
    SouthAm,
    EuWest,
    EuEast,
    MiddleEast,
    Africa,
    AsiaSouth,
    AsiaEast,
    AsiaSe,
    Oceania,
}

impl Region {
    pub const ALL: [Region; 12] = [
        Region::NaWest,
        Region::NaEast,
        Region::CentralAm,
        Region::SouthAm,
        Region::EuWest,
        Region::EuEast,
        Region::MiddleEast,
        Region::Africa,
        Region::AsiaSouth,
        Region::AsiaEast,
        Region::AsiaSe,
        Region::Oceania,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Region::NaWest => "na-west",
            Region::NaEast => "na-east",
            Region::CentralAm => "central-am",
            Region::SouthAm => "south-am",
            Region::EuWest => "eu-west",
            Region::EuEast => "eu-east",
            Region::MiddleEast => "middle-east",
            Region::Africa => "africa",
            Region::AsiaSouth => "asia-south",
            Region::AsiaEast => "asia-east",
            Region::AsiaSe => "asia-se",
            Region::Oceania => "oceania",
        }
    }

    pub fn parse(s: &str) -> Option<Region> {
        Region::ALL.into_iter().find(|r| r.as_str() == s)
    }

    fn continent(self) -> u8 {
        match self {
            Region::NaWest | Region::NaEast | Region::CentralAm => 0,
            Region::SouthAm => 1,
            Region::EuWest | Region::EuEast => 2,
            Region::MiddleEast => 3,
            Region::Africa => 4,
            Region::AsiaSouth | Region::AsiaEast | Region::AsiaSe => 5,
            Region::Oceania => 6,
        }
    }

    pub fn same_continent(self, o: Region) -> bool {
        self.continent() == o.continent()
    }

    pub fn neighbours(self, o: Region) -> bool {
        use Region::*;
        const PAIRS: [(Region, Region); 12] = [
            (NaWest, NaEast),
            (NaWest, CentralAm),
            (NaEast, CentralAm),
            (CentralAm, SouthAm),
            (EuWest, EuEast),
            (EuEast, MiddleEast),
            (EuWest, Africa),
            (MiddleEast, Africa),
            (MiddleEast, AsiaSouth),
            (AsiaSouth, AsiaSe),
            (AsiaSe, AsiaEast),
            (AsiaSe, Oceania),
        ];
        PAIRS
            .iter()
            .any(|&(a, b)| (a == self && b == o) || (a == o && b == self))
    }

    /// Same region or a neighbour ("Near me").
    pub fn near(self, o: Region) -> bool {
        self == o || self.neighbours(o)
    }
}

/// The round trip one can expect between two regions, in ms (an estimate: shown as "about").
pub fn est_ms(a: Region, b: Region) -> u32 {
    if a == b {
        30
    } else if a.neighbours(b) {
        70
    } else if a.same_continent(b) {
        100
    } else {
        160
    }
}

/// The region of a country (ISO 3166 alpha-2), by its most populous part. The United States and
/// Canada count as "na-east"; an app's own time-zone region in the same continent refines it
/// ([`refine`]).
pub fn country_region(cc: [u8; 2]) -> Option<Region> {
    let cc = std::str::from_utf8(&cc).ok()?;
    const TABLE: [(Region, &str); 11] = [
        (Region::NaEast, "US CA PM"),
        (
            Region::CentralAm,
            "MX GT BZ SV HN NI CR PA CU JM HT DO PR BS BB TT AG DM GD KN LC VC AW CW BQ SX KY TC \
             VG VI AI MS GP MQ BL MF BM",
        ),
        (Region::SouthAm, "BR AR CL CO PE VE EC BO PY UY GY SR GF FK"),
        (
            Region::EuWest,
            "GB IE FR BE NL LU DE AT CH LI ES PT AD IT SM VA MT MC IS NO SE DK FI AX FO GI IM JE \
             GG GL SJ",
        ),
        (
            Region::EuEast,
            "PL CZ SK HU SI HR BA RS ME MK AL XK RO BG GR CY MD UA BY LT LV EE RU TR",
        ),
        (
            Region::MiddleEast,
            "IL PS JO LB SY IQ IR KW SA BH QA AE OM YE GE AM AZ EG",
        ),
        (
            Region::Africa,
            "DZ MA TN LY SD SS ET ER DJ SO KE UG TZ RW BI CD CG CF CM GA GQ ST NG NE TD ML BF SN \
             GM GW GN SL LR CI GH TG BJ MR CV AO ZM ZW MW MZ MG MU SC KM RE YT NA BW ZA LS SZ EH SH",
        ),
        (
            Region::AsiaSouth,
            "IN PK BD LK NP BT MV AF KZ UZ TM KG TJ",
        ),
        (Region::AsiaEast, "CN HK MO TW JP KR KP MN"),
        (Region::AsiaSe, "SG MY ID TH VN PH KH LA MM BN TL"),
        (
            Region::Oceania,
            "AU NZ PG FJ SB VU NC PF WS TO KI TV NR FM MH PW GU MP AS CK NU",
        ),
    ];
    TABLE
        .iter()
        .find(|(_, list)| list.split_ascii_whitespace().any(|c| c == cc))
        .map(|(r, _)| *r)
}

/// The region to show: the country's, refined by the app's own (time-zone) region when that is
/// in the same continent (a US host in Pacific time is "na-west"); the app's alone when there is
/// no country. A VPN in another continent keeps the country's: it is where the traffic goes.
pub fn refine(from_country: Option<Region>, own: Option<Region>) -> Option<Region> {
    match (from_country, own) {
        (Some(c), Some(o)) if c.same_continent(o) => Some(o),
        (Some(c), _) => Some(c),
        (None, o) => o,
    }
}

const NONE: [u8; 2] = *b"--";

/// IP ranges → country, as sorted range starts (a gap is a start with no country).
#[derive(Default)]
pub struct Geo {
    v4: Vec<u32>,
    v4cc: Vec<[u8; 2]>,
    v6: Vec<u64>,
    v6cc: Vec<[u8; 2]>,
}

impl Geo {
    /// Parses `start,end,CC` lines (quotes allowed, other lines skipped). Returns the table and
    /// how many lines were skipped.
    pub fn parse(text: &str) -> (Geo, usize) {
        let mut v4: Vec<(u32, u32, [u8; 2])> = Vec::new();
        let mut v6: Vec<(u64, u64, [u8; 2])> = Vec::new();
        let mut bad = 0;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut f = line.split(',').map(|s| s.trim().trim_matches('"'));
            let (Some(a), Some(b), Some(cc)) = (f.next(), f.next(), f.next()) else {
                bad += 1;
                continue;
            };
            let cc = cc.as_bytes();
            if cc.len() != 2 || !cc.iter().all(u8::is_ascii_alphabetic) {
                bad += 1;
                continue;
            }
            let cc = [cc[0].to_ascii_uppercase(), cc[1].to_ascii_uppercase()];
            match (a.parse::<IpAddr>(), b.parse::<IpAddr>()) {
                (Ok(IpAddr::V4(a)), Ok(IpAddr::V4(b))) if a <= b => {
                    v4.push((u32::from(a), u32::from(b), cc))
                }
                (Ok(IpAddr::V6(a)), Ok(IpAddr::V6(b))) if a <= b => v6.push((
                    (u128::from(a) >> 64) as u64,
                    (u128::from(b) >> 64) as u64,
                    cc,
                )),
                _ => bad += 1,
            }
        }
        let (v4, v4cc) = starts(v4);
        let (v6, v6cc) = starts(v6);
        (Geo { v4, v4cc, v6, v6cc }, bad)
    }

    pub fn load(path: &std::path::Path) -> std::io::Result<(Geo, usize)> {
        Ok(Self::parse(&std::fs::read_to_string(path)?))
    }

    pub fn country(&self, key: IpKey) -> Option<[u8; 2]> {
        let (cc, i) = match key {
            IpKey::V4(ip) => (&self.v4cc, self.v4.partition_point(|s| *s <= ip)),
            IpKey::V6Prefix(p) => (&self.v6cc, self.v6.partition_point(|s| *s <= p)),
        };
        let c = *cc.get(i.checked_sub(1)?)?;
        (c != NONE).then_some(c)
    }

    /// Range starts held.
    pub fn len(&self) -> usize {
        self.v4.len() + self.v6.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes the table holds (for the memory budget).
    pub fn bytes(&self) -> usize {
        self.v4.capacity() * 4
            + self.v4cc.capacity() * 2
            + self.v6.capacity() * 8
            + self.v6cc.capacity() * 2
    }
}

/// Sorted, merged range starts: adjacent ranges of one country become one, and a gap gets a
/// start with no country.
fn starts<T>(mut r: Vec<(T, T, [u8; 2])>) -> (Vec<T>, Vec<[u8; 2]>)
where
    T: Copy + Ord + num_like::Succ,
{
    r.sort_by_key(|x| x.0);
    let mut s: Vec<T> = Vec::with_capacity(r.len());
    let mut c: Vec<[u8; 2]> = Vec::with_capacity(r.len());
    let mut end: Option<T> = None;
    for (a, b, cc) in r {
        // Overlaps (or a v6 range inside a /64 already covered): the first one wins.
        if end.is_some_and(|e| a <= e) {
            if end.is_some_and(|e| b > e) && c.last() == Some(&cc) {
                end = Some(b);
            }
            continue;
        }
        let contiguous = end.is_some_and(|e| e.succ() == Some(a));
        if !contiguous {
            if end.is_some() {
                s.push(end.and_then(|e| e.succ()).expect("a gap follows a range"));
                c.push(NONE);
            }
            s.push(a);
            c.push(cc);
        } else if c.last() != Some(&cc) {
            s.push(a);
            c.push(cc);
        }
        end = Some(b);
    }
    if let Some(e) = end.and_then(|e| e.succ()) {
        s.push(e);
        c.push(NONE);
    }
    s.shrink_to_fit();
    c.shrink_to_fit();
    (s, c)
}

mod num_like {
    pub trait Succ: Sized {
        fn succ(self) -> Option<Self>;
    }
    impl Succ for u32 {
        fn succ(self) -> Option<u32> {
            self.checked_add(1)
        }
    }
    impl Succ for u64 {
        fn succ(self) -> Option<u64> {
            self.checked_add(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> IpKey {
        IpKey::of(s.parse().unwrap())
    }

    #[test]
    fn lookups_hit_ranges_and_miss_gaps() {
        let (g, bad) = Geo::parse(
            "1.0.0.0,1.0.0.255,AU\n\
             1.0.1.0,1.0.3.255,CN\n\
             \"1.0.4.0\",\"1.0.7.255\",\"AU\"\n\
             2.0.0.0,2.0.0.255,FR\n\
             2001:db8::,2001:db8:0:ffff:ffff:ffff:ffff:ffff,DE\n\
             garbage\n\
             9.9.9.9,1.1.1.1,US\n",
        );
        assert_eq!(bad, 2);
        assert_eq!(g.country(key("1.0.0.7")), Some(*b"AU"));
        assert_eq!(g.country(key("1.0.2.1")), Some(*b"CN"));
        assert_eq!(g.country(key("1.0.5.1")), Some(*b"AU"));
        assert_eq!(g.country(key("1.0.8.0")), None, "gap");
        assert_eq!(g.country(key("0.0.0.1")), None, "before");
        assert_eq!(g.country(key("2.0.0.9")), Some(*b"FR"));
        assert_eq!(g.country(key("3.0.0.0")), None, "after");
        assert_eq!(g.country(key("2001:db8:0:5::1")), Some(*b"DE"));
        assert_eq!(g.country(key("2001:db9::1")), None);
        assert_eq!(g.country(key("::ffff:1.0.0.7")), Some(*b"AU"));
    }

    #[test]
    fn adjacent_ranges_of_one_country_merge() {
        let (g, _) = Geo::parse("10.0.0.0,10.0.0.255,DE\n10.0.1.0,10.0.1.255,DE\n");
        // One start and the end marker.
        assert_eq!(g.len(), 2);
        assert_eq!(g.country(key("10.0.1.9")), Some(*b"DE"));
    }

    #[test]
    fn regions_and_estimates() {
        assert_eq!(country_region(*b"DE"), Some(Region::EuWest));
        assert_eq!(country_region(*b"JP"), Some(Region::AsiaEast));
        assert_eq!(country_region(*b"US"), Some(Region::NaEast));
        assert_eq!(country_region(*b"ZZ"), None);
        for r in Region::ALL {
            assert_eq!(Region::parse(r.as_str()), Some(r));
            assert_eq!(est_ms(r, r), 30);
        }
        assert_eq!(est_ms(Region::EuWest, Region::EuEast), 70);
        assert_eq!(est_ms(Region::AsiaSouth, Region::AsiaEast), 100);
        assert_eq!(est_ms(Region::EuWest, Region::Oceania), 160);
        // A US host in Pacific time; a VPN abroad keeps the country's region.
        assert_eq!(
            refine(Some(Region::NaEast), Some(Region::NaWest)),
            Some(Region::NaWest)
        );
        assert_eq!(
            refine(Some(Region::EuWest), Some(Region::NaWest)),
            Some(Region::EuWest)
        );
        assert_eq!(refine(None, Some(Region::Africa)), Some(Region::Africa));
        assert_eq!(refine(None, None), None);
    }
}
