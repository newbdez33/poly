# Context handoff — IntraWindowMomentum strategy ready for extended dry-run

**As of:** 2026-05-27 11:51 UTC (last status check)
**Goal:** Resume extended dry-run on the new strategy from another machine, then decide whether to go live.

---

## TL;DR

- Live trader has been **stopped** since 2026-05-23 07:28 UTC after a −$155 loss revealed maker-mode adverse selection.
- New strategy (IntraWindowMomentum) **passes backtest** (75.9% win rate, +$34,970 over 3 months — 6.9× the old strategy).
- A 25-minute dry-run yesterday produced 5/5 correct gate decisions, both directions, no crashes.
- **Pending:** longer dry-run (24h+ recommended) before flipping live.

---

## Current state

| Thing | State |
|---|---|
| VM | `155.248.180.11` (Tailscale `oci` / `100.64.0.103`) — Ubuntu 24.04, 1 GB RAM, x86_64 |
| systemd `poly-trader.service` | **inactive** (was running old strategy `--rsi-filter --maker`) |
| Trader binary on VM | `~/poly/target/release/poly-trader` — already built with `--intra-momentum` support |
| Last ladder in Redis | session `abd865eb` from yesterday's dry-run: 2W/2L/0 skip, −$3.75 PnL, `dry_run:true` |
| Lock key | empty |
| Wallet | $124.70 USDC (stale, last fetched 2026-05-18 — the balance refresher tunnel was down) |
| Local Windows trader | not running (Docker `poly-redis` also stopped) |
| Source repo | `~/projects/dev/poly` on Windows; `~/poly` on VM; pushed to `github.com/newbdez33/poly` (private) |

---

## What was done this session (the long version)

### Investigation: why old strategy lost money

The previous live config was:
```
--exit-rule tp-sl --tp-price 0.83 --rsi-filter --maker --fixed-stake --band-min 0.25 --band-max 0.75
```

Live PnL collapsed from ~breakeven to −$155 over 6 days. Root cause (verified by analyzing Redis events):

1. **Maker fill rate ~5%** — 94.7% of attempted entries timed out (FoK/maker-timeout). Backtest assumes 100% fill at ask.
2. **The 5% that did fill suffered adverse selection** — observed win rate 14% vs backtest expectation 62.8%. Sellers cross our maker bid exactly when they think the price is heading against us.

### Wallet analysis (script `analyze-wallet.py`)

Studied an active arb wallet `0xb55fa1296e6ec55d0ce53d93b9237389f11764d4`. **9 hours of data was the max** the data-api allows (offset hard-capped at 3000).

Findings:
- Pure buy-and-hold + Auto-Redeem (zero sells across 1000 BTC trades)
- Mid-window entry: median t+73s for 5m, t+162s for 15m
- **5m profitable (+$1,198, 56% win rate); 15m loses (−$81, 42% win rate)**
- Solo bets ROI **+57.8%** vs hedged bets **+9.2%** — alpha concentrated in conviction bets
- The 9-hour bp signal looked like *mean-reversion*, but that was sample-induced.

### Backtest (commit `0c36ff4`)

Added `DirectionSignal::IntraWindowMomentum` plus 6 parameter-sweep strategies (60–65). Ran 3 months of real-trade-oracle data:

| # | Range | Scan | Win% | PnL |
|---|---|---|---|---|
| 60 | 3-10 bp | 30-240 | 69.5% | $32,089 |
| **61** | **5-15 bp** | **30-240** | **75.9%** | **$34,970** ⭐ |
| 62 | 3-10 bp + TP=0.83 | 30-240 | 70.1% | $30,108 |
| 63 | 1-5 bp | 30-240 | 58.2% | $13,255 |
| 64 | 10-30 bp | 30-240 | 87.0% | $26,787 |
| 65 | 5-15 bp + late scan (60-180) | 74.3% | $27,595 |

**Strategy 61 wins** on absolute PnL. (Important: initial run with the *reversion* direction yielded −$36,471 / 30% win rate — sign was flipped to *momentum* and won.)

### Live code (commit `9e7afd2`)

Added `src/trader/momentum_gate.rs` + `MomentumGatedExec` wrapper in `src/bin/poly-trader.rs`. New CLI flags:

```
--intra-momentum
--intra-scan-start-secs 30
--intra-scan-end-secs 240
--intra-bp-min 5
--intra-bp-max 15
```

3 unit tests + 380 lib tests all pass.

### 25-minute dry-run on VM

Ran `poly-trader --intra-momentum --exit-rule hold --dry-run --reset` for 25 min on the VM. Five gate decisions:

```
1. 15:55:39  Up   bp=+5.02  t+39s   resolved
2. 16:00:30  Down bp=-7.73  t+30s   resolved
3. 16:05:30  Down bp=-6.32  t+30s   resolved
4. 16:10:30  Down bp=-7.77  t+30s   resolved
5. 16:15:30  Up   bp=+5.73  t+30s   in flight when stopped
```

Result on 4 resolved windows: **2W / 2L / 0 skip, −$3.75 PnL (dry-run sim — not reflective of real)**. Both directions triggered, no crashes, memory stable at 17 MB. Gate logic verified end-to-end.

The dry-run sample is tiny (n=4); 24h+ would give ~280 windows for a statistically meaningful comparison against the backtest's 75.9% expectation.

---

## What to do next from the other machine

### Prerequisites on the new machine

1. **SSH access to VM** — your existing key (Tailscale: hostname `oci`, IP `100.64.0.103`, or fallback `155.248.180.11`)
2. **Optional** — clone the repo for source reference:
   ```
   git clone https://github.com/newbdez33/poly.git
   ```
3. **Optional** — `tailscale up` to use MagicDNS

### Resume the dry-run

The VM already has the built binary and updated unit template. To start a fresh long dry-run:

```bash
ssh ubuntu@oci

# kill any old test session
tmux kill-session -t momentum-test 2>/dev/null

# clear lock + ladder so we start fresh
redis-cli DEL poly:prod:trader:lock poly:prod:trader:ladder

# launch in tmux
tmux new-session -d -s momentum-test 'cd ~/poly && ./target/release/poly-trader \
  --direction up --base 10 --max-step 5 \
  --band-min 0.001 --band-max 0.999 \
  --exit-rule hold \
  --intra-momentum \
  --intra-scan-start-secs 30 --intra-scan-end-secs 240 \
  --intra-bp-min 5 --intra-bp-max 15 \
  --fixed-stake --dry-run --reset \
  2>&1 | tee /tmp/momentum-dryrun.log; echo EXIT=$?'

# verify
sleep 3
ps -eo pid,etime,cmd | grep "release/poly-trader" | grep -v grep
tail -10 ~/poly/logs/trader.log.$(date -u +%Y-%m-%d)
```

### Monitoring commands

```bash
# count gate decisions
grep -c "momentum-gate" ~/poly/logs/trader.log.$(date -u +%Y-%m-%d)

# list all gate decisions
grep "momentum-gate" ~/poly/logs/trader.log.$(date -u +%Y-%m-%d)

# current ladder (win/lose counters + PnL)
redis-cli GET poly:prod:trader:ladder | python3 -m json.tool

# event stream tail
redis-cli XREVRANGE poly:prod:trader:events + - COUNT 10
```

### Stop the dry-run when done

```bash
tmux kill-session -t momentum-test
redis-cli DEL poly:prod:trader:lock
```

### Going live (after dry-run confirms)

```bash
# on the VM
sudo cp ~/poly/docs/systemd/poly-trader-momentum.service \
        /etc/systemd/system/poly-trader.service
sudo systemctl daemon-reload
redis-cli DEL poly:prod:trader:lock poly:prod:trader:ladder
sudo systemctl reset-failed poly-trader
sudo systemctl restart poly-trader

# verify
systemctl status poly-trader --no-pager | head -10
redis-cli GET poly:prod:trader:lock
```

If you want to start with a smaller stake (recommended for first 24h live), edit the unit's `--base` flag down to 5 before copying.

---

## Key files in the repo

| Path | What it is |
|---|---|
| `src/backtest/config.rs` | Strategy 60–65 definitions (IntraWindowMomentum sweep) |
| `src/backtest/runner.rs` | `simulate_intra_window_momentum()` |
| `src/trader/momentum_gate.rs` | Live gate (with 3 unit tests) |
| `src/bin/poly-trader.rs` | `MomentumGatedExec` wiring + CLI flags |
| `src/trader/config.rs` | `--intra-momentum` flag + validation |
| `docs/systemd/poly-trader-momentum.service` | Ready-to-deploy systemd unit |
| `analyze-wallet.py` | The wallet analyzer (BTC bp buckets, hedge stats) |

---

## Commits in this session (newest last)

```
9e7afd2  feat(trader): live IntraWindowMomentum gate (--intra-momentum)
0c36ff4  feat(backtest): add IntraWindowMomentum strategy (3-month +$34970)
1a6203f  feat(poly-status): mobile-responsive layout
6fe9297  feat(poly-status): render timestamps in JST instead of UTC
6d34162  docs: nginx Tailscale-only listener template for poly-status
158c8c8  fix(systemd): self-heal stale trader lock via ExecStartPre
72101ba  docs: add Polymarket network benchmark script
6a694c3  feat: poly-status binary renders HTML snapshot from Redis
bb82adf  feat(v1.15): widen RSI strategy band [0.45,0.55] -> [0.25,0.75]
b3dbcc4  docs: remote-TUI workflow + systemd template + lock-on-restart gotcha
e4bcca1  fix(maker): use ladder share count directly instead of dollars/price round-trip
a68d1e6  docs: add Linux deployment + hardware requirements section
```

---

## Caveats / known unknowns

1. **Backtest 75.9% might be optimistic.** Backtest assumes 100% taker fill at the ask shown by the real-trade oracle at the trigger second. Live taker FoK with our small $5 size should fill ≥95% of the time, but **adverse selection at the gate's trigger moment is uncharacterized**. Specifically: at the moment BTC has moved 5-15 bp, the Polymarket book may have already priced it in, so our taker may pay 1–3 cents above the oracle's mid.
2. **Sample size matters.** A 24-hour dry-run gives ~280 windows. To detect a real 75.9% vs random 50% at p<0.01 you need ~70 trades. 24h should be plenty.
3. **The `hold` exit rule has no SL.** If BTC reverses violently, the position resolves to $0. Backtest already includes these scenarios. Be mentally prepared for individual losing windows of −$5 each at base=10.
4. **Auto-Redeem is required.** Without it, won shares stay on the conditional-token contract until manual `poly-redeem`. Verify via Polymarket portfolio UI before going live.
5. **Maker peers may copy the trade.** Once we (and others) trade momentum at 5 bp, the book may adjust. The +35k/3mo backtest could compress.

---

## Useful one-liners while the dry-run runs

```bash
# wait for N gate decisions (set N=20 for ~100 min of data)
until [ "$(grep -c momentum-gate ~/poly/logs/trader.log.$(date -u +%Y-%m-%d))" -ge 20 ]; do sleep 60; done

# print summary table of all decisions
grep momentum-gate ~/poly/logs/trader.log.$(date -u +%Y-%m-%d) | \
  sed -E 's/.*momentum-gate: trade (\w+) bp=(-?[0-9.]+) t\+([0-9]+)s window=([0-9]+).*/\4  \1  bp=\2  t+\3s/'

# win rate so far
redis-cli GET poly:prod:trader:ladder | \
  python3 -c 'import json,sys; d=json.load(sys.stdin); w,l=d["windows_won"],d["windows_lost"]; print(f"{w}W / {l}L = {100*w/(w+l) if w+l else 0:.1f}% on n={w+l}")'
```

---

End of handoff. Pick up from "Resume the dry-run" on the new machine.
