#!/usr/bin/env bash
# Pair over the API (d11): the guards of the network API, against the built binary on 127.0.0.1.
# It starts a controller with `--api-listen 127.0.0.1:0` on a temp state directory, pairs a device
# with a pinned `curl`, adds and lists a project over the API, checks the 401 answers and the revoke,
# blocks one peer address after 5 bad codes while a second address still pairs, reads the request
# ledger `api.ledger`, and restarts the controller on a new key: a client pinned to the old key exits
# 90 and tells its user `server key changed: re-pair`, and the restart clears the block.
#
# It does not build: `PF_BIN` is a built binary (default `pulse-factory` on PATH). It needs `curl`,
# `jq` and coreutils (`base32`, `base64`, `od`). It spends nothing and reaches no address beyond
# 127.0.0.0/8: `curl --interface 127.0.0.2` and `127.0.0.3` work on Linux, where all of 127/8 is
# on the loopback interface.
#
#   PF_BIN=target/debug/pulse-factory scripts/demo/pair-api.sh
set -euo pipefail

PF_BIN="${PF_BIN:-pulse-factory}"

fail() { echo "pair-api: FAIL: $*" >&2; exit 1; }
for tool in curl jq base32 base64 od tail head tr; do
  command -v "$tool" >/dev/null || fail "$tool is not on PATH (this script needs it)"
done
command -v "$PF_BIN" >/dev/null || [ -x "$PF_BIN" ] || fail "PF_BIN=$PF_BIN is not an executable"

tmp="$(mktemp -d)"
pid=""
stop_controller() {
  [ -n "$pid" ] || return 0
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 100); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  pid=""
}
cleanup() { stop_controller; rm -rf "$tmp"; }
trap cleanup EXIT

unset PF_SEAT_TOKEN
export PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
mkdir -p "$PF_SHARE_DIR" "$tmp/work/alpha"
stderr_file="$tmp/controller.stderr"
ledger_file="$PF_STATE_DIR/api.ledger"
admissions_file="$PF_STATE_DIR/controller.ledger"

check() { [ "$1" = "$2" ] || fail "$3: wanted '$2', got '$1'"; }

# Start the controller on a free loopback port, wait until it listens, and set `addr` and `spki`.
start_controller() {
  "$PF_BIN" controller --tick-ms 3600000 --api-listen 127.0.0.1:0 >/dev/null 2>>"$stderr_file" &
  pid=$!
  local state=""
  for _ in $(seq 1 200); do
    state="$("$PF_BIN" api status --json 2>/dev/null | jq -r '.state' 2>/dev/null || true)"
    [ "$state" = listening ] && break
    kill -0 "$pid" 2>/dev/null || fail "the controller exited before it listened: $(cat "$stderr_file")"
    sleep 0.05
  done
  check "$state" listening "the API state after the controller started"
  addr="$("$PF_BIN" api status --json | jq -r '.listen')"
  spki="$("$PF_BIN" api status --json | jq -r '.key_spki_sha256')"
  case "$addr" in 127.0.0.1:*) ;; *) fail "the API listens on '$addr', not on 127.0.0.1" ;; esac
}

# The raw bytes of a `pf1-` code, in a file: 1 byte of version, 10 of secret, 32 of SPKI hash, then
# the address (docs/contracts/api.md, "Pairing codes").
decode_code() {
  local text="${1#pf1-}" pad
  text="$(printf '%s' "$text" | tr 'a-z' 'A-Z')"
  pad=$(( (8 - ${#text} % 8) % 8 ))
  while [ "$pad" -gt 0 ]; do text="$text="; pad=$((pad - 1)); done
  printf '%s' "$text" | base32 -d > "$2"
}

hex_of() { od -An -tx1 -v -j"$2" -N"$3" "$1" | tr -d ' \n'; }

# One call, pinned to a key as a client must (docs/contracts/api.md, "The pin check"):
#   api_call <pin base64> <address> <method> <path> <token or -> <body or -> [source address]
# It sets `status` and `reply`. When `curl` exits 90 (the key is not the pin), it tells the user
# `server key changed: re-pair` on stderr, sends nothing and returns 90.
status="" reply=""
api_call() {
  local pin="$1" at="$2" method="$3" path="$4" token="$5" body="$6" src="${7:-}"
  local args=(-sS -k --max-time 20 --pinnedpubkey "sha256//$pin" -X "$method" -w '\n%{http_code}')
  [ "$token" = - ] || args+=(-H "Authorization: Bearer $token")
  [ "$body" = - ] || args+=(-H 'Content-Type: application/json' --data-binary "$body")
  [ -z "$src" ] || args+=(--interface "$src")
  local out rc=0
  out="$(curl "${args[@]}" "https://$at$path" 2>"$tmp/curl.err")" || rc=$?
  if [ "$rc" -eq 90 ]; then
    echo "server key changed: re-pair" >&2
    return 90
  fi
  [ "$rc" -eq 0 ] || fail "curl exited $rc for $method $path: $(cat "$tmp/curl.err")"
  status="${out##*$'\n'}"
  reply="${out%$'\n'*}"
}

pair_body() { jq -nc --arg code "$1" --arg name "$2" '{code: $code, device_name: $name}'; }

new_code() { "$PF_BIN" device pair --json | jq -r '.code'; }

# --------------------------------------------------------------------------------- start
start_controller
echo "controller up on $addr"

code1="$(new_code)"
decode_code "$code1" "$tmp/code1.bin"
check "$(od -An -tu1 -N1 "$tmp/code1.bin" | tr -d ' ')" 1 "the version byte of the code"
secret1="$(hex_of "$tmp/code1.bin" 1 10)"
code_spki="$(hex_of "$tmp/code1.bin" 11 32)"
check "$code_spki" "$spki" "the SPKI hash in the code, against the hash of api status"
pin1="$(tail -c +12 "$tmp/code1.bin" | head -c 32 | base64 -w0)"
old_pin="$pin1"
old_spki="$spki"

# 1. a pinned pair with a fresh code returns a token and D-1
api_call "$pin1" "$addr" POST /v1/pair - "$(pair_body "$code1" demo-macbook)"
check "$status" 200 "step 1: the status of the pairing"
token1="$(printf '%s' "$reply" | jq -r '.token')"
check "$(printf '%s' "$reply" | jq -r '.device.id')" D-1 "step 1: the first device"
printf '%s' "$token1" | grep -qE '^[0-9a-f]{64}$' || fail "step 1: the token is not 64 hex digits"
echo "step 1: paired D-1"

# 2. project_add, then project_list, over the API show the project
add="$(jq -nc --arg root "$tmp/work/alpha" '{root: $root, name: "alpha", command_id: "pair-api-add-1"}')"
api_call "$pin1" "$addr" POST /v1/verbs/project_add "$token1" "$add"
check "$status" 200 "step 2: project_add over the API ($reply)"
api_call "$pin1" "$addr" POST /v1/verbs/project_list "$token1" '{}'
check "$status" 200 "step 2: project_list over the API"
check "$(printf '%s' "$reply" | jq -r '[.projects[].name] | join(",")')" alpha "step 2: the projects the API lists"
echo "step 2: project alpha added and listed"

# 3. a call with no token answers 401, and so does the used code
api_call "$pin1" "$addr" POST /v1/verbs/project_list - '{}'
check "$status" 401 "step 3: a call with no token"
api_call "$pin1" "$addr" POST /v1/pair - "$(pair_body "$code1" demo-again)"
check "$status" 401 "step 3: the code used a second time"
echo "step 3: no token and a used code both answer 401"

# 4. a revoke on the CLI refuses the next call at once
"$PF_BIN" device revoke D-1 >/dev/null || fail "step 4: device revoke D-1 failed"
api_call "$pin1" "$addr" POST /v1/verbs/project_list "$token1" '{}'
check "$status" 401 "step 4: a call with the revoked token"
echo "step 4: D-1 revoked, its next call answers 401"

# 5. from 127.0.0.3, a peer with no failures yet: 5 bad codes answer 401, then a fresh valid code
#    answers 429
code2="$(new_code)"
decode_code "$code2" "$tmp/code2.bin"
secret2="$(hex_of "$tmp/code2.bin" 1 10)"
for n in 1 2 3 4 5; do
  api_call "$pin1" "$addr" POST /v1/pair - "$(pair_body pf1-garbage demo-guess)" 127.0.0.3
  check "$status" 401 "step 5: bad code $n from 127.0.0.3"
done
api_call "$pin1" "$addr" POST /v1/pair - "$(pair_body "$code2" demo-blocked)" 127.0.0.3
check "$status" 429 "step 5: a valid code from the blocked address ($reply)"
printf '%s' "$reply" | jq -e '.error | startswith("too many failed attempts from this address: try again after ")' >/dev/null \
  || fail "step 5: the 429 text is not the documented one: $reply"
echo "step 5: 127.0.0.3 blocked after 5 bad codes"

# 6. from 127.0.0.2, the same valid code pairs and returns D-2
api_call "$pin1" "$addr" POST /v1/pair - "$(pair_body "$code2" demo-second)" 127.0.0.2
check "$status" 200 "step 6: the valid code from 127.0.0.2 ($reply)"
check "$(printf '%s' "$reply" | jq -r '.device.id')" D-2 "step 6: the second device"
token2="$(printf '%s' "$reply" | jq -r '.token')"
echo "step 6: 127.0.0.2 paired D-2 while 127.0.0.3 was blocked"

# 7. the request ledger has one line per request so far, and no secret is in any file
[ -f "$ledger_file" ] || fail "step 7: no request ledger at $ledger_file"
check "$(wc -l < "$ledger_file" | tr -d ' ')" 13 "step 7: the lines of api.ledger (one for each request so far)"
outcomes="$(sed -n 's/.* outcome=\([a-z_]*\).*/\1/p' "$ledger_file" | tr '\n' ' ')"
want_outcomes="ok ok ok unauthorized unauthorized unauthorized unauthorized unauthorized unauthorized unauthorized unauthorized blocked ok "
check "$outcomes" "$want_outcomes" "step 7: the outcomes of api.ledger, in order"
check "$(grep -c ' peer=127.0.0.3 ' "$ledger_file")" 6 "step 7: the lines of 127.0.0.3"
check "$(grep -c ' peer=127.0.0.2 ' "$ledger_file")" 1 "step 7: the lines of 127.0.0.2"
check "$(grep -c ' blocked_until=' "$ledger_file")" 1 "step 7: the lines that say when a block ends"
check "$(sed -n 11p "$ledger_file" | grep -c ' blocked_until=')" 1 "step 7: the fifth bad code's line carries blocked_until"
check "$(sed -n 12p "$ledger_file" | grep -c ' outcome=blocked count=1')" 1 "step 7: the 429's line"
check "$(sed -n 2p "$ledger_file" | grep -c ' verb=project_add device=D-1 ')" 1 "step 7: the project_add line names D-1"
for secret in "$token1" "$token2" "$code1" "$code2" "$secret1" "$secret2"; do
  for file in "$ledger_file" "$admissions_file" "$stderr_file"; do
    if grep -qF -- "$secret" "$file"; then fail "step 7: a token, a code or a secret is in $(basename "$file")"; fi
  done
done
echo "step 7: api.ledger has 13 lines and no secret is in any log"

# 8. the key files are removed and the controller restarts on a new key: a client pinned to the old
#    hash exits 90 and says so, sends nothing, and the restart cleared the block
stop_controller
rm -rf "$PF_STATE_DIR/api"
start_controller
[ "$spki" != "$old_spki" ] || fail "step 8: the restart made the same key"
lines_before="$(wc -l < "$ledger_file" | tr -d ' ')"
rc=0
message="$(api_call "$old_pin" "$addr" POST /v1/pair - "$(pair_body "$code2" demo-late)" 2>&1 >/dev/null)" || rc=$?
check "$rc" 90 "step 8: the exit of a client pinned to the old key"
check "$message" "server key changed: re-pair" "step 8: the text the client tells its user"
echo "$message"
check "$(wc -l < "$ledger_file" | tr -d ' ')" "$lines_before" "step 8: api.ledger gained a line for a connection that sent nothing"
code3="$(new_code)"
decode_code "$code3" "$tmp/code3.bin"
pin3="$(tail -c +12 "$tmp/code3.bin" | head -c 32 | base64 -w0)"
api_call "$pin3" "$addr" POST /v1/pair - "$(pair_body "$code3" demo-third)" 127.0.0.3
check "$status" 200 "step 8: a fresh code from 127.0.0.3 after the restart ($reply)"
check "$(printf '%s' "$reply" | jq -r '.device.id')" D-3 "step 8: the third device"
echo "step 8: the old pin exits 90, and 127.0.0.3 pairs again after the restart"

echo "pair-api: ok"
