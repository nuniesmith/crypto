# Detailed coin analysis — 2026-09-23

Resumable. Each stage records its own verdict; a later stage can start from
the previous stage's table without re-running it.

## Stage 1 — screen the 28-coin grid for double-window survivors ✅ DONE

Gate: beat buy-and-hold on BOTH the in-sample and holdout windows, with at
least 8 holdout and 20 in-sample trades. Source:
`20260923T004306Z_direction.json`.

**6 of 1,512 grid cells survive. All six are 240m (4h). None is the 1h
engine that runs live.**

| pair | tf | strategy | hold | hN | hPnL | h vs BH | iN | i vs BH | PF |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|
| FILUSD  | 240 | ema_cross      | 1440  | 15 | 381 | **+36** | 69 | +941 | 3.93 |
| SEIUSD  | 240 | range_filtered | 1440  | 15 | 304 | +7      | 85 | +634 | 2.83 |
| ATOMUSD | 240 | range_break    | 1440  | 17 | 354 | **+86** | 78 | +359 | 4.60 |
| IMXUSD  | 240 | range_break    | 10080 |  8 | 200 | +1      | 50 | +432 | 2.25 |
| SEIUSD  | 240 | range_break    | 1440  | 16 | 297 | +1      | 96 | +389 | 2.50 |
| ATOMUSD | 240 | range_filtered | 1440  | 16 | 273 | +5      | 73 | +343 | 3.77 |

### Do not read the IS column as edge

In-sample was a bear window: every one of the 28 coins lost between **-52%
and -87%** on buy-and-hold. Beating buy-and-hold there is close to automatic
for anything that spends time in cash — losing less than holding is not the
same as making money. The large `i vs BH` figures are a property of the
window, not evidence of a signal.

**The holdout excess is the real test.** Only ATOM (+86) and FIL (+36) have a
margin big enough to be worth anything; +1 to +7 on the other four is noise
at these trade counts.

### Carried forward to Stage 2

- `ATOMUSD 240m range_break hold=1440`
- `FILUSD 240m ema_cross hold=1440`

Both are thin (17 and 15 holdout trades) and neither is the live 1h engine.

## Stage 2 — walk-forward + locked holdout on the two real candidates ✅ DONE

`crypto study --interval 240 --days 365 --holdout-days 60 --folds 6 --trials 60`

**Both candidates fail. Neither should be traded.**

### ATOMUSD — the +$354 holdout was the bounce, not the strategy

Stage 1 liked `range_break` on the 60-day holdout: 17 trades, +$354, PF 4.60,
+86 vs buy-and-hold. Evaluated over the **full year** instead of that one
window, the identical spec loses at every fee tier:

| fee tier | trades | PnL | PF | fees |
|---|---:|---:|---:|---:|
| t1 maker | 65 | **-652.08** | 0.63 | 787.96 |
| t3 maker | 65 | **-264.70** | 0.82 | 400.57 |
| t6 maker (best case) | 65 | **-116.01** | 0.92 | 251.88 |

Buy-and-hold on the same data: IS **-$682 (-67.7%)**, holdout **+$268
(+27.7%)**. The holdout is a bounce in which being long paid; the strategy
was long some of the time and captured part of it. That is not an edge.

Study verdict: *"Nothing survived the locked holdout with PF>1.05 after
fees."* Walk-forward leader was `range_filtered` at $64.76/fold, +EV in 3 of
4 folds — and the locked holdout still killed it.

### FILUSD — worse

*"Walk-forward medians and holdout PnL are ≤ 0 after costs for every default
family."* Leader `vwap_mr` at **-$22.30/fold**, +EV in **1 of 4** folds.

### Fees are the first-order term, not the signal

At the **best** tier ATOM's 65 trades cost **$251.88** against a $1,000 book
— roughly a quarter of book value per year in costs alone. Moving t1 → t6
swings PnL by $536 on an identical trade sequence. The study says it outright:
fee tier is the first-order variable. No parameter search fixes that.

### Two tool caveats worth recording

- The `study` summary still says *"1-minute scalps"* and *"1-minute family"*
  regardless of `--interval`, and prints `cov=0.42%` because coverage is
  computed against 1-minute expectations. **Stale boilerplate from when the
  tool was 1m-only.** The evaluation itself is correct — ATOM ran on 2,191
  bars over 365 days, which is 4h. Verified before trusting the verdict.
- `range_break` *was* among the seven families evaluated (checked explicitly
  rather than assumed), so the candidate really was tested.

## Verdict

**48 coins, two screens, and now a walk-forward plus locked holdout on the
only two names with a real holdout margin. Nothing has produced a tradeable
edge.**

The live sleeves stay ETH/SOL 1h at 20%, and that allocation remains tuition
rather than a proven edge.

### If this is picked up again

Stage 3 would not be more coins. The evidence points at cost, not selection:
every survivor was 4h (never the live 1h), the IS "wins" are a bear-window
artifact, and fee tier moves results more than any parameter. Worth more than
another screen:

1. Measure the real Kraken maker fill-rate rather than assuming maker entry.
   Every +EV result above depends on maker fills that were never verified.
2. Establish what fee tier the account actually gets at its real volume. The
   t1→t6 swing is larger than any edge found so far.
3. Re-test the *live* 1h sleeves against a corrected buy-and-hold benchmark —
   the one in the Rust books was broken (see crypto#9) and every live
   comparison used it.
