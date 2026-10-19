#!/usr/bin/env zsh
# Unit tests for internal/worker.zsh: the bytes _p9k_worker_invoke puts on the wire and
# the environment _p9k_worker_env builds. Run via `make p10kd-test` from the repository
# root.
emulate -L zsh -o extended_glob -o no_multibyte
zmodload zsh/system

local root=${${(%):-%x}:A:h:h:h}
local failures=0
function fail() { print -ru2 -- "FAIL: $*"; (( ++failures )); }
function ok()   { print -r -- "ok: $*" }

typeset -gA _p9k__worker_request_map
typeset -g _p9k__worker_resp_fd=1 _p9k__worker_env sent
autoload -Uz add-zsh-hook
source $root/internal/worker.zsh
function _p9k_worker_send() { sent=$1 }

# Every field is escaped, separators and the escape byte included, and an empty field
# stays a field.
_p9k_worker_invoke disk_usage cb disk_usage "a"$'\x1d'"b"$'\x1e'"c"$'\x1f'"d" ""
local want=$'disk_usage\x1fcb\x1fdisk_usage\x1fa\x1dDb\x1dEc\x1dFd\x1f\x1e'
[[ $sent == $want ]] && ok "invoke escapes fields: ${(q)sent}" || fail "invoke sent ${(q)sent}, want ${(q)want}"

# A second request for an id in flight waits, and only the newest waiting one survives.
sent=
_p9k_worker_invoke disk_usage cb disk_usage /one
_p9k_worker_invoke disk_usage cb disk_usage /two
[[ -z $sent ]] && ok "request for a busy id is queued, not sent" || fail "sent while busy: ${(q)sent}"
[[ $_p9k__worker_request_map[disk_usage] == $'disk_usage\x1fcb\x1fdisk_usage\x1f/two\x1e' ]] \
  && ok "newest queued request survives" || fail "queued: ${(q)_p9k__worker_request_map[disk_usage]}"

# The env holds exported names from the list, as NAME=value joined by NUL, nothing else.
export P10KD_T_EXPORTED=yes
typeset -g P10KD_T_LOCAL=no
_p9k__worker_env_names=(P10KD_T_EXPORTED P10KD_T_LOCAL P10KD_T_UNSET HOME)
_p9k_worker_env
want=$'P10KD_T_EXPORTED=yes\0'"HOME=$HOME"
[[ $_p9k__worker_env == $want ]] && ok "env is the exported names only: ${(q)_p9k__worker_env}" \
  || fail "env is ${(q)_p9k__worker_env}, want ${(q)want}"
_p9k__worker_env_names=(P10KD_T_UNSET)
_p9k_worker_env
[[ -z $_p9k__worker_env ]] && ok "env with nothing exported is empty" || fail "env: ${(q)_p9k__worker_env}"

(( failures == 0 )) && print -r -- "worker: all checks passed" || { print -ru2 -- "worker: $failures check(s) failed"; exit 1 }
