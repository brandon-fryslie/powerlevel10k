# The shell side of p10kd (see ../p10kd/src/main.rs).
#
# One daemon per user samples machine facts for every shell. A shell attaches once at
# init and then, per prompt, sends one request per asynchronous segment; the daemon
# answers each with a zsh command line to evaluate (the segment's sync callback with the
# facts as quoted arguments) followed by a done marker that releases the next request
# queued for that segment.
#
# Wire format, shared with p10kd/src/protocol.rs: records end in \x1e, fields are
# separated by \x1f, and every field is escaped so any byte survives: \x1d is the
# escape byte, with \x1dD, \x1dE, \x1dF standing for \x1d, \x1e, \x1f.
#   request:  id \x1f callback \x1f segment \x1f arg...
#   reply:    e<callback> 'fact' 'fact'...   then   d<id>
# A sample the daemon could not take answers the same way with no facts (the daemon
# logs why), so every callback runs exactly once per request.
#
# Segments that run a program (rust_version, gcloud, public_ip) take the environment to
# run it in as their first argument; _p9k_worker_env builds it.
#
# Traffic rides two FIFOs rather than a socket: a zsocket descriptor is inherited by
# every child the shell spawns, while sysopen -o cloexec is not.

# usage: _p9k_worker_invoke <request-id> <callback> <segment> [arg]...
#
# At most one request per id is in flight; a newer one waits until the daemon reports
# the older done, and only the newest waiting request survives.
function _p9k_worker_invoke() {
  [[ -n $_p9k__worker_resp_fd ]] || return
  local e=$'\x1d' r=$'\x1e' f=$'\x1f'
  local -a fields=("${(@)${(@)${(@)@//$e/${e}D}//$r/${e}E}//$f/${e}F}")
  local req=${(pj:\x1f:)fields}$r
  if (( $+_p9k__worker_request_map[$1] == 0 )); then
    _p9k__worker_request_map[$1]=
    _p9k_worker_send $req
  else
    _p9k__worker_request_map[$1]=$req
  fi
}

# The exported variables that rustc (through rustup), gcloud, curl, wget and dig consult.
# The daemon runs those programs with exactly this environment on the shell's behalf,
# because its own environment belongs to whichever shell started it.
typeset -ga _p9k__worker_env_names=(
  HOME PATH TMPDIR LANG LC_ALL LC_CTYPE
  RUSTUP_HOME RUSTUP_TOOLCHAIN RUSTUP_DIST_SERVER RUSTUP_UPDATE_ROOT CARGO_HOME
  CLOUDSDK_CONFIG CLOUDSDK_PYTHON CLOUDSDK_ACTIVE_CONFIG_NAME
  http_proxy https_proxy ftp_proxy all_proxy no_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY NO_PROXY
  CURL_HOME CURL_CA_BUNDLE SSL_CERT_FILE SSL_CERT_DIR WGETRC
)

# Sets _p9k__worker_env to the current values of $_p9k__worker_env_names that are
# exported, as NUL-separated NAME=value entries: the env argument of a program-running
# segment.
function _p9k_worker_env() {
  local n
  local -a kv
  for n in $_p9k__worker_env_names; do
    [[ $parameters[$n] == *export* ]] && kv+=$n=${(P)n}
  done
  _p9k__worker_env=${(pj:\0:)kv}
}

function _p9k_worker_send() {
  print -rnu $_p9k__worker_req_fd -- $1 && return
  print -ru2 -- "[p10k] cannot send a request to p10kd (${ERRNO:+errno $ERRNO}); async segments stop updating"
  _p9k_worker_stop
  return 1
}

function _p9k_worker_cleanup() {
  # __p9k_intro bugs out here in some cases for some reason.
  emulate -L zsh
  [[ $_p9k__worker_shell_pid == $sysparams[pid] ]] && _p9k_worker_stop
  return 0
}

function _p9k_worker_stop() {
  # See comments in _p9k_worker_cleanup.
  emulate -L zsh
  add-zsh-hook -D zshexit _p9k_worker_cleanup
  [[ -n $_p9k__worker_resp_fd ]] && zle -F $_p9k__worker_resp_fd
  [[ -n $_p9k__worker_resp_fd ]] && exec {_p9k__worker_resp_fd}>&-
  [[ -n $_p9k__worker_req_fd  ]] && exec {_p9k__worker_req_fd}>&-
  _p9k__worker_req_fd=
  _p9k__worker_resp_fd=
  _p9k__worker_shell_pid=
  _p9k__worker_buf=
  _p9k__worker_request_map=()
  return 0
}

# zle -F handler: drains the response FIFO, evaluates every complete record, keeps a
# partial one for next time.
function _p9k_worker_receive() {
  eval "$__p9k_intro"

  [[ -z $_p9k__worker_resp_fd ]] && return

  {
    (( $# <= 1 )) || return

    local buf=$_p9k__worker_buf resp
    [[ -t $_p9k__worker_resp_fd ]]  # https://www.zsh.org/mla/workers/2020/msg00207.html
    while true; do
      sysread -i $_p9k__worker_resp_fd -t 0 'buf[$#buf+1]' && continue
      case $? in
        4) break;;  # nothing more to read right now
        5) print -ru2 -- "[p10k] p10kd closed the connection; async segments stop updating"; return 1;;
        *) print -ru2 -- "[p10k] reading from p10kd failed (errno $ERRNO); async segments stop updating"; return 1;;
      esac
    done
    _p9k__worker_buf=${buf##*$'\x1e'}
    buf=${buf[1,-$#_p9k__worker_buf-1]}

    local -i reset max_reset
    for resp in ${(ps:\x1e:)buf}; do
      local arg=$resp[2,-1]
      case $resp[1] in
        d)
          local req=$_p9k__worker_request_map[$arg]
          if [[ -n $req ]]; then
            _p9k__worker_request_map[$arg]=
            _p9k_worker_send $req || return
          else
            unset "_p9k__worker_request_map[$arg]"
          fi
        ;;
        e)
          reset=0
          () { eval $arg }
          (( reset > max_reset )) && max_reset=reset
        ;;
        *)
          print -ru2 -- "[p10k] p10kd sent an unknown record ${(q)resp}; async segments stop updating"
          return 1
        ;;
      esac
    done

    if (( max_reset == 2 )); then
      _p9k__refresh_reason=worker
      _p9k_set_prompt
      _p9k__refresh_reason=''
    fi
    (( max_reset )) && _p9k_reset_prompt
    return 0
  } always {
    (( $? )) && _p9k_worker_stop
  }
}

function _p9k_worker_start() {
  {
    [[ -n $_p9k__worker_resp_fd ]] && return

    local bin=$__p9k_root_dir/p10kd/target/release/p10kd
    if [[ ! -x $bin ]]; then
      print -ru2 -- "[p10k] async segments need p10kd: run 'make -C ${(q)__p9k_root_dir} p10kd'"
      return 1
    fi

    if [[ -n "$TMPDIR" && ( ( -d "$TMPDIR" && -w "$TMPDIR" ) || ! ( -d /tmp && -w /tmp ) ) ]]; then
      local base=$TMPDIR/p10kd-$EUID
    else
      local base=/tmp/p10kd-$EUID
    fi
    local pid=$sysparams[pid]

    # attach prints the directory the FIFOs live in: $base/<build id of $bin>.
    local dir
    dir=$(command $bin attach $base $pid) || return
    # Both opens are immediate: the daemon holds the other end of each FIFO before
    # `attach` returns. Non-blocking opens turn a daemon that died in between into an
    # error instead of a hang.
    sysopen -w -o cloexec -o nonblock -u _p9k__worker_req_fd  $dir/$pid.req  || return
    sysopen -r -o cloexec -o nonblock -u _p9k__worker_resp_fd $dir/$pid.resp || return
    print -rnu $_p9k__worker_req_fd -- $'h\x1e' || return

    _p9k__worker_buf=
    zle -F $_p9k__worker_resp_fd _p9k_worker_receive
    _p9k__worker_shell_pid=$pid
    add-zsh-hook zshexit _p9k_worker_cleanup
  } always {
    (( $? )) && _p9k_worker_stop
  }
}
