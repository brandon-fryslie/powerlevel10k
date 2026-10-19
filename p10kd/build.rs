//! Gives the binary an identity: a hash of everything that shapes it. Two builds from
//! the same sources, profile and target share a daemon; any other pair get separate ones,
//! so a rebuilt p10kd never talks to a daemon speaking an older protocol.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            sources(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Everything that shapes the binary: hashed into its id, and what makes this script re-run.
const ROOTS: [&str; 4] = ["build.rs", "Cargo.toml", "Cargo.lock", "src"];

fn main() {
    let mut files = Vec::new();
    for root in ROOTS {
        println!("cargo:rerun-if-changed={root}");
        let path = PathBuf::from(root);
        if path.is_dir() {
            sources(&path, &mut files);
        } else {
            files.push(path);
        }
    }
    let mut h = std::hash::DefaultHasher::new();
    for path in &files {
        path.hash(&mut h);
        std::fs::read(path).unwrap().hash(&mut h);
    }
    for var in ["PROFILE", "TARGET"] {
        std::env::var(var).unwrap().hash(&mut h);
    }
    println!("cargo:rustc-env=P10KD_BUILD_ID={:016x}", h.finish());
}
