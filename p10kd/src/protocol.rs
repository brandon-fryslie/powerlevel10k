//! The wire contract between a shell and the daemon.
//!
//! Records are `\x1e`-terminated; fields inside a record are `\x1f`-separated. These are
//! the separators `internal/worker.zsh` has always used, so the shell side splits them
//! with `${(ps:\x1e:)buf}` as before.
//!
//! Request:  `id \x1f callback \x1f segment \x1f arg…`
//! Reply:    `e<callback> 'fact' 'fact'…`  then  `d<id>`
//!
//! [LAW:parse-dont-validate] bytes become a typed [`Request`] exactly once, here. Nothing
//! downstream sees a raw field again.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::zsh;

pub const RECORD_END: u8 = 0x1e;
pub const FIELD_SEP: u8 = 0x1f;

/// The first record a shell sends after opening its FIFOs.
pub const HELLO: &[u8] = b"h";

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: String,
    pub callback: String,
    pub segment: Segment,
}

/// Every fact the daemon knows how to sample, with the inputs the sample depends on.
/// Anything a sampler needs that varies per shell (cwd, binary path, host) travels here;
/// anything that is policy (thresholds, formats, regexes) stays in the shell.
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    DiskUsage { path: PathBuf },
    Load,
    Ram,
    Swap,
    Battery,
    Wifi,
    NetIface,
    PublicIp { host: String, methods: Vec<PublicIpMethod> },
    RustVersion { rustc: PathBuf, cwd: PathBuf },
    Gcloud { gcloud: PathBuf, configuration: String, account: String, project_id: String },
    Time { interval: Duration },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicIpMethod {
    Dig,
    Curl,
    Wget,
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    MissingField(&'static str),
    UnknownSegment(String),
    Arity { segment: &'static str, expected: usize, got: usize },
    UnknownMethod(String),
    BadInterval(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::MissingField(name) => write!(f, "request is missing the {name} field"),
            ParseError::UnknownSegment(s) => write!(f, "unknown segment {s:?}"),
            ParseError::Arity { segment, expected, got } => {
                write!(f, "segment {segment} takes {expected} argument(s), got {got}")
            }
            ParseError::UnknownMethod(m) => write!(f, "unknown public_ip method {m:?}"),
            ParseError::BadInterval(s) => write!(f, "time interval {s:?} is not a positive number"),
        }
    }
}

impl std::error::Error for ParseError {}

impl Request {
    /// Parse one record (without its terminating `\x1e`).
    pub fn parse(record: &[u8]) -> Result<Request, ParseError> {
        let mut fields = record
            .split(|&b| b == FIELD_SEP)
            .map(|f| String::from_utf8_lossy(f).into_owned());
        let id = fields.next().filter(|s| !s.is_empty()).ok_or(ParseError::MissingField("id"))?;
        let callback = fields.next().filter(|s| !s.is_empty()).ok_or(ParseError::MissingField("callback"))?;
        let name = fields.next().ok_or(ParseError::MissingField("segment"))?;
        let args: Vec<String> = fields.collect();
        let segment = Segment::parse(&name, args)?;
        Ok(Request { id, callback, segment })
    }
}

fn exact<const N: usize>(segment: &'static str, args: Vec<String>) -> Result<[String; N], ParseError> {
    let got = args.len();
    args.try_into().map_err(|_| ParseError::Arity { segment, expected: N, got })
}

impl Segment {
    fn parse(name: &str, args: Vec<String>) -> Result<Segment, ParseError> {
        Ok(match name {
            "disk_usage" => {
                let [path] = exact::<1>("disk_usage", args)?;
                Segment::DiskUsage { path: path.into() }
            }
            "load" => { exact::<0>("load", args)?; Segment::Load }
            "ram" => { exact::<0>("ram", args)?; Segment::Ram }
            "swap" => { exact::<0>("swap", args)?; Segment::Swap }
            "battery" => { exact::<0>("battery", args)?; Segment::Battery }
            "wifi" => { exact::<0>("wifi", args)?; Segment::Wifi }
            "net_iface" => { exact::<0>("net_iface", args)?; Segment::NetIface }
            "public_ip" => {
                let mut it = args.into_iter();
                let host = it.next().ok_or(ParseError::Arity { segment: "public_ip", expected: 1, got: 0 })?;
                let methods = it.map(|m| PublicIpMethod::parse(&m)).collect::<Result<_, _>>()?;
                Segment::PublicIp { host, methods }
            }
            "rust_version" => {
                let [rustc, cwd] = exact::<2>("rust_version", args)?;
                Segment::RustVersion { rustc: rustc.into(), cwd: cwd.into() }
            }
            "gcloud" => {
                let [gcloud, configuration, account, project_id] = exact::<4>("gcloud", args)?;
                Segment::Gcloud { gcloud: gcloud.into(), configuration, account, project_id }
            }
            "time" => {
                let [interval] = exact::<1>("time", args)?;
                let secs: f64 = interval.parse().map_err(|_| ParseError::BadInterval(interval.clone()))?;
                if !(secs.is_finite() && secs > 0.0) {
                    return Err(ParseError::BadInterval(interval));
                }
                Segment::Time { interval: Duration::from_secs_f64(secs) }
            }
            other => return Err(ParseError::UnknownSegment(other.to_owned())),
        })
    }
}

impl PublicIpMethod {
    fn parse(s: &str) -> Result<Self, ParseError> {
        match s {
            "dig" => Ok(PublicIpMethod::Dig),
            "curl" => Ok(PublicIpMethod::Curl),
            "wget" => Ok(PublicIpMethod::Wget),
            other => Err(ParseError::UnknownMethod(other.to_owned())),
        }
    }
}

/// The facts a sampler produced, in the positional order the shell callback expects.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Facts(pub Vec<String>);

impl Facts {
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(items: I) -> Facts {
        Facts(items.into_iter().map(Into::into).collect())
    }
}

/// `e<callback> 'fact'…\x1e` followed by `d<id>\x1e`. The shell evaluates the first and
/// uses the second to release the next queued request for that id.
pub fn render_reply(req: &Request, facts: &Facts) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(b'e');
    out.extend_from_slice(req.callback.as_bytes());
    for fact in &facts.0 {
        out.push(b' ');
        out.extend_from_slice(zsh::quote(fact).as_bytes());
    }
    out.push(RECORD_END);
    out.extend_from_slice(&render_done(&req.id));
    out
}

/// `d<id>\x1e` alone: the request finished without a fact to deliver.
pub fn render_done(id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(id.len() + 2);
    out.push(b'd');
    out.extend_from_slice(id.as_bytes());
    out.push(RECORD_END);
    out
}

/// Splits a growing byte buffer into complete records, leaving any partial tail in place.
pub struct RecordBuffer {
    buf: Vec<u8>,
}

impl RecordBuffer {
    pub fn new() -> Self {
        RecordBuffer { buf: Vec::new() }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Removes and returns the next complete record, without its terminator.
    pub fn next_record(&mut self) -> Option<Vec<u8>> {
        let end = self.buf.iter().position(|&b| b == RECORD_END)?;
        let record = self.buf[..end].to_vec();
        self.buf.drain(..=end);
        Some(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(fields: &[&str]) -> Vec<u8> {
        fields.join("\x1f").into_bytes()
    }

    #[test]
    fn parses_each_segment_with_its_arity() {
        let r = Request::parse(&rec(&["disk_usage", "_cb", "disk_usage", "/tmp/x y"])).unwrap();
        assert_eq!(r.id, "disk_usage");
        assert_eq!(r.callback, "_cb");
        assert_eq!(r.segment, Segment::DiskUsage { path: "/tmp/x y".into() });

        assert_eq!(Request::parse(&rec(&["l", "cb", "load"])).unwrap().segment, Segment::Load);
        assert_eq!(
            Request::parse(&rec(&["p", "cb", "public_ip", "https://v4.ident.me/", "dig", "curl"])).unwrap().segment,
            Segment::PublicIp { host: "https://v4.ident.me/".into(), methods: vec![PublicIpMethod::Dig, PublicIpMethod::Curl] }
        );
        assert_eq!(
            Request::parse(&rec(&["g", "cb", "gcloud", "/g", "conf", "me@x", "proj"])).unwrap().segment,
            Segment::Gcloud { gcloud: "/g".into(), configuration: "conf".into(), account: "me@x".into(), project_id: "proj".into() }
        );
        assert_eq!(
            Request::parse(&rec(&["t", "cb", "time", "0.5"])).unwrap().segment,
            Segment::Time { interval: Duration::from_millis(500) }
        );
    }

    #[test]
    fn rejects_malformed_requests() {
        assert_eq!(Request::parse(b"").unwrap_err(), ParseError::MissingField("id"));
        assert_eq!(Request::parse(&rec(&["id", "cb"])).unwrap_err(), ParseError::MissingField("segment"));
        assert_eq!(Request::parse(&rec(&["id", "cb", "nope"])).unwrap_err(), ParseError::UnknownSegment("nope".into()));
        assert_eq!(
            Request::parse(&rec(&["id", "cb", "load", "extra"])).unwrap_err(),
            ParseError::Arity { segment: "load", expected: 0, got: 1 }
        );
        assert_eq!(Request::parse(&rec(&["id", "cb", "public_ip", "h", "ftp"])).unwrap_err(), ParseError::UnknownMethod("ftp".into()));
        assert_eq!(Request::parse(&rec(&["id", "cb", "time", "0"])).unwrap_err(), ParseError::BadInterval("0".into()));
        assert_eq!(Request::parse(&rec(&["id", "cb", "time", "x"])).unwrap_err(), ParseError::BadInterval("x".into()));
    }

    #[test]
    fn reply_is_callback_with_quoted_facts_then_done() {
        let req = Request::parse(&rec(&["ram", "_p9k_prompt_ram_sync", "ram"])).unwrap();
        let out = render_reply(&req, &Facts::new(["123", "it's"]));
        assert_eq!(out, b"e_p9k_prompt_ram_sync '123' 'it'\\''s'\x1edram\x1e".to_vec());
        assert_eq!(render_done("ram"), b"dram\x1e".to_vec());
    }

    #[test]
    fn record_buffer_yields_complete_records_and_keeps_tail() {
        let mut rb = RecordBuffer::new();
        rb.push(b"a\x1eb");
        assert_eq!(rb.next_record(), Some(b"a".to_vec()));
        assert_eq!(rb.next_record(), None);
        rb.push(b"c\x1e\x1e");
        assert_eq!(rb.next_record(), Some(b"bc".to_vec()));
        assert_eq!(rb.next_record(), Some(b"".to_vec()));
        assert_eq!(rb.next_record(), None);
    }
}
