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

fn main() {
    let mut files = vec![PathBuf::from("build.rs"), PathBuf::from("Cargo.toml"), PathBuf::from("Cargo.lock")];
    sources(Path::new("src"), &mut files);
    for dep in ["build.rs", "Cargo.toml", "Cargo.lock", "src"] {
        println!("cargo:rerun-if-changed={dep}");
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
