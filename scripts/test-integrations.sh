#!/usr/bin/env bash
# Runs the example shops in examples/integrations against a real xmrgate on a simulated chain:
# each one must create an invoice, and report the order as paid once the signed webhook arrives.
#
# Usage: scripts/test-integrations.sh        (needs cargo, curl, jq, php, node, python3)

set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
GATE="http://127.0.0.1:3999"
CONTROL="http://127.0.0.1:3998"
export NO_PROXY="127.0.0.1,localhost" no_proxy="127.0.0.1,localhost"
PIDS=()
FAILED=0
cleanup() {
  for pid in ${PIDS[@]+"${PIDS[@]}"}; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT
pass() { printf '  PASS  %s\n' "$*"; }
fail() { printf '  FAIL  %s\n' "$*"; FAILED=1; }

(cd "$REPO" && cargo build --example demo) || exit 1
"$REPO/target/debug/examples/demo" --fresh >"$WORK/demo.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 50); do grep -q '^control:' "$WORK/demo.log" && break; sleep 0.2; done
ADDRESS="$(sed -n 's/^address: //p' "$WORK/demo.log")"
VIEW_KEY="$(sed -n 's/^viewkey: //p' "$WORK/demo.log")"
curl -sS -c "$WORK/jar" -X POST "$GATE/setup/api" -H 'Content-Type: application/json' \
  -d "{\"token\":\"demo\",\"address\":\"$ADDRESS\",\"view_key\":\"$VIEW_KEY\",\"password\":\"demo-password\"}" >/dev/null
KEY="$(curl -sS -b "$WORK/jar" "$GATE/admin/api/state" | jq -r '.sites[0].api_key')"
[[ -n "$KEY" && "$KEY" != "null" ]] || { echo "could not set up the gateway"; exit 1; }

check() { # language, port, command...
  local name="$1" port="$2"; shift 2
  local shop="http://127.0.0.1:$port" log="$WORK/$name.log"
  echo "== $name"
  XMRGATE_URL="$GATE" XMRGATE_KEY="$KEY" SHOP_URL="$shop" "$@" >"$log" 2>&1 &
  local pid=$!
  PIDS+=("$pid")
  for _ in $(seq 1 50); do curl -sS -o /dev/null -m 2 "$shop/" 2>/dev/null && break; sleep 0.2; done

  local checkout
  checkout="$(curl -sS -o /dev/null -w '%{redirect_url}' "$shop/buy")"
  if [[ "$checkout" == "$GATE/pay/"* ]]; then pass "the buy link leads to the payment page"; else fail "no payment page (got: $checkout)"; kill "$pid"; return; fi

  local forged
  forged="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "$shop/webhook" -H 'X-Xmrgate-Signature: 00' -d '{"invoice":{"status":"confirmed","order_ref":"forged","received_xmr":"1"}}')"
  if [[ "$forged" == "401" ]]; then pass "a forged webhook is refused"; else fail "a forged webhook got HTTP $forged"; fi

  local invoice
  invoice="$(curl -sS "$checkout/status")"
  curl -sS -X POST "$CONTROL/pay" -H 'Content-Type: application/json' \
    -d "{\"address\":\"$(jq -r '.address' <<<"$invoice")\",\"piconero\":$(jq -r '.amount_piconero' <<<"$invoice")}" >/dev/null
  sleep 7
  curl -sS -X POST "$CONTROL/mine" >/dev/null
  local paid=""
  for _ in $(seq 1 40); do
    paid="$(grep -o 'PAID order-[0-9]* [0-9.]* XMR' "$log" || true)"
    [[ -n "$paid" ]] && break
    sleep 1
  done
  if [[ -n "$paid" ]]; then pass "the shop was told: $paid"; else fail "the shop never saw the payment"; sed 's/^/        /' "$log" | tail -5; fi
  if grep -q 'UPDATE order-[0-9]* seen' "$log"; then pass "it was told first that the payment was on its way"; else fail "no 'seen' update arrived"; fi
  if grep -q 'forged' "$log"; then fail "the forged webhook was acted on"; fi
  kill "$pid" 2>/dev/null || true
}

check PHP 8101 php -S 127.0.0.1:8101 "$REPO/examples/integrations/shop.php"
check Node.js 8102 node "$REPO/examples/integrations/shop.mjs"
check Python 8103 python3 "$REPO/examples/integrations/shop.py"

echo
if [[ "$FAILED" == "0" ]]; then echo "ALL CHECKS PASSED"; else echo "SOME CHECKS FAILED"; exit 1; fi
