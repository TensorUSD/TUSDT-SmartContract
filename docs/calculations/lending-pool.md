# tusdt-lending-pool — Calculation Guide

Step-by-step numeric walkthroughs for the lending pool: how APY is calculated, how
interest accrues, how your debt grows, and how/when liquidation works — all worked with
the exact integer math the contract uses. Companion to the reference page
[`../contracts/lending-pool.md`](../contracts/lending-pool.md) and the
[error catalog](../errors/lending-pool.md).

> **Verification**: every number on this page was recomputed with an independent Python
> harness that reimplements the contract's fixed-point operations exactly (truncating
> 1e18 `Ratio` mul/div, square-and-multiply `pow_fixed`, ceiling scaled-debt conversion)
> and reproduces every numeric pin in the contract test suite (`tests.rs`) — including
> the rate-curve points, the live-chain borrow index
> `1_000_316_946_018_546_683`, the mint/redeem exchange-rate tests, and the
> liquidation bookkeeping test (`index 1.4, scaled 3 → debt 4`).
>
> **One caveat**: the health-factor functions (`get_health_factor`,
> `get_available_borrow_tusdt`, `is_liquidatable`, collateral/debt value getters) are
> `pub` but **not** `#[ink(message)]` — they are not queryable on-chain. UIs compute
> them client-side with the formulas below (see `tusdt-app/src/lib/lendingHealth.ts`).

## Numbers and scales (read this first)

| Quantity | Scale | Example |
|---|---|---|
| Balance / token amount | `u64`, 9 decimals | 1 TAO = 1 TUSDT = 1 α = 1e9 rao |
| Internal `Ratio` (fixed-point) | 1e18 inner | 1.5 = 1.5 × 10¹⁸ |
| Parameter configs (external messages) | basis points | 4,000 bps = 40% |
| Oracle price (TUSDT/TAO) | 1e18 Ratio | 250 → inner 250 × 10¹⁸ |
| Chain alpha price (ext fn 15) | rao, 1e9 per α | 300,000,000 rao = 0.3 TAO/α |
| Yield index / borrow index / exchange rate | 1e18 Ratio | grow over time |
| Time | milliseconds | 1 h = 3,600,000 ms; year = 8,760 h |

Markets: **0 = TAO**, **1 = TUSDT** (supply + borrow, accruing); **≥ 2 = alpha
collateral-only markets** (no accrual, no supply/borrow).

Rounding directions (the ones that matter): **borrow** converts face → scaled with
**ceiling** (`checked_div_value_ceil`, Aave `rayDivUp`); **every read** of scaled →
face uses **floor** (`floor(scaled × index / 1e18)`); lToken **mint** = floor
(`amount / exchange_rate`), **redeem** = floor (`ltokens × exchange_rate`); repay
clamps to `min(amount, current debt)` and its scaled conversion is ceiling too.

## 1. The interest-rate curve (how APY is calculated)

Utilization is computed against the market's total liquidity
(`rates.rs:57-66`):

$$U = \frac{total\_debt}{total\_debt + cash}$$

`cash` for market 0 (TAO) = the pool's native balance + TAO staked on the root subnet
(1:1); for market 1 (TUSDT) = the pool's TUSDT balance.

The **annual borrow rate** (`compute_borrow_rate`, rates.rs:144-168):

$$r = \begin{cases} base + slope_1 \cdot \dfrac{U}{U_{opt}} & U \le U_{opt} \\[4pt]
base + slope_1 + slope_2 \cdot \dfrac{U - U_{opt}}{1 - U_{opt}} & U > U_{opt} \end{cases}$$

The **annual supply rate** is the borrowers' interest after the reserve slice:

$$s = r \times U \times (1 - reserve\_factor)$$

Defaults: **TAO** base 0 / slope1 4% / slope2 96% / optimal 80% / reserve 20%;
**TUSDT** base 0 / slope1 3% / slope2 97% / optimal 80% / reserve 20%.

**Worked curve points** (exact integer math; `bps = value × 1e14`, `U` inner = % × 1e16):

```text
U = 40%: fraction = (4e17 × 1e18) // 8e17 = 5e17 (0.5); r = 0 + 4e16 × 5e17 // 1e18 = 2e16  → 2.0%
U = 90%: excess = 1e17; range = 2e17; fraction = 5e17
         r = 4e16 + 9.6e17 × 5e17 // 1e18 = 4e16 + 4.8e17 = 5.2e17                → 52.0%
U =100%: fraction = 1e18; r = 4e16 + 9.6e17 = 1e18                                 → 100.0%
```

| U | TAO borrow | TAO supply | TUSDT borrow | TUSDT supply |
|---|---|---|---|---|
| 0% | 0.0% | 0.0% | 0.0% | 0.0% |
| 10% | 0.5% | 0.04% | 0.375% | 0.03% |
| 40% | 2.0% | 0.64% | 1.5% | 0.48% |
| 80% | 4.0% | 2.56% | 3.0% | 1.92% |
| 90% | 52.0% | 37.44% | 51.5% | 37.08% |
| 100% | 100.0% | 80.0% | 100.0% | 80.0% |

`get_borrow_rate` / `get_supply_rate` return these **annual** 1e18 ratios — client APY
display = `inner / 1e18 × 100`. The ÷8,760 (hours per year) happens inside the contract.

## 2. How interest accrues: hourly discrete compounding

`accrue_interest` (rates.rs:33-138) charges **whole hours only**:

- `dt_hours = floor(dt_ms / 3_600_000)`; if `dt_hours == 0` with live debt, nothing
  happens and `last_update` is **untouched** — the sub-hour remainder carries into the
  next window (never discarded). With no debt at all, the clock just tracks real time.
- No accrual while `total_scaled_debt == 0`; alpha markets (≥ 2) never accrue.

$$\text{borrow\_growth} = \left(1 + \frac{r_{annual}}{8760}\right)^{dt\_hours}
\qquad \text{supply\_growth} = \left(1 + \frac{s_{annual}}{8760}\right)^{dt\_hours}$$

- `borrow_index' = borrow_index × borrow_growth` (truncating 1e18 mul).
- `exchange_rate' = exchange_rate × supply_growth`.
- Interest accrues **purely through index growth**: the scaled totals are never
  mutated by accrual; face values are re-derived from them.
- Any borrow/repay/supply/withdraw/liquidate accrues first (lazy), and anyone may call
  `accrue_market_interest()` permissionlessly.

**Full lifecycle example (TAO market, defaults).** Alice supplies 1,000 TAO (genesis →
1:1 → `1,000,000,000,000` lTokens), Bob supplies 1,000 TAO (also 1:1 while ER = 1.0;
at ER = 1.005 the same supply would mint `1,000e9 × 1e18 // 1.005e18 =
995,024,875,621` lTokens). Charlie deposits alpha and borrows 800 TAO.
`U = 800 / (800 + 2,000) = 40%` → `r = 2%/yr`, `s = 0.64%/yr`.

```text
hourly rates:  r_h = 2e16 // 8760 = 2_283_105_022_831
               s_h = 6.4e15 // 8760 = 730_593_607_305

after 1 hour:
  borrow_growth = (1e18 + 2_283_105_022_831)^1  = 1_000_002_283_105_022_831
  Charlie debt  = floor(800e9 × growth / 1e18)  = 800_001_826_484 rao   (interest 1_826_484)
  exchange_rate = 1_000_000_730_593_607_305
  Alice underlying = floor(1e12 × ER / 1e18)    = 1_000_000_730_593 rao  (earned 730_593)

after 30 days (720 h):
  borrow_growth = 1_001_645_185_575_227_616
  Charlie debt  = 801_316_148_460 rao = 801.31614846 TAO  (interest 1.31614846 TAO)
  exchange_rate = 1_000_526_165_581_675_849
  Alice underlying = 1_000_526_165_581 rao (earned 0.526165581 TAO)

after 1 year (8_760 h):
  borrow_growth = 1_020_201_316_734_517_106
  Charlie debt  = 816_161_053_387 rao (interest 16.161053387 TAO)
  effective borrow APY = (1 + 2%/8760)^8760 − 1 = 2.020132%   (quoted APR: 2.0%)
  effective supply APY = (1 + 0.64%/8760)^8760 − 1 = 0.642052% (quoted APR: 0.64%)
```

Hourly compounding is why the realized APY slightly exceeds the quoted APR.
(The `growth`/index/ER inners above are what the contract's step-truncating
`pow_fixed` produces; an exact non-truncating power series differs only in the last
few digits of the 1e18 inner and floors to the *identical* debt and redemption
amounts.)

## 3. Your debt: scaled units, index, principal

Debt is stored **scaled** (Aave model). On borrow
(`scaled = ceil(amount / borrow_index)`, ceiling — never understates debt); on every
read (`debt = floor(scaled × borrow_index / 1e18)`). The market total is derived the
same way from `total_scaled_debt`, and borrow/repay/liquidate move total and position
in **lockstep** — the last full repay drives both to exactly 0 (no ghost dust).

**Interest = debt − principal.** A `debt_principal` mapping tracks principal: `+=` on
borrow, `saturating_sub` on repay/liquidate. `get_user_debt_details(market, user)`
returns `(debt, principal)`; legacy pre-tracking positions estimate principal.

**The ceil/floor pairing, step by step** (index `1_000_316_946_018_546_683`, the
live-chain value pinned in tests):

```text
borrow 5 TUSDT:
  scaled = ceil(5e9 × 1e18 / index) = 4_998_415_773
  debt   = floor(4_998_415_773 × index / 1e18) = 5_000_000_000   (exactly; floor
           would have stored 4_999_999_999 — the bug this rounding prevents)
repay 1 rao at the same index:
  scaled_repaid = min(ceil(1 / index) = 1, 4_998_415_773) = 1
  scaled → 4_998_415_772, debt → 4_999_999_999   (fell by exactly the repaid rao)
full repay of 4_999_999_999:
  scaled_repaid = min(ceil(4_999_999_999 / index), 4_998_415_772) = 4_998_415_772
  → position 0 AND total_scaled_debt 0. Exact.
partial repay at index 1.4: debt 4 (scaled 3), repay 2 → scaled_repaid = ceil(2/1.4) = 2
  → remaining scaled 1 → debt 1.
sub-index repay (tests.rs:1826): 1 rao at index 1_000_160_150_930_897_932 credits
  exactly 1 scaled unit — every positive repayment credits at least one unit.
```

Repay clamps to `min(amount, current debt)` — overpaying (a "MAX" button) is safe; you
can never be charged more than you owe.

## 4. Supplying: lTokens and the exchange rate

Supply mints lTokens at `floor(amount / exchange_rate)` (1:1 when the market is empty);
withdrawal redeems `floor(ltokens × exchange_rate)`. `total_supplied` is stored in
scaled lToken units; its face value is `total_supplied × exchange_rate`.

**Rounding demo** (ER = 1.1): supply 100 TUSDT → mint
`100e9 × 1e18 // 1.1e18 = 90_909_090_909` lTokens; redeem those → `99_999_999_999`
rao. The 1-rao sub-precision dust stays with the pool (redeem floors).

**Exchange-rate reset**: ER is monotonic and resets to 1.0 **only when the market fully
drains** (`total_supplied == 0`). A market with supply 1,000,000,000,000 lTokens at ER
1.05: the last withdrawer redeems `1,050,000,000,000` rao, ER resets to 1e18, and the
next supply mints 1:1 again. Utilization 0 *with supply remaining* does **not** reset
ER — suppliers keep their accrued value.

**Reserve split** (market level, at the 30-day mark of the lifecycle above):
`debt_interest = 801_316_148_460 − 800_000_000_000 = 1_316_148_460` rao;
`supply_interest = floor(ΔER 526_165_581_675_849 × total_supplied_scaled 2e12 / 1e18)
= 1_052_331_163` rao; `reserve_delta = 263_817_297` rao → `reserve_accrued`
(permissionless `claim_reserve`). The naive projection (0.64% × 2,000 TAO × 30/365 =
1.052055 TAO) matches the exact result 1.052331 TAO to 0.03% — the difference is hourly
compounding and flooring.

## 5. Health factor and how much you can borrow

Collateral is alpha, valued at `effective_alpha = floor(principal × yield_index)` ×
`collateral_price = oracle × α_price_rao / 1e9`, summed over all your alpha netuids.
Debt is valued in TUSDT: TUSDT debt + `floor(TAO debt × oracle)`.

$$\text{HF} = \frac{maxLT \times \text{collateral\_value}}{\text{debt\_value}}
\qquad
\text{available} = \left\lfloor minCF \times \text{collateral\_value} \right\rfloor - \text{debt\_value}$$

`maxLT` = the **max** liquidation threshold across your alpha netuids (default 60%);
`minCF` = the **min** collateral factor across them (default 50%). Both are
**global across markets** — never per-market. HF is a Ratio; **liquidatable ⇔
HF < 1.0 strictly**. Borrow reverts `BorrowHealthExceeded` when
`borrow value > available`; `LiquidityInsufficient` when the market lacks cash; caps
(if set) → `BorrowCapExceeded`.

**Worked example.** Alice deposits 1,000 α (α-price 300,000,000 rao = 0.3 TAO/α,
oracle 250, yield index 1.0):

```text
collateral_price = 250e18 × (3e8 × 1e18 // 1e9) // 1e18 = 7.5e19        (= 75 TUSDT/α)
collateral_value = floor(1e12 × 7.5e19 / 1e18) = 75_000_000_000_000 rao = 75,000 TUSDT
available        = floor(0.5 × 75_000e9) = 37_500 TUSDT exactly
borrow 37_500 TUSDT → HF = floor(floor(0.6 × 75_000e9 / 1e18) × 1e18 / 37_500e9)
                        = 1.2e18 inner = 1.2
borrow 37_500 TUSDT + 1 rao → BorrowHealthExceeded
TAO variant: 150 TAO → value = floor(250e18 × 150e9 / 1e18) = 37_500e9 → allowed
            150 TAO + 1 rao → value 37_500_000_000_250 > available → BorrowHealthExceeded
```

**Interest erodes health.** After 30 days at U = 50% on the TUSDT market
(r = 3% × 0.5/0.8 = 1.875%/yr, index growth `1_001_542_282_337_097_954`), her debt is
`floor(37_500e9 × index / 1e18) = 37_557.835587641 TUSDT` (interest 57.84 TUSDT),
HF drops 1.2 → 1.19815, and available borrow is **0** — the CF-50% ceiling (37,500) is
below her debt (37,557.8) even though HF is still healthy. CF < LT means your borrow
power runs out *before* you become liquidatable.

## 6. Liquidation: when, how, and what everyone must do

**When.** `is_liquidatable` = HF < 1.0 (strict). HF is recomputed from live prices;
interest on **both** debt markets is accrued at the start of `liquidate` so a stale
index can't understate debt. Oracle prices are re-read and staleness-checked.

**How.** Anyone calls
`liquidate(borrower, debt_market, debt_to_cover, collateral_netuid)` — payable. The
liquidator repays debt **on behalf of the borrower** and receives alpha at a discount:

$$\text{max\_cover} = \left\lfloor \text{close\_factor} \times \text{debt\_value} \right\rfloor
\qquad \text{close\_factor default 50\%, max 50\%}$$

$$\alpha_{seized} = \left\lfloor \frac{\text{cover\_value} \times (1 + \text{bonus})}{\text{collateral\_price}} \right\rfloor
\qquad \text{principal}_{seized} = \left\lfloor \frac{\alpha_{seized}}{yield\_index} \right\rfloor
\qquad \text{bonus default 5\%, max 25\%}$$

Debt repayment: TAO market → the call must carry ≥ the covered TAO as `value` (excess
refunded); TUSDT market → `transfer_from` (pre-approve the pool). The seized alpha is
transferred to the liquidator's coldkey under the pool hotkey (`transfer_stake`, ext fn
6). Bookkeeping subtracts `min(ceil(covered/index), pos.scaled)` from position and
market total in lockstep; principal retires dollar-for-dollar.

**TUSDT-debt walkthrough.** Alice (from §5) after 30 days: oracle drops 250 → 200, so
collateral = 60,000 TUSDT, debt = 37,557.835587641 → HF = 0.95852 → liquidatable. Bob
calls `liquidate(alice, 1, 20_000e9, netuid)`:

```text
max_cover    = floor(0.5 × 37_557_835_587_641) = 18_778_917_793_820 rao  (close factor binds)
cover_tusdt  = min(20_000e9, 18_778_917_793_820) = 18_778_917_793_820
actual       = min(cover, borrower debt) = 18_778_917_793_820 rao = 18_778.917793820 TUSDT
with bonus   = floor(actual × 1.05) = 19_717_863_683_511 rao
α seized     = floor(19_717_863_683_511 / 6e19) = 328_631_061_391 rao = 328.631061391 α
post: debt   = 18_778_917_793_821 rao   α = 671.368938609
post collateral = 671_368_938_609 × 60 = 40_282.13631654 TUSDT
post HF      = 0.6 × 40_282.13631654 / 18_778.917793821 = 1.28704   (was 0.95852)
Bob paid 18_778.917793820 TUSDT, received α worth 19_717.863683460 → profit 938.94588964
TUSDT ≈ the 5% bonus, floored.
```

**TAO-debt walkthrough.** Same Alice, but she borrowed **150 TAO** and the **α-price**
(not the oracle) crashes: 300,000,000 → 200,000,000 rao (oracle stays 250).
Collateral = 250 × 0.2 × 1,000 = 50,000 TUSDT; debt = 150 TAO → 37,500 TUSDT;
HF = 0.8 → liquidatable. Bob covers 100 TAO:

```text
max_cover = floor(0.5 × 37_500e9) = 18_750e9
cover     = min(floor(250 × 100e9) = 25_000e9, 18_750e9) = 18_750e9
actual    = min(floor(18_750e9 / 250e18) = 75e9, 150e9) = 75 TAO
α seized  = floor(18_750e9 × 1.05 / 5e19) = 393_750_000_000 rao = 393.75 α
post: debt 75 TAO, α 606.25, HF = 0.6 × 606.25 × 50 / (75 × 250) = 0.97 → STILL liquidatable
Bob paid 75 TAO, received 393.75 α worth 19_687.5 TUSDT → profit 937.5 TUSDT (5% of 18_750)
```

Two takeaways: (1) a pure-TAO borrower's HF is **invariant to the oracle** — both legs
of the ratio scale with it — so α-price moves and interest are what put her underwater;
(2) the 50% close factor cannot cure a deeply underwater position in one call: HF
0.8 → 0.97 here, so a second liquidation can follow.

**What the borrower must do.** At oracle 200, debt 37,500 TUSDT (no accrual), HF = 0.96:
- **Repay** exactly `37_500 − 0.6 × 60_000 = 1,500 TUSDT` → HF = 1.0 exactly (safe:
  strict `<`).
- **Or deposit** `41.666666667 α` (41,666,666,667 rao) → collateral 62,500.00000002 →
  `0.6 × collateral ≥ debt` → safe.
- **Do nothing** → anyone liquidates.
- **Withdrawing alpha** is blocked when it would break health
  (`HealthFactorBelowThreshold`): after the TUSDT liquidation above, the most she can
  withdraw is the amount that leaves `0.6 × collateral ≥ remaining debt` — the same
  HF-1.0 boundary computed against her post-liquidation balances.

## 7. Edge cases and parameter variations

**Dust debt.** Every accrual floors to whole rao. A 2-rao debt at 100%/yr accrues
0.00023 rao/hour → interest stays 0 until the index makes `floor(2 × growth) = 3`,
which takes **3,553 hours (148 days)** at 100%/yr and **88,798 hours (10.1 years)** at
4%/yr. Practical rule: test with ≥ 1 TUSDT (1e9 rao).

**Sub-hour clock.** Borrow at t=0: accrue at 30 min → nothing (index 1e18,
`last_update` still 0); at 60 min → exactly 1 hour charged, `last_update = 3_600_000`;
at 61 min → still 1 hour (the 60 s remainder carries); at 125 min → 2 hours
(`last_update = 7_200_000`, 300 s remainder carries).

**Rate-curve parameter variations** (borrow rate at U):

| U | TAO default (0/4/96, opt 80%) | TUSDT default (0/3/97, opt 80%) | Conservative (1/3/47, opt 85%, res 15%) |
|---|---|---|---|
| 0% | 0.0% | 0.0% | 1.0% |
| 20% | 1.0% | 0.75% | 1.7059% |
| 40% | 2.0% | 1.5% | 2.4118% |
| 60% | 3.0% | 2.25% | 3.1176% |
| 80% | 4.0% | 3.0% | 3.8235% |
| 90% | 52.0% | 51.5% | 19.6667% |
| 100% | 100.0% | 100.0% | 51.0% |

**Invalid configs** (governance cannot set; `InvalidParam` / `InvalidRatio`):
- Interest: optimal 0 or > 100%; `slope1 + slope2 > 100%` (e.g. 2% + 198%); reserve ≥
  100%; `base + slope1 + slope2 > 100%`.
- Alpha market: `CF ≥ LT` (e.g. 50/50), CF 0, LT > 100%, bonus > 25%.
- Global: close factor 0 or > 50%, performance fee > 50%, oracle age 0.

**Valid alternative alpha params** CF 70% / LT 80% / bonus 10% on the §5 position
(75,000 TUSDT collateral, 37,500 debt): available borrow rises 0 → **15,000 TUSDT**,
HF rises 1.2 → **1.6**.

**Caps and liquidity** (all strict `>` checks): borrow cap 1,000 TUSDT with existing
debt 999,999,999,999 rao → borrowing 2 rao reverts `BorrowCapExceeded`, 1 rao lands
exactly on the cap and passes. Supply cap 5,000 TAO with existing face supply
4,999,999,999,999 → 2 rao `SupplyCapExceeded`, 1 rao ok. Cash 100 rao, borrow 101 →
`LiquidityInsufficient`.

**Boundary reverts**: HF exactly 1.0 → `NotLiquidatable`; `debt_to_cover = 0` →
`ZeroAmount`; `debt_market = 2` → `InvalidDebtMarket`; seizure exceeding the position
(10 α position, huge debt → 328.125 α needed) → `CollateralAwardExceedsPosition`;
repay 0 → `ZeroAmount`; repay 10,000 against a 100-rao debt → clamps to 100, clears the
position, no error. (Declared but never returned: `CloseFactorExceeded` and
`InvalidCollateralNetuid` — the close factor clamps instead of erroring, so those enum
variants are reserved.)

## 8. Quick reference (defaults)

| Formula | Value |
|---|---|
| Utilization | `U = total_debt / (total_debt + cash)` |
| Borrow rate | `base + slope1·U/U_opt`; above: `base + slope1 + slope2·(U−U_opt)/(1−U_opt)` |
| Supply rate | `r × U × (1 − reserve)` |
| Accrual | hourly: `(1 + r/8760)^dt_hours`, whole hours only, remainder carried |
| Debt | `floor(scaled × borrow_index / 1e18)`; scaled = `ceil(amount / index)` on borrow |
| Interest | `debt − principal` (principal tracked separately) |
| lTokens | mint `floor(amount / ER)`, redeem `floor(ltokens × ER)`; ER resets only on full drain |
| Health factor | `maxLT × collateral_value / debt_value` — global, liquidatable ⇔ HF < 1.0 |
| Borrow capacity | `floor(minCF × collateral) − debt` |
| Liquidation | cover ≤ `floor(50% × debt)`; seize `floor(cover × 1.05 / price)`; 5% bonus |
| Params | TAO 0/4%/96%, TUSDT 0/3%/97%, optimal 80%, reserve 20%; CF 50%, LT 60%, bonus 5%, close 50% |

Errors for these flows: `BorrowHealthExceeded`, `LiquidityInsufficient`,
`MintBelowPrecision`, `SupplyCapExceeded`, `BorrowCapExceeded`, `NotLiquidatable`,
`HealthFactorBelowThreshold`, `CollateralAwardExceedsPosition`, `ZeroAmount`,
`InvalidDebtMarket`, `TokenTransferFromFailed`, `ArithmeticError`,
`OraclePriceStale`, `OraclePriceUnavailable` — see
[`../errors/lending-pool.md`](../errors/lending-pool.md).
