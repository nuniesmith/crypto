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

