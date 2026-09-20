from .vwap_mr import vwap_mean_reversion
from .ema_cross import ema_cross
from .range_break import range_breakout
from .ema_pullback import ema_pullback
from .trendline_break import trendline_break
from .structure import range_filtered, structure_filtered

STRATEGIES = {
    "vwap_mr": vwap_mean_reversion,
    "ema_cross": ema_cross,
    "range_break": range_breakout,
    "ema_pullback": ema_pullback,
    "trendline_break": trendline_break,
    "structure_filtered": structure_filtered,
    "range_filtered": range_filtered,
}

__all__ = list(STRATEGIES.keys()) + ["STRATEGIES"]
