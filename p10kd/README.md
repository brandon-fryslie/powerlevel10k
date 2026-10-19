# p10kd

The daemon behind powerlevel10k's asynchronous prompt segments: `disk_usage`, `load`,
`ram`, `swap`, `battery`, `wifi`, `public_ip`, `ip`, `vpn_ip`, `time` (realtime mode),
`rust_version` and `gcloud`.

One `p10kd` process per user serves every interactive shell. A shell attaches once at
startup and, per prompt, asks for the facts its segments need; the daemon answers
through a pair of FIFOs. Shells carry no helper processes and fork nothing for these
segments: load, memory, disk, interfaces, battery and wifi are read through syscalls and
system frameworks. `rustc`, `gcloud` and the public-IP fetchers remain subprocesses of
the daemon because running them is the fact itself.

## Build

```sh
make p10kd          # cargo build --release, from the repository root
make p10kd-test     # unit + daemon tests, the shell client's unit tests, then an interactive end-to-end run
```

The theme looks for `p10kd/target/release/p10kd` under its own directory and prints the
build command to stderr when it is missing.

## Where it lives at runtime

`$TMPDIR/p10kd-$EUID/<build id>/` (or `/tmp/p10kd-$EUID/<build id>/`). The build id is
a hash of the sources the binary was built from, so a rebuilt p10kd gets a daemon of its
own instead of talking to one that speaks an older protocol; `p10kd attach` prints the
directory. Both directory levels must be real directories owned by the user with mode
0700, which `p10kd` sets on its own directories and refuses otherwise.

| file           | purpose                                              |
|----------------|------------------------------------------------------|
| `sock`         | attach socket                                        |
| `<pid>.req`    | shell → daemon requests for the shell with that pid  |
| `<pid>.resp`   | daemon → shell replies                               |
| `log`          | start, exit, attach, detach, and every sampler error |
| `run.lock`     | held by the running daemon                           |
| `start.lock`   | held while a daemon starts or stops                  |

The daemon starts on the first attach and exits when the last shell detaches. It keeps
its socket on disk: when a temporary-directory sweeper removes it, the daemon re-binds
within a second.

`rustc`, `gcloud`, `curl`, `wget` and `dig` run with the environment the asking shell
sends along (`_p9k__worker_env_names` in `internal/worker.zsh`), not the daemon's own.

## Layout

- `src/protocol.rs` — the wire contract and its parser.
- `src/sample/` — one sampler per fact; the positional order of each sampler's facts is
  mirrored by the matching `_p9k_prompt_<segment>_sync` function in `internal/p10k.zsh`.
- `src/serve.rs`, `src/attach.rs` — daemon lifecycle and the shell-side attach helper.
- `../internal/worker.zsh` — the shell client.
