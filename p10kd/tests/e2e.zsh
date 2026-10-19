#!/usr/bin/env zsh
# Drives interactive shells through a pty with every daemon-served segment enabled and
# checks that each segment's variables arrive through p10kd, that the shells spawn no
# helper processes, that both shells share one daemon, and that the daemon leaves with
# the last shell. Run via `make p10kd-test` from the repository root.
emulate -L zsh
setopt err_exit pipe_fail extended_glob
zmodload zsh/zpty zsh/zselect zsh/system zsh/datetime

local root=${${(%):-%x}:A:h:h:h}
local bin=$root/p10kd/target/release/p10kd
[[ -x $bin ]] || { print -ru2 -- "e2e: build first: make -C $root p10kd"; exit 1 }

local tmp
tmp=$(mktemp -d -t p10kd-e2e)
local failures=0

function fail() { print -ru2 -- "FAIL: $*"; (( ++failures )); }
function ok()   { print -r -- "ok: $*" }
# zselect reports a timeout as failure, which is the point of a sleep.
function nap()  { zselect -t $1 || true }

# Isolate the daemon under $tmp so this never touches the user's real one. The rustup
# proxy must still find the real toolchain, or `rustc --version` installs one into $tmp.
export RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup} CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
export TMPDIR=$tmp/tmp ZDOTDIR=$tmp/home HOME=$tmp/home
mkdir -p $TMPDIR $HOME
print -r -- '203.0.113.7' > $tmp/public_ip
cat > $HOME/.zshrc <<ZRC
typeset -g POWERLEVEL9K_LEFT_PROMPT_ELEMENTS=(disk_usage load ram swap battery wifi public_ip ip vpn_ip time rust_version)
typeset -g POWERLEVEL9K_RIGHT_PROMPT_ELEMENTS=()
typeset -g POWERLEVEL9K_EXPERIMENTAL_TIME_REALTIME=true
typeset -g POWERLEVEL9K_EXPERIMENTAL_TIME_REALTIME_INTERVAL_SEC=0.3
typeset -g POWERLEVEL9K_IP_INTERFACE='lo0?'
typeset -g POWERLEVEL9K_VPN_IP_INTERFACE='lo0?'
typeset -g POWERLEVEL9K_PUBLIC_IP_METHODS=(curl)
typeset -g POWERLEVEL9K_PUBLIC_IP_HOST='file://$tmp/public_ip'
typeset -g POWERLEVEL9K_RUST_VERSION_PROJECT_ONLY=0
typeset -g POWERLEVEL9K_DISABLE_GITSTATUS=true
typeset -g POWERLEVEL9K_INSTANT_PROMPT=off
typeset -gi __ticks=0
source $root/powerlevel10k.zsh-theme
functions[_p9k_prompt_time_sync]='(( ++__ticks )); '\$functions[_p9k_prompt_time_sync]
ZRC

# Reads from pty $1 until its output contains $2 or $3 seconds pass; leaves it in $out.
function read_until() {
  local name=$1 pattern=$2 chunk
  local -F deadline=$((EPOCHREALTIME + $3))
  out=
  while true; do
    if zpty -r -t $name chunk; then
      out+=$chunk
      [[ $out == *$~pattern* ]] && return 0
    else
      (( EPOCHREALTIME < deadline )) || return 1
      nap 5
    fi
  done
}

local out
local -A pids
for shell in a b; do
  zpty -b $shell zsh -i
  read_until $shell '%' 10 || fail "shell $shell showed no prompt: ${(q)out}"
  zpty -w $shell 'print -r -- "PID $$ EN${:-}D"'
  read_until $shell 'PID <-> END' 10 || fail "shell $shell printed no pid: ${(q)out}"
  pids[$shell]=${${${(M)${(f)out}:#PID <-> END*}[-1]}[(w)2]}
done

# Give the daemon a moment to answer the first round of requests, draining the ptys
# meanwhile: a shell blocks in tcsetattr when its pty output is left unread.
local -F until=$((EPOCHREALTIME + 2))
while (( EPOCHREALTIME < until )); do
  for shell in a b; do read_until $shell 'NEVER' 0 || true; done
  nap 5
done

for shell in a b; do
  zpty -w $shell 'print -r -- "RESULT pct=$_p9k__disk_usage_pct load=$_p9k__load_value ram=$_p9k__ram_free swap=$_p9k__swap_used bat=${_p9k__battery_args[1]} wifi=$_p9k__wifi_on/$P9K_WIFI_BARS pubip=$_p9k__public_ip ip=$P9K_IP_IP/$P9K_IP_INTERFACE/$P9K_IP_RX_BYTES vpn=$_p9k__vpn_ip_ips rust=$P9K_RUST_VERSION ticks=$__ticks map=${(j:,:)${(k)_p9k__worker_request_map}} EN${:-}D"'
  read_until $shell 'RESULT*END' 10 || fail "shell $shell printed no result: ${(q)out}"
  local line=${${(M)${(f)out}:#RESULT *END*}[-1]}
  [[ -n $line ]] || { fail "shell $shell: no RESULT line in ${(q)out}"; continue }
  print -r -- "$shell: $line"
  local -A kv=(${(s: :)${${line#RESULT }% END*}//=/ })
  [[ $kv[pct] == <0-100> ]]          && ok "$shell disk_usage=$kv[pct]"   || fail "$shell disk_usage: ${(q)kv[pct]}"
  [[ $kv[load] == <->.<-> ]]         && ok "$shell load=$kv[load]"        || fail "$shell load: ${(q)kv[load]}"
  [[ $kv[ram] == <->(|.<->)[KMGT] ]] && ok "$shell ram=$kv[ram]"          || fail "$shell ram: ${(q)kv[ram]}"
  [[ $kv[swap] == <->(|.<->)[BKMGT] ]] && ok "$shell swap=$kv[swap]"      || fail "$shell swap: ${(q)kv[swap]}"
  [[ ${kv[bat]:-} == (|prompt_battery_*) ]] && ok "$shell battery=${kv[bat]:-none}" || fail "$shell battery: ${(q)kv[bat]}"
  [[ $kv[wifi] == (/|1/<0-4>) ]]     && ok "$shell wifi=$kv[wifi]"        || fail "$shell wifi: ${(q)kv[wifi]}"
  [[ $kv[pubip] == 203.0.113.7 ]]    && ok "$shell public_ip=$kv[pubip]"  || fail "$shell public_ip: ${(q)kv[pubip]}"
  [[ $kv[ip] == 127.0.0.1/lo0#/<-> ]] && ok "$shell ip=$kv[ip]"           || fail "$shell ip: ${(q)kv[ip]}"
  [[ $kv[vpn] == 127.0.0.1 ]]        && ok "$shell vpn_ip=$kv[vpn]"       || fail "$shell vpn_ip: ${(q)kv[vpn]}"
  [[ $kv[rust] == <->.<->.<-> ]]     && ok "$shell rust_version=$kv[rust]" || fail "$shell rust_version: ${(q)kv[rust]}"
  (( kv[ticks] >= 1 ))               && ok "$shell time ticks=$kv[ticks]" || fail "$shell time ticks: ${(q)kv[ticks]}"
done

# A directory whose name holds the wire separators is still a directory the segments
# can ask about: the request fields travel escaped.
local weird=$tmp/$'we\x1dird\x1e\x1f'
mkdir -p $weird
zpty -w a "cd ${(qqqq)weird}"
until=$((EPOCHREALTIME + 1))
while (( EPOCHREALTIME < until )); do read_until a 'NEVER' 0 || true; nap 5; done
zpty -w a 'print -r -- "WEIRD fd=$_p9k__worker_resp_fd pct=$_p9k__disk_usage_pct cwd=${PWD:t} EN${:-}D"'
read_until a 'WEIRD*END' 10 || fail "shell a printed no WEIRD line: ${(q)out}"
line=${${(M)${(f)out}:#WEIRD *END*}[-1]}
kv=(${(s: :)${${line#WEIRD }% END*}//=/ })
[[ -n $kv[fd] ]]             && ok "a worker still attached in ${(q)kv[cwd]}" || fail "a worker detached after cd into ${(q)weird}"
[[ $kv[pct] == <0-100> ]]    && ok "a disk_usage=$kv[pct] in ${(q)kv[cwd]}"  || fail "a disk_usage in weird dir: ${(q)kv[pct]}"

# No helper processes: the shells have no zsh children, and one daemon serves both.
local rtdir=($TMPDIR/p10kd-$EUID/*(/N))
(( $#rtdir == 1 )) && ok "one runtime directory ($rtdir:t)" || fail "expected one runtime directory, found: $rtdir"
local daemons=(${(f)"$(pgrep -f "p10kd serve $TMPDIR" || true)"})
(( $#daemons == 1 )) && ok "one daemon ($daemons)" || fail "expected one daemon, found: $daemons"
for shell in a b; do
  local kids=(${(f)"$(pgrep -P $pids[$shell] || true)"})
  (( $#kids == 0 )) && ok "shell $shell ($pids[$shell]) has no child processes" || fail "shell $shell has children: $kids"
done
[[ -S $rtdir/sock ]] && ok "socket present while shells live" || fail "socket missing"

# The daemon survives one shell leaving and exits after the last one.
zpty -w a 'exit'
zpty -d a
nap 50
[[ -S $rtdir/sock ]] && ok "daemon stays for shell b" || fail "daemon left early"
zpty -w b 'exit'
zpty -d b
local -F deadline=$((EPOCHREALTIME + 3))
while [[ -e $rtdir/sock ]] && (( EPOCHREALTIME < deadline )); do nap 5; done
[[ ! -e $rtdir/sock ]] && ok "daemon exited with the last shell" || fail "daemon still listening"
local leftovers=(${(f)"$(pgrep -f "p10kd serve $TMPDIR" || true)"})
(( $#leftovers == 0 )) && ok "no daemon process left" || { fail "daemon processes left: $leftovers"; kill $leftovers }
grep -q 'last shell left' $rtdir/log && ok "log records the exit" || fail "log: $(<$rtdir/log)"

rm -rf $tmp
(( failures == 0 )) && print -r -- "e2e: all checks passed" || { print -ru2 -- "e2e: $failures check(s) failed"; exit 1 }
