#!/usr/bin/env python3
"""
Forensic numeric repro of the tusdt-lending-pool liquidation spiral.

Mirrors EXACTLY the contract fixed-point helpers:
  primitives/src/lib.rs:108  checked_mul_value(v)      = floor(v * inner / 1e18)
  primitives/src/lib.rs:123  checked_div_value(v)      = floor(v * 1e18 / inner)
  primitives/src/lib.rs:136  checked_div_value_ceil(v) = ceil (v * 1e18 / inner)
  primitives/src/lib.rs:146  checked_div_int(n)        = Ratio(inner // n)   <-- NOTE: no rescale
  Ratio inner scale = 1e18 (FIXED_SCALE), Balance = u64 rao (9 decimals).

Contract sites modelled:
  risk.rs:169-190  get_collateral_value_tusdt
  risk.rs:196-234  get_debt_value_tusdt
  risk.rs:240-255  get_health_factor
  risk.rs:281-286  is_liquidatable
  risk.rs:129-163  compute_liquidation_seizure
  lib.rs:2050-2132 liquidate() cover/close-factor/seizure/guard
"""

from fractions import Fraction

E18 = 10**18
E9 = 10**9
U64_MAX = 2**64 - 1

# ── contract helper mirrors ────────────────────────────────────────────────
def mul_value(inner, v):            # Ratio::checked_mul_value
    return (inner * v) // E18

def div_value(inner, v):            # Ratio::checked_div_value  (v / ratio, floor)
    return (v * E18) // inner

def div_value_ceil(inner, v):       # Ratio::checked_div_value_ceil (v / ratio, ceil)
    return (v * E18 + inner - 1) // inner

def div_int(inner, n):              # Ratio::checked_div_int -> Ratio(inner // n)
    return inner // n

def ratio_mul(a, b):                # Ratio::checked_mul  = floor(a*b/1e18)
    return (a * b) // E18

def bps(x):                         # Ratio::from_basis_points
    return x * E18 // 10_000

# ── default params (params.rs:179-199) ────────────────────────────────────
CF    = bps(5000)    # collateral_factor      50%
LT    = bps(6000)    # liquidation_threshold  60%
BONUS = bps(500)     # liquidation_bonus       5%
CLOSE = bps(5000)    # close_factor           50%
YIELD = E18          # netuid_yield_index      1.0 (no alpha yield accrued)


class Pool:
    """Single borrower, one alpha netuid, two debt markets."""

    def __init__(self, alpha_principal, scaled0, scaled1, idx0, idx1,
                 oracle_inner, alpha_price_rao):
        self.alpha_principal = alpha_principal      # rao alpha
        self.scaled0, self.scaled1 = scaled0, scaled1
        self.idx0, self.idx1 = idx0, idx1           # borrow_index inners
        self.oracle_inner = oracle_inner            # TUSDT per TAO
        self.alpha_price_rao = alpha_price_rao      # alpha price in RAO of TAO

    # risk.rs:28-37 collateral_price = oracle * (alpha_price_rao / 1e9)
    def collateral_price(self):
        alpha_to_tao = div_int(self.alpha_price_rao * E18, E9)   # risk.rs:41-45
        return ratio_mul(self.oracle_inner, alpha_to_tao)

    def effective_alpha(self):                                   # risk.rs:50-65
        return mul_value(YIELD, self.alpha_principal)

    def collateral_value(self):                                  # risk.rs:169-190
        return mul_value(self.collateral_price(), self.effective_alpha())

    def face0(self):                                             # market 0 debt (TAO)
        return mul_value(self.idx0, self.scaled0) if self.scaled0 else 0

    def face1(self):                                             # market 1 debt (TUSDT)
        return mul_value(self.idx1, self.scaled1) if self.scaled1 else 0

    def debt_value(self):                                        # risk.rs:196-234
        tao_tusdt = mul_value(self.oracle_inner, self.face0()) if self.face0() else 0
        return self.face1() + tao_tusdt

    # risk.rs:240-255 — AS WRITTEN (missing 1e18 rescale)
    def hf_contract_inner(self):
        d = self.debt_value()
        if d == 0:
            return None
        c = self.collateral_value()
        if c == 0:
            return 0
        return div_int(mul_value(LT, c), d)

    def is_liquidatable_contract(self):                          # risk.rs:281-286
        hf = self.hf_contract_inner()
        return False if hf is None else hf < E18

    # what the doc-comment/UI means by HF (exact rational)
    def hf_true(self):
        d = self.debt_value()
        if d == 0:
            return None
        return Fraction(LT * self.collateral_value(), E18 * d)

    # risk.rs:129-163
    def compute_seizure(self, cover_value_tusdt):
        bonus_mult = E18 + BONUS
        collateral_value_tusdt = mul_value(bonus_mult, cover_value_tusdt)
        alpha_to_seize = div_value(self.collateral_price(), collateral_value_tusdt)
        alpha_principal_to_seize = div_value(YIELD, alpha_to_seize)
        return alpha_to_seize, alpha_principal_to_seize

    # lib.rs:1999-2230 (state-changing part; returns dict describing the call)
    def liquidate(self, debt_market, debt_to_cover, enforce_gate=True):
        out = {"market": debt_market}
        if enforce_gate and not self.is_liquidatable_contract():
            out["err"] = "NotLiquidatable"          # lib.rs:2037-2040
            return out

        debt_value = self.debt_value()
        max_cover = mul_value(CLOSE, debt_value)                    # lib.rs:2052-2057
        cover = (mul_value(self.oracle_inner, debt_to_cover)         # lib.rs:2060-2067
                 if debt_market == 0 else debt_to_cover)
        cover = min(cover, max_cover)                                # lib.rs:2068
        out["max_cover"] = max_cover
        if cover == 0:
            out["err"] = "ZeroAmount"                               # lib.rs:2069-2072
            return out

        borrower_debt = self.face0() if debt_market == 0 else self.face1()
        if debt_market == 0:                                        # lib.rs:2091-2099
            actual = min(div_value(self.oracle_inner, cover), borrower_debt)
        else:
            actual = min(cover, borrower_debt)
        if actual == 0:
            out["err"] = "ZeroAmount"                               # lib.rs:2102-2105
            return out
        out["actual"] = actual

        cover_value = (mul_value(self.oracle_inner, actual)          # lib.rs:2108-2115
                       if debt_market == 0 else actual)
        out["cover_value"] = cover_value

        alpha_to_seize, alpha_principal_to_seize = self.compute_seizure(cover_value)
        out["alpha_to_seize"] = alpha_to_seize
        out["alpha_principal_to_seize"] = alpha_principal_to_seize

        if alpha_principal_to_seize > self.alpha_principal:          # lib.rs:2129-2132
            out["err"] = "CollateralAwardExceedsPosition"
            return out

        # effects — lib.rs:2141-2184
        idx = self.idx0 if debt_market == 0 else self.idx1
        scaled = self.scaled0 if debt_market == 0 else self.scaled1
        scaled_repaid = min(div_value_ceil(idx, actual), scaled)
        if debt_market == 0:
            self.scaled0 -= scaled_repaid
        else:
            self.scaled1 -= scaled_repaid
        self.alpha_principal -= alpha_principal_to_seize
        out["scaled_repaid"] = scaled_repaid
        out["err"] = None
        return out


# ── calibration: "borrowed the FULL available amount" ─────────────────────
# get_available_borrow_tusdt (risk.rs:261-276): max = CF * collateral_value.
# Borrower took 0.30 TAO (market 0) + 40 TUSDT (market 1) at oracle 230.
ALPHA = 1400 * E9                      # 1400 alpha units
ORACLE0 = 230 * E18                    # 230 TUSDT per TAO
D_TAO0 = 300_000_000                   # 0.30 TAO in rao
D_TUSDT0 = 40 * E9                     # 40 TUSDT
debt0_value = 40 * E9 + (230 * 300_000_000)      # 109 TUSDT
C_target = debt0_value * 2                        # CF = 50%  ->  C = 2 * debt
price_target_inner = C_target * E18 // ALPHA
ALPHA_PRICE_RAO = round(price_target_inner / (230 * E9))   # integer rao

# accrued borrow indices at liquidation time
IDX0 = 1_002_000_000_000_000_000                  # 1.002   (TAO market)
IDX1 = 1_000_316_946_018_546_683                  # live testnet index (TUSDT market)


def fresh(oracle_inner=ORACLE0, alpha_price_rao=ALPHA_PRICE_RAO):
    return Pool(ALPHA, D_TAO0, D_TUSDT0, IDX0, IDX1, oracle_inner, alpha_price_rao)


def fmt(x, dec=9):
    return f"{x / 10**dec:,.6f}"


def hf_str(p):
    t = p.hf_true()
    if t is None:
        return "  n/a  "
    return f"{float(t):7.4f}"


HDR = ("it | oracle |  alpha remaining |  debt TAO  |  debt TUSDT  |   HF   | HFcontract |"
       "   max cover  |  alpha seized  | result")


def run(title, oracle_path, alpha_price_rao=ALPHA_PRICE_RAO, enforce_gate=True,
        clear_tao_first=True, max_it=40):
    print("=" * 132)
    print(title)
    print(f"alpha_price_rao={alpha_price_rao} -> collateral_price(oracle=230) = "
          f"{fmt(ratio_mul(230*E18, alpha_price_rao*E18//E9), 18)} TUSDT/alpha")
    print("=" * 132)
    print(HDR)
    print("-" * 132)
    p = fresh(oracle_path[0], alpha_price_rao)
    it = 0
    p0 = fresh(oracle_path[0], alpha_price_rao)
    print(f"{0:2d} | {oracle_path[0]//E18:6d} | {fmt(p0.alpha_principal):>16} | "
          f"{fmt(p0.face0()):>10} | {fmt(p0.face1()):>12} | {hf_str(p0)} | "
          f"{p0.hf_contract_inner():10d} | {'-':>12} | {'-':>14} | initial state "
          f"(collateral value {fmt(p0.collateral_value())} TUSDT, debt value {fmt(p0.debt_value())})")
    fail = None
    while it < max_it:
        it += 1
        p.oracle_inner = oracle_path[min(it - 1, len(oracle_path) - 1)]
        if p.face0() == 0 and p.face1() == 0:
            print(f"{it:2d} |   ---  | {fmt(p.alpha_principal):>16} | {'0':>10} | {'0':>12} |"
                  f"    n/a  |        n/a |            - |              - | DEBT FULLY CLEARED")
            break
        market = 0 if (clear_tao_first and p.face0() > 0) else 1
        want = p.face0() if market == 0 else p.face1()
        pre_hf, pre_hfc = hf_str(p), p.hf_contract_inner()
        pre_alpha, pre_f0, pre_f1 = p.alpha_principal, p.face0(), p.face1()
        r = p.liquidate(market, want, enforce_gate=enforce_gate)
        res = r.get("err") or (f"ok m{market}: covered {fmt(r['actual'])} "
                               f"(value {fmt(r['cover_value'])} TUSDT)")
        print(f"{it:2d} | {p.oracle_inner//E18:6d} | {fmt(pre_alpha):>16} | {fmt(pre_f0):>10} | "
              f"{fmt(pre_f1):>12} | {pre_hf} | {pre_hfc:10d} | "
              f"{fmt(r.get('max_cover', 0)):>12} | {fmt(r.get('alpha_to_seize', 0)):>14} | {res}")
        if r.get("err"):
            fail = (it, r, pre_alpha, pre_f0, pre_f1)
            break
    print("-" * 132)
    print(f"FINAL: alpha_principal = {fmt(p.alpha_principal)} alpha "
          f"(value {fmt(p.collateral_value())} TUSDT) | debt TAO {fmt(p.face0())} "
          f"| debt TUSDT {fmt(p.face1())} | debt value {fmt(p.debt_value())} TUSDT "
          f"| scaled0={p.scaled0} scaled1={p.scaled1}")
    if fail:
        it, r, a, f0, f1 = fail
        print(f"FAILURE at iteration {it}: {r['err']}")
        if r["err"] == "CollateralAwardExceedsPosition":
            print(f"  requested alpha_principal_to_seize = {fmt(r['alpha_principal_to_seize'])} "
                  f"> available alpha_principal = {fmt(a)}  "
                  f"(overshoot {fmt(r['alpha_principal_to_seize'] - a)} alpha)")
            print(f"  residual UNPAYABLE debt: {fmt(p.debt_value())} TUSDT "
                  f"({fmt(p.face0())} TAO + {fmt(p.face1())} TUSDT)")
            print(f"  residual STRANDED collateral: {fmt(p.alpha_principal)} alpha "
                  f"= {fmt(p.collateral_value())} TUSDT  -> alpha_principal == 0 ? "
                  f"{p.alpha_principal == 0}")
            print(f"  collateral/debt ratio at failure = "
                  f"{float(Fraction(p.collateral_value(), max(p.debt_value(),1))):.4f} "
                  f"(1+bonus = {float(Fraction(E18+BONUS, E18)):.4f})")
    print()
    return p


def spiral_threshold_demo():
    print("=" * 132)
    print("SPIRAL THRESHOLD:  liquidating x of debt value seizes x*(1+b) of collateral value")
    print("  HF' < HF  <=>  LT*(C - x(1+b))/(D - x) < LT*C/D  <=>  (1+b)*D > C  <=>  HF < LT*(1+b)")
    print(f"  LT*(1+b) = 0.60 * 1.05 = {float(Fraction(LT,E18)*Fraction(E18+BONUS,E18)):.4f}")
    print("  Identical condition: C < (1+b)*D  <=>  collateral can never cover the debt at bonus")
    print("=" * 132)
    print(" HF at start | after 1 liq (50% close) | direction")
    print("-" * 60)
    for hf_target in ["0.99", "0.80", "0.6300", "0.62", "0.50", "0.40"]:
        hf = Fraction(hf_target)
        D = Fraction(109 * E9)
        C = hf * D / Fraction(LT, E18)
        x = D / 2
        C2, D2 = C - x * Fraction(E18 + BONUS, E18), D - x
        hf2 = Fraction(LT, E18) * C2 / D2 if D2 > 0 else None
        arrow = "DOWN (spiral)" if hf2 < hf else "UP (self-healing)"
        print(f"   {float(hf):8.4f} |        {float(hf2):8.4f}         | {arrow}")
    print()


if __name__ == "__main__":
    print(f"calibration: alpha_price_rao = {ALPHA_PRICE_RAO} rao/alpha")
    p = fresh()
    print(f"  collateral_value @230 = {fmt(p.collateral_value())} TUSDT, "
          f"debt_value = {fmt(p.debt_value())} TUSDT, "
          f"available_borrow left = {fmt(max(mul_value(CF, p.collateral_value()) - p.debt_value(), 0))} TUSDT")
    print(f"  HF(true) = {float(p.hf_true()):.4f}   HF(contract inner) = {p.hf_contract_inner()} "
          f"(compared against 1e18 at risk.rs:284 -> is_liquidatable = {p.is_liquidatable_contract()})")
    print()

    spiral_threshold_demo()

    # Scenario A: user's report — hard oracle drop, then repeated liquidations.
    run("SCENARIO A — oracle TUSDT/TAO 230 -> 60 (-74%), alpha/TAO price unchanged. "
        "Liquidator clears TAO first, then hits TUSDT repeatedly (contract gate as written).",
        [60 * E18])

    # Scenario B: gradual oracle step-down each iteration.
    run("SCENARIO B — oracle stepped down each iteration: 230,150,100,80,65,55,50,45,40...",
        [230*E18, 150*E18, 100*E18, 80*E18, 65*E18, 55*E18, 50*E18, 45*E18, 40*E18])

    # Scenario C: pure collateral shock — alpha depreciates vs TAO, oracle held at 230.
    run("SCENARIO C — alpha/TAO collapses -60% (677019 -> 270808 rao), oracle held at 230. "
        "Pure collateral shock: debt value untouched.",
        [230 * E18], alpha_price_rao=270_808)

    # Scenario D: what a CORRECT is_liquidatable gate would do in the mild band.
    run("SCENARIO D — mild drop (oracle 230 -> 148, HF~1.00): what the vacuous gate allows. "
        "Each liquidation RAISES HF (HF > LT*(1+b)) yet the contract keeps allowing them.",
        [148 * E18])
