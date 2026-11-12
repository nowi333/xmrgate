#!/usr/bin/env bash
# End-to-end test of xmrgate against the real Monero software, on a private local chain.
#
# It starts monerod in regtest mode (no network, instant blocks), creates a merchant wallet and
# a customer wallet, and pays xmrgate invoices with real Monero transactions.
#
# Usage:
#   scripts/regtest-e2e.sh /path/to/monero
#
# where /path/to/monero is the unpacked Monero command-line tools folder from getmonero.org,
# the one containing `monerod` and `monero-wallet-rpc`. Also needs: curl, jq, cargo.
#
# Nothing here touches real funds or the real network. All data goes to a temporary folder
# that is removed at the end (set KEEP=1 to keep it for inspection).

set -euo pipefail

MONERO_DIR="${1:-}"
if [[ -z "$MONERO_DIR" || ! -x "$MONERO_DIR/monerod" || ! -x "$MONERO_DIR/monero-wallet-rpc" ]]; then
  echo "usage: $0 /path/to/monero   (the folder containing monerod and monero-wallet-rpc)" >&2
  exit 2
fi
for tool in curl jq cargo; do
  command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 2; }
done

REPO="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
DAEMON_PORT=48081
P2P_PORT=48080
WALLET_PORT=48083
GATE_PORT=43080
DAEMON="http://127.0.0.1:$DAEMON_PORT"
WALLET="http://127.0.0.1:$WALLET_PORT/json_rpc"
GATE="http://127.0.0.1:$GATE_PORT"
JAR="$WORK/cookies"
XMR=1000000000000
PIDS=()
GATE_PID=""
FAILED=0

# Local services must not go through an HTTP proxy.
export NO_PROXY="127.0.0.1,localhost" no_proxy="127.0.0.1,localhost"

cleanup() {
  for pid in ${PIDS[@]+"${PIDS[@]}"} "$GATE_PID"; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  if [[ "${KEEP:-0}" == "1" ]]; then
    echo "data kept in $WORK"
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

step() { printf '\n== %s\n' "$*"; }
pass() { printf '  PASS  %s\n' "$*"; }
fail() { printf '  FAIL  %s\n' "$*"; FAILED=1; }
die() {
  printf '\nABORTED: %s\n' "$*" >&2
  for log in monerod wallet xmrgate; do
    [[ -f "$WORK/$log.log" ]] && { echo "--- last lines of $log.log"; tail -n 15 "$WORK/$log.log"; } >&2
  done
  exit 1
}

# --- Monero helpers ---

daemon_rpc() { # method, params-json
  curl -sS -m 120 "$DAEMON/json_rpc" -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":\"0\",\"method\":\"$1\",\"params\":$2}"
}

wallet_rpc() { # method, params-json -> the "result" object; aborts on an RPC error
  local answer
  answer="$(curl -sS -m 300 "$WALLET" -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":\"0\",\"method\":\"$1\",\"params\":$2}")"
  if [[ "$(jq -r 'has("error")' <<<"$answer")" == "true" ]]; then
    die "wallet RPC $1 failed: $(jq -c '.error' <<<"$answer")"
  fi
  jq -c '.result' <<<"$answer"
}

mine() { # number of blocks, address receiving the reward
  local answer
  answer="$(daemon_rpc generateblocks "{\"amount_of_blocks\":$1,\"wallet_address\":\"$2\"}")"
  [[ "$(jq -r '.result.status // "error"' <<<"$answer")" == "OK" ]] || die "mining failed: $answer"
}

chain_height() { curl -sS -m 20 "$DAEMON/get_height" | jq -r '.height'; }

pay() { # address, piconero: send from the customer wallet (which must be the open wallet)
  wallet_rpc refresh '{}' >/dev/null
  wallet_rpc transfer "{\"destinations\":[{\"amount\":$2,\"address\":\"$1\"}],\"account_index\":0,\"priority\":1}" \
    | jq -r '.tx_hash'
}

# --- xmrgate helpers ---

start_gate() {
  XMRGATE_SETUP_TOKEN=regtest "$REPO/target/release/xmrgate" \
    --listen "127.0.0.1:$GATE_PORT" --data "$WORK/gate" >>"$WORK/xmrgate.log" 2>&1 &
  GATE_PID=$!
  for _ in $(seq 1 50); do
    curl -sS -m 2 -o /dev/null "$GATE/health" 2>/dev/null && return 0
    sleep 0.2
  done
  die "xmrgate did not start"
}

gate_post() { # path, json body (with the dashboard session)
  curl -sS -m 30 -b "$JAR" -c "$JAR" -X POST "$GATE$1" -H 'Content-Type: application/json' -d "$2"
}

new_invoice() { # amount in XMR as text, description -> invoice json
  local invoice
  invoice="$(gate_post /admin/api/invoices "{\"amount\":\"$1\",\"currency\":\"XMR\",\"description\":\"$2\"}")"
  [[ "$(jq -r '.id // empty' <<<"$invoice")" != "" ]] || die "could not create an invoice: $invoice"
  echo "$invoice"
}

invoice_status() { curl -sS -m 10 "$GATE/pay/$1/status" | jq -r '.status'; }

wait_status() { # invoice id, wanted status, seconds, label
  local status=""
  for _ in $(seq 1 "$3"); do
    status="$(invoice_status "$1" 2>/dev/null || true)"
    if [[ "$status" == "$2" ]]; then
      pass "$4"
      return 0
    fi
    sleep 1
  done
  fail "$4 (expected status \"$2\", still \"$status\" after $3 s)"
  return 0
}

wait_scanned() { # wait until xmrgate has caught up with the regtest chain
  local want scanned=""
  for _ in $(seq 1 90); do
    want=$(( $(chain_height) - 1 ))
    scanned="$(curl -sS -m 5 "$GATE/health" | jq -r '.scanner.scanned // empty')"
    [[ "$scanned" == "$want" ]] && return 0
    sleep 1
  done
  die "xmrgate did not sync to the regtest chain (scanned: ${scanned:-nothing}, chain tip: $want)"
}

# --- 1. Build and start everything ---

step "Building xmrgate"
(cd "$REPO" && cargo build --release --bin xmrgate) || die "build failed"

step "Starting monerod on a private chain"
"$MONERO_DIR/monerod" --regtest --offline --fixed-difficulty 1 \
  --data-dir "$WORK/node" --rpc-bind-ip 127.0.0.1 --rpc-bind-port "$DAEMON_PORT" \
  --p2p-bind-ip 127.0.0.1 --p2p-bind-port "$P2P_PORT" --no-zmq --no-igd \
  --disable-dns-checkpoints --check-updates disabled --non-interactive --log-level 0 \
  >"$WORK/monerod.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 60); do
  chain_height >/dev/null 2>&1 && break
  sleep 0.5
done
chain_height >/dev/null 2>&1 || die "monerod did not start"
pass "monerod is up"

step "Starting the wallet service"
mkdir -p "$WORK/wallets"
WALLET_ARGS=(--wallet-dir "$WORK/wallets" --rpc-bind-ip 127.0.0.1 --rpc-bind-port "$WALLET_PORT"
  --disable-rpc-login --daemon-address "127.0.0.1:$DAEMON_PORT" --trusted-daemon
  --log-file "$WORK/wallet-rpc.log" --log-level 0)
# Only pass flags this version of the wallet knows.
WALLET_HELP="$("$MONERO_DIR/monero-wallet-rpc" --help 2>&1 || true)"
grep -q -- '--allow-mismatched-daemon-version' <<<"$WALLET_HELP" && WALLET_ARGS+=(--allow-mismatched-daemon-version)
grep -q -- '--non-interactive' <<<"$WALLET_HELP" && WALLET_ARGS+=(--non-interactive)
"$MONERO_DIR/monero-wallet-rpc" "${WALLET_ARGS[@]}" >"$WORK/wallet.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 60); do
  curl -sS -m 2 "$WALLET" -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":"0","method":"get_version"}' >/dev/null 2>&1 && break
  sleep 0.5
done
wallet_rpc get_version '{}' >/dev/null
pass "wallet service is up"

step "Creating the merchant and customer wallets"
wallet_rpc create_wallet '{"filename":"merchant","password":"","language":"English"}' >/dev/null
MERCHANT_ADDRESS="$(wallet_rpc get_address '{"account_index":0}' | jq -r '.address')"
MERCHANT_VIEW_KEY="$(wallet_rpc query_key '{"key_type":"view_key"}' | jq -r '.key')"
wallet_rpc close_wallet '{}' >/dev/null
wallet_rpc create_wallet '{"filename":"customer","password":"","language":"English"}' >/dev/null
CUSTOMER_ADDRESS="$(wallet_rpc get_address '{"account_index":0}' | jq -r '.address')"
pass "merchant address ${MERCHANT_ADDRESS:0:12}…"

step "Mining 150 blocks to fund the customer"
mine 150 "$CUSTOMER_ADDRESS"
wallet_rpc refresh '{}' >/dev/null
UNLOCKED="$(wallet_rpc get_balance '{"account_index":0}' | jq -r '.unlocked_balance')"
[[ "$UNLOCKED" -gt $((5 * XMR)) ]] || die "the customer wallet has no spendable funds (unlocked: $UNLOCKED)"
pass "customer can spend $((UNLOCKED / XMR)) XMR"

step "Starting xmrgate and running its setup"
start_gate
SETUP="$(gate_post /setup/api "{\"token\":\"regtest\",\"address\":\"$MERCHANT_ADDRESS\",\"view_key\":\"$MERCHANT_VIEW_KEY\",\"password\":\"regtest-password\",\"shop_name\":\"Regtest Shop\"}")"
[[ "$(jq -r '.ok // false' <<<"$SETUP")" == "true" ]] || die "setup was refused: $SETUP"
pass "xmrgate accepted the wallet's own address and view key"
SETTINGS="$(gate_post /admin/api/settings "{\"nodes\":[\"$DAEMON\"],\"confirmations\":1}")"
[[ "$(jq -r '.ok // false' <<<"$SETTINGS")" == "true" ]] || die "could not set the node: $SETTINGS"
wait_scanned
pass "xmrgate follows the regtest chain"

# --- 2. A normal payment ---

step "Test 1: full payment, seen in the mempool then confirmed"
INVOICE="$(new_invoice 0.5 'full payment')"
ID="$(jq -r '.id' <<<"$INVOICE")"
pay "$(jq -r '.address' <<<"$INVOICE")" "$(jq -r '.amount_piconero' <<<"$INVOICE")" >/dev/null
wait_status "$ID" seen 40 "payment detected before it is mined"
mine 1 "$CUSTOMER_ADDRESS"
wait_status "$ID" confirmed 40 "payment confirmed after one block"

# --- 3. A payment in two parts ---

step "Test 2: partial payment, then the rest"
INVOICE="$(new_invoice 0.3 'two parts')"
ID="$(jq -r '.id' <<<"$INVOICE")"
ADDRESS="$(jq -r '.address' <<<"$INVOICE")"
pay "$ADDRESS" $((XMR / 10)) >/dev/null
mine 1 "$CUSTOMER_ADDRESS"
wait_status "$ID" underpaid 40 "0.1 of 0.3 XMR is reported as a partial payment"
DUE="$(curl -sS -m 10 "$GATE/pay/$ID/status" | jq -r '.due_xmr')"
if [[ "$DUE" == "0.2" ]]; then pass "0.2 XMR is shown as still due"; else fail "amount still due should be 0.2, got $DUE"; fi
pay "$ADDRESS" $((XMR / 5)) >/dev/null
mine 1 "$CUSTOMER_ADDRESS"
wait_status "$ID" confirmed 40 "invoice confirmed once the rest arrives"

# --- 4. A payment made while xmrgate is stopped ---

step "Test 3: payment made while xmrgate is off"
INVOICE="$(new_invoice 0.2 'while offline')"
ID="$(jq -r '.id' <<<"$INVOICE")"
kill "$GATE_PID"; wait "$GATE_PID" 2>/dev/null || true; GATE_PID=""
pay "$(jq -r '.address' <<<"$INVOICE")" "$(jq -r '.amount_piconero' <<<"$INVOICE")" >/dev/null
mine 5 "$CUSTOMER_ADDRESS"
start_gate
wait_status "$ID" confirmed 60 "payment found after restart"
# Dashboard sessions do not survive a restart: sign in again.
LOGIN="$(gate_post /login/api '{"password":"regtest-password"}')"
[[ "$(jq -r '.ok // false' <<<"$LOGIN")" == "true" ]] || die "could not sign in again after the restart: $LOGIN"

# --- 5. A chain reorganisation ---

step "Test 4: the block holding a payment is replaced"
INVOICE="$(new_invoice 0.15 'reorganisation')"
ID="$(jq -r '.id' <<<"$INVOICE")"
pay "$(jq -r '.address' <<<"$INVOICE")" "$(jq -r '.amount_piconero' <<<"$INVOICE")" >/dev/null
mine 1 "$CUSTOMER_ADDRESS"
wait_status "$ID" confirmed 40 "payment confirmed"
POP="$(curl -sS -m 30 "$DAEMON/pop_blocks" -H 'Content-Type: application/json' -d '{"nblocks":1}' || true)"
if [[ "$(jq -r '.status // "error"' <<<"$POP" 2>/dev/null)" == "OK" ]]; then
  # The transaction goes back to the mempool and is mined again in the replacement block.
  mine 2 "$CUSTOMER_ADDRESS"
  sleep 12
  wait_status "$ID" confirmed 40 "payment still confirmed on the new chain"
  if curl -sS -m 10 -b "$JAR" "$GATE/admin/api/state" | jq -e '[.events[].kind] | index("reorg")' >/dev/null; then
    pass "the reorganisation was noticed and logged"
  else
    fail "the reorganisation was not logged"
  fi
else
  echo "  SKIP  this monerod does not offer pop_blocks ($POP)"
fi

# --- 6. Through a SOCKS proxy, as with Tor ---

step "Test 5: reaching the node through a SOCKS5 proxy (the mechanism used for Tor)"
if command -v python3 >/dev/null; then
  # A minimal SOCKS5 proxy. It resolves the made-up name "monerod.test" itself, the way Tor
  # resolves .onion names, so the test fails if xmrgate tries to resolve names on its own.
  cat >"$WORK/socks.py" <<'PY'
import socket, sys, threading

def pipe(a, b):
    try:
        while True:
            data = a.recv(65536)
            if not data:
                break
            b.sendall(data)
    except OSError:
        pass
    finally:
        for s in (a, b):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

def read(conn, n):
    data = b""
    while len(data) < n:
        chunk = conn.recv(n - len(data))
        if not chunk:
            raise OSError("closed")
        data += chunk
    return data

def serve(conn):
    try:
        version, count = read(conn, 2)
        read(conn, count)
        conn.sendall(b"\x05\x00")
        _, command, _, kind = read(conn, 4)
        if kind == 3:
            host = read(conn, read(conn, 1)[0]).decode()
        elif kind == 1:
            host = socket.inet_ntoa(read(conn, 4))
        else:
            raise OSError("unsupported address type")
        port = int.from_bytes(read(conn, 2), "big")
        print(f"CONNECT {host}:{port}", flush=True)
        target = "127.0.0.1" if host.endswith(".test") else host
        upstream = socket.create_connection((target, port), timeout=20)
        conn.sendall(b"\x05\x00\x00\x01" + bytes(6))
        threading.Thread(target=pipe, args=(conn, upstream), daemon=True).start()
        pipe(upstream, conn)
    except OSError:
        conn.close()

server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", int(sys.argv[1])))
server.listen(64)
while True:
    client, _ = server.accept()
    threading.Thread(target=serve, args=(client,), daemon=True).start()
PY
  SOCKS_PORT=49050
  python3 "$WORK/socks.py" "$SOCKS_PORT" >"$WORK/socks.log" 2>&1 &
  PIDS+=($!)
  sleep 1
  SETTINGS="$(gate_post /admin/api/settings "{\"nodes\":[\"http://monerod.test:$DAEMON_PORT\"],\"tor_proxy\":\"127.0.0.1:$SOCKS_PORT\"}")"
  [[ "$(jq -r '.ok // false' <<<"$SETTINGS")" == "true" ]] || die "could not set the proxy: $SETTINGS"
  INVOICE="$(new_invoice 0.05 'through the proxy')"
  ID="$(jq -r '.id' <<<"$INVOICE")"
  pay "$(jq -r '.address' <<<"$INVOICE")" "$(jq -r '.amount_piconero' <<<"$INVOICE")" >/dev/null
  mine 1 "$CUSTOMER_ADDRESS"
  wait_status "$ID" confirmed 60 "payment confirmed with all node traffic going through the proxy"
  if grep -q "CONNECT monerod.test:$DAEMON_PORT" "$WORK/socks.log"; then
    pass "the proxy saw the connections, and resolved the node's name itself"
  else
    fail "the proxy saw no connection to the node"
  fi
  PROXY_PAID="0.05"
else
  echo "  SKIP  python3 is not installed"
  PROXY_PAID="0"
fi

# --- 7. Did the money really arrive? ---

step "Test 6: the merchant's own wallet sees the funds"
wallet_rpc close_wallet '{}' >/dev/null
wallet_rpc open_wallet '{"filename":"merchant","password":""}' >/dev/null
wallet_rpc refresh '{}' >/dev/null
BALANCE="$(wallet_rpc get_balance '{"account_index":0}' | jq -r '.balance')"
EXPECTED=$(( XMR / 2 + XMR * 3 / 10 + XMR / 5 + XMR * 15 / 100 ))
WANT="4 paid, 1.15 XMR"
if [[ "$PROXY_PAID" != "0" ]]; then
  EXPECTED=$(( EXPECTED + XMR / 20 ))
  WANT="5 paid, 1.2 XMR"
fi
if [[ "$BALANCE" == "$EXPECTED" ]]; then
  pass "merchant wallet balance is exactly the sum of the paid invoices ($EXPECTED piconero)"
else
  fail "merchant wallet balance is $BALANCE piconero, expected $EXPECTED"
fi
PAID="$(curl -sS -m 10 -b "$JAR" "$GATE/admin/api/state" | jq -r '"\(.totals.paid) paid, \(.totals.received_xmr) XMR"')"
if [[ "$PAID" == "$WANT" ]]; then
  pass "dashboard totals agree: $PAID"
else
  fail "dashboard totals should be \"$WANT\", got \"$PAID\""
fi

echo
if [[ "$FAILED" == "0" ]]; then
  echo "ALL CHECKS PASSED"
else
  echo "SOME CHECKS FAILED. Re-run with KEEP=1 to keep the logs, and send the output above."
  exit 1
fi
