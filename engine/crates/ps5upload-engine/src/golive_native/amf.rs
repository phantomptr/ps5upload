//! AMF0, the values RTMP commands and FLV's onMetaData carry: what a publisher writes, and what
//! a streaming site answers (`_result`, `_error`, `onStatus`). Unknown or nested-too-deep values
//! from a server end the parse instead of looping or recursing without bound.

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Amf {
    Number(f64),
    Bool(bool),
    Str(String),
    Object(Vec<(String, Amf)>),
    Null,
    Undefined,
    EcmaArray(Vec<(String, Amf)>),
    StrictArray(Vec<Amf>),
}

impl Amf {
    pub(crate) fn str(s: &str) -> Amf {
        Amf::Str(s.to_string())
    }

    pub(crate) fn obj(props: &[(&str, Amf)]) -> Amf {
        Amf::Object(
            props
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    /// A property of an object or ECMA array.
    pub(crate) fn get(&self, key: &str) -> Option<&Amf> {
        match self {
            Amf::Object(p) | Amf::EcmaArray(p) => p.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Amf::Str(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Amf::Number(n) => Some(*n),
            _ => None,
        }
    }
}

fn put_key(out: &mut Vec<u8>, k: &str) {
    let b = k.as_bytes();
    let n = b.len().min(0xffff);
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&b[..n]);
}

fn put_props(out: &mut Vec<u8>, props: &[(String, Amf)]) {
    for (k, v) in props {
        put_key(out, k);
        write(out, v);
    }
    out.extend_from_slice(&[0, 0, 9]);
}

pub(crate) fn write(out: &mut Vec<u8>, v: &Amf) {
    match v {
        Amf::Number(n) => {
            out.push(0x00);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Amf::Bool(b) => out.extend_from_slice(&[0x01, *b as u8]),
        Amf::Str(s) => {
            if s.len() > 0xffff {
                out.push(0x0c);
                out.extend_from_slice(&(s.len() as u32).to_be_bytes());
                out.extend_from_slice(s.as_bytes());
            } else {
                out.push(0x02);
                put_key(out, s);
            }
        }
        Amf::Object(p) => {
            out.push(0x03);
            put_props(out, p);
        }
        Amf::Null => out.push(0x05),
        Amf::Undefined => out.push(0x06),
        Amf::EcmaArray(p) => {
            out.push(0x08);
            out.extend_from_slice(&(p.len() as u32).to_be_bytes());
            put_props(out, p);
        }
        Amf::StrictArray(a) => {
            out.push(0x0a);
            out.extend_from_slice(&(a.len() as u32).to_be_bytes());
            for x in a {
                write(out, x);
            }
        }
    }
}

pub(crate) fn encode(vals: &[Amf]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in vals {
        write(&mut out, v);
    }
    out
}

/// Nesting deeper than this from a server is not a real answer.
const MAX_DEPTH: u32 = 16;

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn f64(&mut self) -> Option<f64> {
        self.take(8).map(|s| {
            let mut a = [0u8; 8];
            a.copy_from_slice(s);
            f64::from_be_bytes(a)
        })
    }
    fn string(&mut self, n: usize) -> Option<String> {
        self.take(n)
            .map(|s| String::from_utf8_lossy(s).into_owned())
    }

    fn props(&mut self, depth: u32) -> Option<Vec<(String, Amf)>> {
        let mut v = Vec::new();
        loop {
            if self.at >= self.b.len() {
                // Some servers leave the end marker off the last object.
                return Some(v);
            }
            let n = self.u16()? as usize;
            if n == 0 {
                // The end marker (0x09), or the end of the data.
                if self.b.get(self.at) == Some(&0x09) {
                    self.at += 1;
                }
                return Some(v);
            }
            let k = self.string(n)?;
            let val = self.value(depth + 1)?;
            v.push((k, val));
        }
    }

    fn value(&mut self, depth: u32) -> Option<Amf> {
        if depth > MAX_DEPTH {
            return None;
        }
        Some(match self.u8()? {
            0x00 => Amf::Number(self.f64()?),
            0x01 => Amf::Bool(self.u8()? != 0),
            0x02 => {
                let n = self.u16()? as usize;
                Amf::Str(self.string(n)?)
            }
            0x03 => Amf::Object(self.props(depth)?),
            0x05 => Amf::Null,
            0x06 | 0x0d => Amf::Undefined,
            // A reference to an earlier object: not something an answer needs.
            0x07 => {
                self.u16()?;
                Amf::Undefined
            }
            0x08 => {
                self.u32()?;
                Amf::EcmaArray(self.props(depth)?)
            }
            0x0a => {
                let n = self.u32()? as usize;
                let mut a = Vec::new();
                for _ in 0..n {
                    a.push(self.value(depth + 1)?);
                }
                Amf::StrictArray(a)
            }
            0x0b => {
                let t = self.f64()?;
                self.u16()?;
                Amf::Number(t)
            }
            0x0c | 0x0f => {
                let n = self.u32()? as usize;
                Amf::Str(self.string(n)?)
            }
            0x10 => {
                let n = self.u16()? as usize;
                self.take(n)?;
                Amf::Object(self.props(depth)?)
            }
            _ => return None,
        })
    }
}

/// The values in `b`, up to the first one that can't be read.
pub(crate) fn decode(b: &[u8]) -> Vec<Amf> {
    let mut r = Reader { b, at: 0 };
    let mut v = Vec::new();
    while r.at < b.len() {
        match r.value(0) {
            Some(x) => v.push(x),
            None => break,
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_kind() {
        let vals = vec![
            Amf::str("connect"),
            Amf::Number(1.0),
            Amf::obj(&[
                ("app", Amf::str("live2")),
                ("fpad", Amf::Bool(false)),
                ("n", Amf::Null),
            ]),
            Amf::Null,
            Amf::Undefined,
            Amf::EcmaArray(vec![("width".into(), Amf::Number(1920.0))]),
            Amf::StrictArray(vec![Amf::Number(2.0), Amf::str("x")]),
            Amf::Str("y".repeat(70_000)),
        ];
        assert_eq!(decode(&encode(&vals)), vals);
    }

    #[test]
    fn the_wire_bytes_of_a_string_and_a_number() {
        assert_eq!(encode(&[Amf::str("ab")]), [0x02, 0, 2, b'a', b'b']);
        assert_eq!(
            encode(&[Amf::Number(1.0)]),
            [0x00, 0x3f, 0xf0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            encode(&[Amf::obj(&[])]),
            [0x03, 0, 0, 9],
            "an empty object is just its end marker"
        );
    }

    #[test]
    fn a_damaged_or_hostile_answer_stops_the_parse() {
        // A string longer than the data.
        assert_eq!(decode(&[0x02, 0xff, 0xff, b'a']), vec![]);
        // An unknown type after a good value.
        let mut b = encode(&[Amf::Number(3.0)]);
        b.push(0x42);
        assert_eq!(decode(&b), vec![Amf::Number(3.0)]);
        // Objects nested past the limit.
        let mut deep = Vec::new();
        for _ in 0..40 {
            deep.extend_from_slice(&[0x03, 0, 1, b'k']);
        }
        assert_eq!(decode(&deep), vec![]);
        // A strict array claiming more items than there are.
        assert_eq!(decode(&[0x0a, 0xff, 0xff, 0xff, 0xff]), vec![]);
    }

    #[test]
    fn an_object_without_its_end_marker_still_reads() {
        let mut b = vec![0x03];
        b.extend_from_slice(&[0, 4]);
        b.extend_from_slice(b"code");
        write(&mut b, &Amf::str("NetStream.Publish.Start"));
        let v = decode(&b);
        assert_eq!(
            v[0].get("code").and_then(Amf::as_str),
            Some("NetStream.Publish.Start")
        );
    }
}
