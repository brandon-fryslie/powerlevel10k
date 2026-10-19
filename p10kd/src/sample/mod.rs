//! Each sampler is a function from a [`Segment`]'s inputs to its [`Facts`]: the daemon's
//! whole purpose, and the one place effects happen ([LAW:effects-at-boundaries]). Policy
//! (thresholds, formatting, regex matching) never lives here; it lives in the shell.
//!
//! The positional order of each sampler's facts is the contract with the matching
//! `_p9k_prompt_<segment>_sync` function in `internal/p10k.zsh`.

use std::fmt;

use crate::protocol::{Facts, Segment};

mod battery;
mod disk;
mod exec;
mod load;
mod memory;
mod netiface;
mod public_ip;
mod time;
mod wifi;

/// A sampler could not do its job. This is reported, never hidden: the daemon logs it
/// and tells the shell the request finished without facts ([LAW:no-silent-failure]).
#[derive(Debug)]
pub struct SampleError(pub String);

impl fmt::Display for SampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SampleError {}

impl From<std::io::Error> for SampleError {
    fn from(e: std::io::Error) -> Self {
        SampleError(e.to_string())
    }
}

pub type SampleResult = Result<Facts, SampleError>;

pub fn sample(segment: &Segment) -> SampleResult {
    match segment {
        Segment::DiskUsage { path } => disk::sample(path),
        Segment::Load => load::sample(),
        Segment::Ram => memory::ram(),
        Segment::Swap => memory::swap(),
        Segment::Battery => battery::sample(),
        Segment::Wifi => wifi::sample(),
        Segment::NetIface => netiface::sample(),
        Segment::PublicIp { env, host, methods } => public_ip::sample(env, host, methods),
        Segment::RustVersion { env, rustc, cwd } => exec::rust_version(env, rustc, cwd),
        Segment::Gcloud { env, gcloud, configuration, account, project_id } => {
            exec::gcloud_project_name(env, gcloud, configuration, account, project_id)
        }
        Segment::Time { interval } => time::sample(*interval),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("p10kd samples macOS and Linux; port the samplers before building elsewhere");
