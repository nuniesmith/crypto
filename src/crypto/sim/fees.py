"""Kraken Pro spot fee tiers (July 2026 cross-platform schedule).

Percentages are of notional. We model maker vs taker per leg so the
optimizer can reward limit-order logic.
"""

from __future__ import annotations

from dataclasses import dataclass

# (maker_pct, taker_pct) — fractions, not percent points
FEE_TIERS: dict[int, tuple[float, float]] = {
    1: (0.0040, 0.0080),
    2: (0.0030, 0.0060),
    3: (0.0022, 0.0038),
    4: (0.0020, 0.0035),
    5: (0.0015, 0.0030),
    6: (0.0012, 0.0025),
    7: (0.0010, 0.0022),
    8: (0.0008, 0.0020),
    9: (0.0006, 0.0018),
    10: (0.0004, 0.0015),
    11: (0.0002, 0.0012),
    12: (0.0000, 0.0010),
}


@dataclass(frozen=True)
class FeeModel:
    maker: float
    taker: float
    # extra adverse slippage in fraction of price (e.g. 0.0001 = 1 bp)
    slippage: float = 0.0001

    @classmethod
    def from_tier(cls, tier: int, slippage: float = 0.0001) -> "FeeModel":
        tier = max(1, min(12, tier))
        m, t = FEE_TIERS[tier]
        return cls(maker=m, taker=t, slippage=slippage)

    def cost(self, notional: float, is_maker: bool) -> float:
        rate = self.maker if is_maker else self.taker
        return notional * (rate + self.slippage)
