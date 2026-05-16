#!/usr/bin/env bash
# Compare network latency to Polymarket endpoints. Runs N samples per
# endpoint and reports min / median / p95 / max for:
#   - DNS lookup
#   - TCP connect (RTT * ~1)
#   - TLS handshake (RTT * ~2 — actual cost matters for WS upgrade)
#   - Total (TTFB)
#
# Usage: ./bench-poly-net.sh [N]   (default N=20)
set -euo pipefail

N="${1:-20}"

ENDPOINTS=(
    "https://clob.polymarket.com/time"
    "https://gamma-api.polymarket.com/events?limit=1"
    "https://data-api.polymarket.com/trades?market=0x0000000000000000000000000000000000000000000000000000000000000000&limit=1"
    "https://ws-subscriptions-clob.polymarket.com/"
    "https://ws-live-data.polymarket.com/"
)

LABELS=(
    "clob-rest      "
    "gamma-rest     "
    "data-rest      "
    "clob-ws        "
    "live-data-ws   "
)

pct() {
    # pct <p> <sorted-values...> - p in 0..100
    local p=$1; shift
    local n=$#
    local idx=$(( (p * (n - 1) + 50) / 100 ))
    local i=1
    for v in "$@"; do
        if [ $i -eq $((idx + 1)) ]; then echo "$v"; return; fi
        i=$((i + 1))
    done
}

stats() {
    local sorted; sorted=$(printf '%s\n' "$@" | sort -n)
    local arr=($sorted)
    local min=${arr[0]}
    local max=${arr[$((${#arr[@]} - 1))]}
    local med; med=$(pct 50 "${arr[@]}")
    local p95; p95=$(pct 95 "${arr[@]}")
    printf '%6.0f  %6.0f  %6.0f  %6.0f' "$min" "$med" "$p95" "$max"
}

bench_one() {
    local url=$1
    local connect=() appconnect=() total=()
    for _ in $(seq 1 "$N"); do
        local line; line=$(curl -sk -o /dev/null \
            -w '%{time_connect} %{time_appconnect} %{time_total}\n' \
            --max-time 10 "$url" 2>/dev/null || echo "0 0 0")
        # Convert seconds to milliseconds (×1000)
        local c=$(awk '{printf "%.0f", $1*1000}' <<<"$line")
        local a=$(awk '{printf "%.0f", $2*1000}' <<<"$line")
        local t=$(awk '{printf "%.0f", $3*1000}' <<<"$line")
        connect+=("$c"); appconnect+=("$a"); total+=("$t")
    done
    printf '%-15s connect-ms : %s\n' "$1" "$(stats "${connect[@]}")"
    printf '%-15s tls-ms     : %s\n' "$1" "$(stats "${appconnect[@]}")"
    printf '%-15s total-ms   : %s\n' "$1" "$(stats "${total[@]}")"
}

echo "================================================================"
echo " Polymarket network benchmark   (N=$N samples per endpoint)"
echo " Host: $(hostname)   $(date -u +%FT%TZ)"
echo "================================================================"
printf '%-15s %-12s  %6s  %6s  %6s  %6s\n' "endpoint" "metric" "min" "med" "p95" "max"
echo "----------------------------------------------------------------"

for i in "${!ENDPOINTS[@]}"; do
    bench_one "${ENDPOINTS[$i]}" | sed "s|${ENDPOINTS[$i]}|${LABELS[$i]}|g"
    echo
done
