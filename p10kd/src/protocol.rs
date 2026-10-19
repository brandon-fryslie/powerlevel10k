//! The wire contract between a shell and the daemon.
//!
//! Records are `\x1e`-terminated; fields inside a record are `\x1f`-separated. Every
//! field is escaped so that any byte sequence survives the trip: `\x1d` is the escape
//! byte, and `\x1dD`, `\x1dE`, `\x1dF` stand for `\x1d`, `\x1e`, `\x1f`. The shell side
//! (`internal/worker.zsh`) escapes on send; this module unescapes after splitting.
//!
//! Request:  `id \x1f callback \x1f segment \x1f arg…`
//! Reply:    `e<callback> 'fact' 'fact'…`  then  `d<id>`
//!
//! Segments that run a program take the environment to run it in as their first
//! argument: `NAME=value` entries separated by NUL, empty for no environment.
//!
//! [LAW:parse-dont-validate] bytes become a typed [`Request`] exactly once, here. Nothing
//! downstream sees a raw field again. Fields are bytes, not text: a path is whatever
//! bytes the shell's cwd is made of ([LAW:types-are-the-program]).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::time::Duration;

use crate::zsh;

pub const RECORD_END: u8 = 0x1e;
pub const FIELD_SEP: u8 = 0x1f;
pub const ESCAPE: u8 = 0x1d;

/// The first record a shell sends after opening its FIFOs.
pub const HELLO: &[u8] = b"h";

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: Vec<u8>,
    pub callback: Vec<u8>,
    pub segment: Segment,
}

/// Every fact the daemon knows how to sample, with the inputs the sample depends on.
/// Anything a sampler needs that varies per shell (cwd, binary path, host, environment)
/// travels here; anything that is policy (thresholds, formats, regexes) stays in the
/// shell.
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    DiskUsage { path: PathBuf },
    Load,
    Ram,
    Swap,
    Battery,
    Wifi,
    NetIface,
    PublicIp { env: Env, host: OsString, methods: Vec<PublicIpMethod> },
    RustVersion { env: Env, rustc: PathBuf, cwd: PathBuf },
    Gcloud { env: Env, gcloud: PathBuf, configuration: OsString, account: OsString, project_id: OsString },
    Time { interval: Duration },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicIpMethod {
    Dig,
    Curl,
    Wget,
}

/// The environment a program runs in on the shell's behalf: exactly these variables,
/// nothing inherited from the daemon.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Env(pub Vec<(OsString, OsString)>);

impl Env {
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.0.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str()))
    }

    fn parse(field: Vec<u8>) -> Result<Env, ParseError> {
        if field.is_empty() {
            return Ok(Env::default());
        }
        field
            .split(|&b| b == 0)
            .map(|entry| {
                let eq = entry.iter().position(|&b| b == b'=').filter(|&i| i > 0);
                let eq = eq.ok_or_else(|| ParseError::BadEnv(entry.to_vec()))?;
                Ok((OsString::from_vec(entry[..eq].to_vec()), OsString::from_vec(entry[eq + 1..].to_vec())))
            })
            .collect::<Result<_, _>>()
            .map(Env)
    }
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    MissingField(&'static str),
    BadEscape(Vec<u8>),
    UnknownSegment(Vec<u8>),
    Arity { segment: &'static str, expected: usize, got: usize },
    UnknownMethod(Vec<u8>),
    BadInterval(Vec<u8>),
    BadEnv(Vec<u8>),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        match self {
            ParseError::MissingField(name) => write!(f, "request is missing the {name} field"),
            ParseError::BadEscape(field) => write!(f, "field {:?} has a dangling or unknown escape", text(field)),
            ParseError::UnknownSegment(s) => write!(f, "unknown segment {:?}", text(s)),
            ParseError::Arity { segment, expected, got } => {
                write!(f, "segment {segment} takes {expected} argument(s), got {got}")
            }
            ParseError::UnknownMethod(m) => write!(f, "unknown public_ip method {:?}", text(m)),
            ParseError::BadInterval(s) => write!(f, "time interval {:?} is not a positive number", text(s)),
            ParseError::BadEnv(e) => write!(f, "environment entry {:?} is not NAME=value", text(e)),
        }
    }
}

impl std::error::Error for ParseError {}

/// Undo the shell's escaping of one field.
fn unescape(field: &[u8]) -> Result<Vec<u8>, ParseError> {
    let mut out = Vec::with_capacity(field.len());
    let mut bytes = field.iter();
    while let Some(&b) = bytes.next() {
        if b != ESCAPE {
            out.push(b);
            continue;
        }
        out.push(match bytes.next() {
            Some(b'D') => ESCAPE,
            Some(b'E') => RECORD_END,
            Some(b'F') => FIELD_SEP,
            _ => return Err(ParseError::BadEscape(field.to_vec())),
        });
    }
    Ok(out)
}

impl Request {
    /// Parse one record (without its terminating `\x1e`).
    pub fn parse(record: &[u8]) -> Result<Request, ParseError> {
        let mut fields = record.split(|&b| b == FIELD_SEP).map(unescape);
        let id = fields.next().transpose()?.filter(|s| !s.is_empty()).ok_or(ParseError::MissingField("id"))?;
        let callback =
            fields.next().transpose()?.filter(|s| !s.is_empty()).ok_or(ParseError::MissingField("callback"))?;
        let name = fields.next().transpose()?.ok_or(ParseError::MissingField("segment"))?;
        let args: Vec<Vec<u8>> = fields.collect::<Result<_, _>>()?;
        let segment = Segment::parse(&name, args)?;
        Ok(Request { id, callback, segment })
    }
}

fn exact<const N: usize>(segment: &'static str, args: Vec<Vec<u8>>) -> Result<[Vec<u8>; N], ParseError> {
    let got = args.len();
    args.try_into().map_err(|_| ParseError::Arity { segment, expected: N, got })
}

fn path(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes))
}

impl Segment {
    fn parse(name: &[u8], args: Vec<Vec<u8>>) -> Result<Segment, ParseError> {
        Ok(match name {
            b"disk_usage" => {
                let [p] = exact::<1>("disk_usage", args)?;
                Segment::DiskUsage { path: path(p) }
            }
            b"load" => { exact::<0>("load", args)?; Segment::Load }
            b"ram" => { exact::<0>("ram", args)?; Segment::Ram }
            b"swap" => { exact::<0>("swap", args)?; Segment::Swap }
            b"battery" => { exact::<0>("battery", args)?; Segment::Battery }
            b"wifi" => { exact::<0>("wifi", args)?; Segment::Wifi }
            b"net_iface" => { exact::<0>("net_iface", args)?; Segment::NetIface }
            b"public_ip" => {
                let got = args.len();
                let mut it = args.into_iter();
                let (Some(env), Some(host)) = (it.next(), it.next()) else {
                    return Err(ParseError::Arity { segment: "public_ip", expected: 2, got });
                };
                let methods = it.map(|m| PublicIpMethod::parse(&m)).collect::<Result<_, _>>()?;
                Segment::PublicIp { env: Env::parse(env)?, host: OsString::from_vec(host), methods }
            }
            b"rust_version" => {
                let [env, rustc, cwd] = exact::<3>("rust_version", args)?;
                Segment::RustVersion { env: Env::parse(env)?, rustc: path(rustc), cwd: path(cwd) }
            }
            b"gcloud" => {
                let [env, gcloud, configuration, account, project_id] = exact::<5>("gcloud", args)?;
                Segment::Gcloud {
                    env: Env::parse(env)?,
                    gcloud: path(gcloud),
                    configuration: OsString::from_vec(configuration),
                    account: OsString::from_vec(account),
                    project_id: OsString::from_vec(project_id),
                }
            }
            b"time" => {
                let [interval] = exact::<1>("time", args)?;
                let secs: Option<f64> = std::str::from_utf8(&interval).ok().and_then(|s| s.parse().ok());
                match secs {
                    Some(secs) if secs.is_finite() && secs > 0.0 => Segment::Time { interval: Duration::from_secs_f64(secs) },
                    _ => return Err(ParseError::BadInterval(interval)),
                }
            }
            other => return Err(ParseError::UnknownSegment(other.to_owned())),
        })
    }
}

impl PublicIpMethod {
    fn parse(s: &[u8]) -> Result<Self, ParseError> {
        match s {
            b"dig" => Ok(PublicIpMethod::Dig),
            b"curl" => Ok(PublicIpMethod::Curl),
            b"wget" => Ok(PublicIpMethod::Wget),
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
    out.extend_from_slice(&req.callback);
    for fact in &facts.0 {
        out.push(b' ');
        out.extend_from_slice(zsh::quote(fact).as_bytes());
    }
    out.push(RECORD_END);
    out.extend_from_slice(&render_done(&req.id));
    out
}

/// `d<id>\x1e` alone: the request finished without a fact to deliver.
pub fn render_done(id: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(id.len() + 2);
    out.push(b'd');
    out.extend_from_slice(id);
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

    fn rec(fields: &[&[u8]]) -> Vec<u8> {
        fields.join(&FIELD_SEP)
    }

    fn env(entries: &[(&str, &str)]) -> Env {
        Env(entries.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))).collect())
    }

    #[test]
    fn parses_each_segment_with_its_arity() {
        let r = Request::parse(&rec(&[b"disk_usage", b"_cb", b"disk_usage", b"/tmp/x y"])).unwrap();
        assert_eq!(r.id, b"disk_usage");
        assert_eq!(r.callback, b"_cb");
        assert_eq!(r.segment, Segment::DiskUsage { path: "/tmp/x y".into() });

        assert_eq!(Request::parse(&rec(&[b"l", b"cb", b"load"])).unwrap().segment, Segment::Load);
        assert_eq!(
            Request::parse(&rec(&[b"p", b"cb", b"public_ip", b"PATH=/bin\0HOME=/h", b"https://v4.ident.me/", b"dig", b"curl"]))
                .unwrap()
                .segment,
            Segment::PublicIp {
                env: env(&[("PATH", "/bin"), ("HOME", "/h")]),
                host: "https://v4.ident.me/".into(),
                methods: vec![PublicIpMethod::Dig, PublicIpMethod::Curl],
            }
        );
        assert_eq!(
            Request::parse(&rec(&[b"r", b"cb", b"rust_version", b"", b"/r", b"/d"])).unwrap().segment,
            Segment::RustVersion { env: Env::default(), rustc: "/r".into(), cwd: "/d".into() }
        );
        assert_eq!(
            Request::parse(&rec(&[b"g", b"cb", b"gcloud", b"A=1", b"/g", b"conf", b"me@x", b"proj"])).unwrap().segment,
            Segment::Gcloud {
                env: env(&[("A", "1")]),
                gcloud: "/g".into(),
                configuration: "conf".into(),
                account: "me@x".into(),
                project_id: "proj".into(),
            }
        );
        assert_eq!(
            Request::parse(&rec(&[b"t", b"cb", b"time", b"0.5"])).unwrap().segment,
            Segment::Time { interval: Duration::from_millis(500) }
        );
    }

    #[test]
    fn rejects_malformed_requests() {
        assert_eq!(Request::parse(b"").unwrap_err(), ParseError::MissingField("id"));
        assert_eq!(Request::parse(&rec(&[b"id", b"cb"])).unwrap_err(), ParseError::MissingField("segment"));
        assert_eq!(Request::parse(&rec(&[b"id", b"cb", b"nope"])).unwrap_err(), ParseError::UnknownSegment(b"nope".to_vec()));
        assert_eq!(
            Request::parse(&rec(&[b"id", b"cb", b"load", b"extra"])).unwrap_err(),
            ParseError::Arity { segment: "load", expected: 0, got: 1 }
        );
        assert_eq!(
            Request::parse(&rec(&[b"id", b"cb", b"public_ip", b"", b"h", b"ftp"])).unwrap_err(),
            ParseError::UnknownMethod(b"ftp".to_vec())
        );
        assert_eq!(
            Request::parse(&rec(&[b"id", b"cb", b"public_ip", b""])).unwrap_err(),
            ParseError::Arity { segment: "public_ip", expected: 2, got: 1 }
        );
        assert_eq!(Request::parse(&rec(&[b"id", b"cb", b"time", b"0"])).unwrap_err(), ParseError::BadInterval(b"0".to_vec()));
        assert_eq!(Request::parse(&rec(&[b"id", b"cb", b"time", b"x"])).unwrap_err(), ParseError::BadInterval(b"x".to_vec()));
        assert_eq!(
            Request::parse(&rec(&[b"id", b"cb", b"rust_version", b"novalue", b"/r", b"/d"])).unwrap_err(),
            ParseError::BadEnv(b"novalue".to_vec())
        );
        assert_eq!(
            Request::parse(&rec(&[b"id", b"cb", b"rust_version", b"=v", b"/r", b"/d"])).unwrap_err(),
            ParseError::BadEnv(b"=v".to_vec())
        );
    }

    #[test]
    fn escaped_separators_and_arbitrary_bytes_survive() {
        // What worker.zsh sends for the path "a\x1db\x1ec\x1fd" with a Latin-1 byte.
        let r = Request::parse(&rec(&[b"d", b"cb", b"disk_usage", b"a\x1dDb\x1dEc\x1dFd\xe9"])).unwrap();
        assert_eq!(r.segment, Segment::DiskUsage { path: path(b"a\x1db\x1ec\x1fd\xe9".to_vec()) });
        assert_eq!(Request::parse(&rec(&[b"d", b"cb", b"disk_usage", b"x\x1d"])).unwrap_err(), ParseError::BadEscape(b"x\x1d".to_vec()));
        assert_eq!(Request::parse(&rec(&[b"d", b"cb", b"disk_usage", b"x\x1dZ"])).unwrap_err(), ParseError::BadEscape(b"x\x1dZ".to_vec()));
    }

    #[test]
    fn reply_is_callback_with_quoted_facts_then_done() {
        let req = Request::parse(&rec(&[b"ram", b"_p9k_prompt_ram_sync", b"ram"])).unwrap();
        let out = render_reply(&req, &Facts::new(["123", "it's"]));
        assert_eq!(out, b"e_p9k_prompt_ram_sync '123' 'it'\\''s'\x1edram\x1e".to_vec());
        assert_eq!(render_done(b"ram"), b"dram\x1e".to_vec());
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
