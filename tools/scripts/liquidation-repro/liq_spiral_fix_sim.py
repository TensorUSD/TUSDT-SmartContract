#!/usr/bin/env python3
"""
Fix-validation sim: same fixed-point mirrors, three changes applied.

FIX-0 (HF scale)  risk.rs:250-253 — inner must be rescaled by 1e18.
FIX-A (clamp)     lib.rs:2129-2132 — clamp seizure to available alpha and
                  BACK-COMPUTE the debt actually covered (ceil on the cover so
                  the liquidator never gets free collateral).
FIX-B (full close) lib.rs:2052-2057 — close_factor = 100% when HF < 0.95.
FIX-C (deficit)   record unpayable residual instead of leaving it in the market.
"""
from fractions import Fraction
from liq_spiral_repro import (Pool, E18, E9, LT, CF, BONUS, CLOSE, YIELD, mul_value,
                              div_value, div_value_ceil, ratio_mul, div_int, bps, fmt,
                              ALPHA, D_TAO0, D_TUSDT0, IDX0, IDX1, ALPHA_PRICE_RAO, fresh)

FULL_CLOSE_HF = bps(9500)   # 0.95 — proposed new global param


class FixedPool(Pool):
    # FIX-0: HF as a true 1e18 ratio
    def hf_inner(self):
        d = self.debt_value()
        if d == 0:
            return None
        c = self.collateral_value()
        if c == 0:
            return 0
        return mul_value(LT, c) * E18 // d          # <-- the missing rescale

    def is_liquidatable_fixed(self):
        hf = self.hf_inner()
        return False if hf is None else hf < E18

    def liquidate_fixed(self, debt_market, debt_to_cover):
        out = {"market": debt_market, "bad_debt": 0}
        hf = self.hf_inner()
        if hf is None or hf >= E18:
            out["err"] = "NotLiquidatable"
            return out

        debt_value = self.debt_value()
        # FIX-B: full close below the deep-underwater threshold
        close = E18 if hf < FULL_CLOSE_HF else CLOSE
        max_cover = mul_value(close, debt_value)
        out["close_used"] = close
        cover = (mul_value(self.oracle_inner, debt_to_cover)
                 if debt_market == 0 else debt_to_cover)
        cover = min(cover, max_cover)
        out["max_cover"] = max_cover
        if cover == 0:
            out["err"] = "ZeroAmount"
            return out

        borrower_debt = self.face0() if debt_market == 0 else self.face1()
        actual = (min(div_value(self.oracle_inner, cover), borrower_debt)
                  if debt_market == 0 else min(cover, borrower_debt))
        if actual == 0:
            out["err"] = "ZeroAmount"
            return out
        cover_value = (mul_value(self.oracle_inner, actual)
                       if debt_market == 0 else actual)

        alpha_to_seize, alpha_principal_to_seize = self.compute_seizure(cover_value)
        clamped = False
        if alpha_principal_to_seize > self.alpha_principal:
            # ── FIX-A ──────────────────────────────────────────────────────
            clamped = True
            alpha_principal_to_seize = self.alpha_principal
            # effective alpha actually transferable (floor: never move more
            # stake than the principal backs)
            alpha_to_seize = mul_value(YIELD, alpha_principal_to_seize)
            # value of what is actually seized (floor: never credit the
            # liquidator with value the collateral does not have)
            seized_value = mul_value(self.collateral_price(), alpha_to_seize)
            # cover the liquidator must pay for it, CEIL: liquidator never
            # receives collateral cheaper than value/(1+bonus)
            cover_value = div_value_ceil(E18 + BONUS, seized_value)
            if debt_market == 0:
                # TAO units, CEIL then clamp: liquidator pays at least the value
                actual = min(div_value_ceil(self.oracle_inner, cover_value), borrower_debt)
                cover_value = mul_value(self.oracle_inner, actual)
            else:
                actual = min(cover_value, borrower_debt)
                cover_value = actual
            if actual == 0:
                out["err"] = "ZeroAmount(dust collateral)"
                return out

        out.update(actual=actual, cover_value=cover_value, clamped=clamped,
                   alpha_to_seize=alpha_to_seize,
                   alpha_principal_to_seize=alpha_principal_to_seize)

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
        # FIX-C: collateral exhausted and debt remains -> deficit
        if self.alpha_principal == 0 and (self.scaled0 or self.scaled1):
            out["bad_debt"] = self.debt_value()
        return out


def run_fixed(title, oracle_inner, alpha_price_rao=ALPHA_PRICE_RAO, max_it=12):
    print("=" * 128)
    print(title)
    print("=" * 128)
    p = FixedPool(ALPHA, D_TAO0, D_TUSDT0, IDX0, IDX1, oracle_inner, alpha_price_rao)
    print("it |  alpha remaining |  debt TAO  |  debt TUSDT  |   HF   | close |   max cover  "
          "|  alpha seized  | result")
    print("-" * 128)
    hf0 = p.hf_inner()
    print(f" 0 | {fmt(p.alpha_principal):>16} | {fmt(p.face0()):>10} | {fmt(p.face1()):>12} | "
          f"{hf0/E18:6.4f} |     - |            - |              - | initial "
          f"(C={fmt(p.collateral_value())} D={fmt(p.debt_value())}; "
          f"(1+b)*D={fmt(mul_value(E18+BONUS, p.debt_value()))})")
    for it in range(1, max_it + 1):
        if p.face0() == 0 and p.face1() == 0:
            print(f"{it:2d} | {fmt(p.alpha_principal):>16} |          - |            - |"
                  f"    -   |     - |            - |              - | DEBT CLEARED")
            break
        market = 0 if p.face0() > 0 else 1
        pre = (p.alpha_principal, p.face0(), p.face1(), p.hf_inner())
        r = p.liquidate_fixed(market, pre[1] if market == 0 else pre[2])
        tag = r.get("err") or (("CLAMPED " if r["clamped"] else "") +
                               f"ok m{market}: covered {fmt(r['actual'])} "
                               f"(value {fmt(r['cover_value'])})")
        cl = r.get("close_used")
        print(f"{it:2d} | {fmt(pre[0]):>16} | {fmt(pre[1]):>10} | {fmt(pre[2]):>12} | "
              f"{pre[3]/E18:6.4f} | {(str(cl*100//E18)+'%') if cl else '   - ':>5} | "
              f"{fmt(r.get('max_cover',0)):>12} | {fmt(r.get('alpha_to_seize',0)):>14} | {tag}")
        if r.get("err"):
            break
        if r["bad_debt"]:
            print(f"   -> COLLATERAL EXHAUSTED (alpha_principal == 0). "
                  f"BAD DEBT = {fmt(r['bad_debt'])} TUSDT of value "
                  f"({fmt(p.face0())} TAO + {fmt(p.face1())} TUSDT, "
                  f"scaled0={p.scaled0} scaled1={p.scaled1})")
            break
    print("-" * 128)
    C, D = p.collateral_value(), p.debt_value()
    print(f"FINAL: alpha={fmt(p.alpha_principal)} (value {fmt(C)}) | debt value {fmt(D)} | "
          f"alpha_principal == 0 ? {p.alpha_principal == 0}")
    if D:
        theo = D - Fraction(C * E18, E18 + BONUS)
        print(f"theoretical minimum unrecoverable debt = D - C/(1+b) = {fmt(int(theo))} TUSDT")
    print()


if __name__ == "__main__":
    # sanity: FIX-0 restores meaningful HF/gate values
    p = fresh()
    fp = FixedPool(ALPHA, D_TAO0, D_TUSDT0, IDX0, IDX1, 230 * E18, ALPHA_PRICE_RAO)
    print("FIX-0 sanity — same position, both HF formulas:")
    print(f"  as written (risk.rs:250-253): inner={p.hf_contract_inner()}  "
          f"-> is_liquidatable={p.is_liquidatable_contract()}   [WRONG: healthy 1.198 position]")
    print(f"  rescaled:                    inner={fp.hf_inner()} ({fp.hf_inner()/E18:.4f})  "
          f"-> is_liquidatable={fp.is_liquidatable_fixed()}")
    print()

    run_fixed("FIXED — Scenario A (oracle 60): clamp + full-close + deficit accounting", 60 * E18)
    run_fixed("FIXED — Scenario C (alpha -60%, oracle 230)", 230 * E18, alpha_price_rao=270_808)
    run_fixed("FIXED — Scenario D (oracle 148, HF 0.996): correct gate stops the drain", 148 * E18)
    run_fixed("FIXED — mild drop, HF in the self-healing band (oracle 130)", 130 * E18)
