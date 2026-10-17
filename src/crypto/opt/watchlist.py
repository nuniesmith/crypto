"""What is trending, and which liquid coins are in a strong trend, right now.

INFORMATION ONLY. trending.py tested buying coins like these: re-picking three
satellites every month by 30-day return, 90-day return, or strength against
BTC lost to holding BTC/ETH/SOL alone from almost every start date. This shows
what those screens see today, so the question can be checked by eye.

Sources, all public and keyless: CoinGecko /search/trending (what people are
searching for) and /coins/markets (the largest coins); Kraken /AssetPairs
(what can be bought against USD where the bot trades) and /OHLC daily
candles (the 200-day rule's own input, priced on Kraken).

Run: `python -m crypto.opt.watchlist` -> data/basket_results/watchlist_<day>.csv
"""

from __future__ import annotations

import time

import pandas as pd
import requests

from crypto.opt.basket import DATA, SMA_DAYS, sma200_band

OUT = DATA / "basket_results"
CG = "https://api.coingecko.com/api/v3"
KRAKEN = "https://api.kraken.com/0/public"
TOP = 100  # the largest coins by market cap to consider
MIN_KRAKEN_VOLUME = (
    1_000_000  # USD a day on Kraken: enough to fill $5-50 orders at the touch
)
BASKET = ("BTC", "ETH", "SOL", "LINK", "XRP", "INJ")
KRAKEN_CODE = {"BTC": "XBT", "DOGE": "XDG"}
STABLE = {
    "DAI",
    "FDUSD",
    "USDE",
    "USDS",
    "PYUSD",
    "TUSD",
    "USD1",
    "BUIDL",
    "USYC",
    "EURC",
    "USDG",
    "RLUSD",
}
DERIVATIVE = (
    "wrapped",
    "staked",
    "bridged",
    "lido",
    "binance-peg",
    "restaked",
    "tether gold",
    "pax gold",
)


def get(url: str, **params) -> dict | list:
    for attempt in range(5):
        r = requests.get(url, params=params, timeout=30)
        if r.status_code == 429:  # CoinGecko's keyless limit: back off and retry
            time.sleep(20 * (attempt + 1))
            continue
        r.raise_for_status()
        return r.json()
    r.raise_for_status()
    raise RuntimeError(url)


def is_coin(m: dict) -> bool:
    """False for stablecoins, wrapped/staked/bridged copies and tokenized gold."""
    sym, name = m["symbol"].upper(), m["name"].lower()
    if "USD" in sym or sym in STABLE or any(w in name for w in DERIVATIVE):
        return False
    price, move = (
        m.get("current_price") or 0,
        m.get("price_change_percentage_30d_in_currency"),
    )
    return not (0.97 < price < 1.03 and move is not None and abs(move) < 2)


def kraken_usd_pairs() -> dict[str, str]:
    """Base asset (Kraken's code) -> the USD pair's name for /OHLC."""
    res = get(f"{KRAKEN}/AssetPairs")["result"]
    return {
        v["wsname"].split("/")[0]: v["altname"]
        for v in res.values()
        if v.get("wsname", "").endswith("/USD")
        and v.get("status", "online") == "online"
    }


def daily(pair: str) -> pd.DataFrame:
    """Kraken's closed daily candles for `pair` (its last 720 days)."""
    res = get(f"{KRAKEN}/OHLC", pair=pair, interval=1440)
    if res.get("error"):
        raise RuntimeError(f"{pair}: {res['error']}")
    rows = next(v for k, v in res["result"].items() if k != "last")
    df = pd.DataFrame(
        rows,
        columns=["time", "open", "high", "low", "close", "vwap", "volume", "count"],
    )
    df.index = pd.to_datetime(df["time"], unit="s", utc=True)
    df = df[["close", "vwap", "volume"]].astype(float)
    return df[
        df.index < pd.Timestamp.now(tz="UTC").normalize()
    ]  # today's is still forming


def trend(df: pd.DataFrame) -> dict:
    c = df["close"].to_numpy()
    out = {"last_day": df.index[-1].date().isoformat(), "days": len(c), "close": c[-1]}
    if len(c) >= SMA_DAYS:
        sma = c[-SMA_DAYS:].mean()
        state = sma200_band(c)[-1]
        out["trend_200d"] = "bull" if state == 1.0 else "bear"
        out["vs_200d_avg"] = c[-1] / sma - 1
    for d in (30, 90):
        if len(c) > d:
            out[f"return_{d}d"] = c[-1] / c[-1 - d] - 1
    out["kraken_usd_volume"] = float((df["volume"] * df["vwap"]).iloc[-30:].mean())
    return out


def main() -> None:
    OUT.mkdir(exist_ok=True)
    markets = get(
        f"{CG}/coins/markets",
        vs_currency="usd",
        order="market_cap_desc",
        per_page=150,
        page=1,
        price_change_percentage="30d,1y",
    )
    trending = get(f"{CG}/search/trending")["coins"]
    hot = {
        c["item"]["symbol"].upper(): rank for rank, c in enumerate(trending, start=1)
    }
    names = {c["item"]["symbol"].upper(): c["item"]["name"] for c in trending}
    caps = {
        c["item"]["symbol"].upper(): c["item"].get("market_cap_rank") for c in trending
    }

    coins: dict[str, dict] = {}
    for m in markets:
        sym = m["symbol"].upper()
        if is_coin(m) and sym not in coins and len(coins) < TOP:
            coins[sym] = {
                "name": m["name"],
                "cap_rank": m.get("market_cap_rank"),
                "return_1y_cg": m.get("price_change_percentage_1y_in_currency"),
            }
    for sym in hot:  # trending coins outside the top 100 still get a row
        coins.setdefault(
            sym, {"name": names[sym], "cap_rank": caps[sym], "return_1y_cg": None}
        )

    usd = kraken_usd_pairs()
    rows = []
    for sym, info in coins.items():
        row = {
            "coin": sym,
            **info,
            "trending_rank": hot.get(sym),
            "on_kraken_usd": False,
        }
        pair = usd.get(KRAKEN_CODE.get(sym, sym))
        if pair:
            row["on_kraken_usd"] = True
            try:
                row.update(trend(daily(pair)))
            except Exception as e:  # noqa: BLE001 - one bad pair must not sink the list
                row["error"] = str(e)
            time.sleep(1.1)  # Kraken's public limit
        rows.append(row)

    df = pd.DataFrame(rows)
    btc90 = df.loc[df.coin == "BTC", "return_90d"].iloc[0]
    df["vs_btc_90d"] = df["return_90d"] - btc90
    day = df.loc[df.coin == "BTC", "last_day"].iloc[0]
    df.to_csv(OUT / f"watchlist_{day}.csv", index=False)

    pct = ["vs_200d_avg", "return_30d", "return_90d", "vs_btc_90d"]
    show = [
        "coin",
        "cap_rank",
        "trending_rank",
        "trend_200d",
        *pct,
        "kraken_usd_volume",
    ]

    def fmt(t: pd.DataFrame) -> str:
        t = t[show].copy()
        for c in pct:
            t[c] = t[c].map(lambda x: "" if pd.isna(x) else f"{x:+.0%}")
        t["kraken_usd_volume"] = t["kraken_usd_volume"].map(
            lambda x: "" if pd.isna(x) else f"${x / 1e6:,.1f}M"
        )
        return t.to_string(index=False)

    print(f"Kraken daily closes through {day}.\n\nThe live basket:")
    print(
        fmt(df[df.coin.isin(BASKET)].set_index("coin").loc[list(BASKET)].reset_index())
    )
    print("\nTrending on CoinGecko now:")
    t = df[df.trending_rank.notna()].sort_values("trending_rank")
    print(fmt(t[t.on_kraken_usd]))
    off = t.loc[~t.on_kraken_usd, "coin"].tolist()
    if off:
        print("not on Kraken against USD:", ", ".join(off))
    print(
        f"\nStrongest liquid coins on Kraken (top {TOP} by market cap, 200-day bull, >= $1M a day):"
    )
    strong = df[
        (df.trend_200d == "bull")
        & (df.kraken_usd_volume >= MIN_KRAKEN_VOLUME)
        & ~df.coin.isin(BASKET)
    ]
    print(fmt(strong.sort_values("vs_btc_90d", ascending=False).head(15)))
    n_bull = int((df.trend_200d == "bull").sum())
    n_read = int(df.trend_200d.notna().sum())
    print(
        f"\n{n_bull} of {n_read} Kraken-listed coins with 200 days of history are in a 200-day bull."
    )


if __name__ == "__main__":
    main()
