//! p10kd: the daemon behind powerlevel10k's asynchronous prompt segments.
//!
//! ```text
//! p10kd serve  <dir>        start the daemon under <dir> unless one is listening; exit 0 once one is
//! p10kd attach <dir> <pid>  make <pid>.req and <pid>.resp ready for the shell <pid> and print
//!                           the directory they are in (<dir>/<build id>)
//! ```
//!
//! Exit codes: 0 done, 1 usage, 2 the operation failed (the reason is on stderr).

mod attach;
mod lock;
mod log;
mod protocol;
mod runtime;
mod sample;
mod serve;
mod zsh;

use std::path::PathBuf;
use std::process::ExitCode;

use runtime::RuntimeDir;

enum Cmd {
    Serve(PathBuf),
    Attach(PathBuf, u32),
}

fn parse_args(args: &[String]) -> Option<Cmd> {
    match args {
        [cmd, dir] if cmd == "serve" => Some(Cmd::Serve(PathBuf::from(dir))),
        [cmd, dir, pid] if cmd == "attach" => Some(Cmd::Attach(PathBuf::from(dir), pid.parse().ok()?)),
        _ => None,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = parse_args(&args) else {
        eprintln!("usage: p10kd serve <dir> | p10kd attach <dir> <pid>");
        return ExitCode::from(1);
    };
    let result = match cmd {
        Cmd::Serve(base) => RuntimeDir::establish(&base).and_then(|rt| serve::serve(&rt)),
        Cmd::Attach(base, pid) => RuntimeDir::establish(&base).and_then(|rt| attach::attach(&rt, pid)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("p10kd: {e}");
            ExitCode::from(2)
        }
    }
}
