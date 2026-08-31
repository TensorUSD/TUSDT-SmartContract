# tusdt-lending-pool — Calculation Guide

Step-by-step numeric walkthroughs for the lending pool: how APY is calculated, how
interest accrues, how debt grows, and how/when liquidation works — all worked with
the exact integer math the contract uses. Companion to the reference page
[`../contracts/lending-pool.md`](../contracts/lending-pool.md) and the
[error catalog](../errors/lending-pool.md).

> **Verification**: every number here was recomputed with an independent Python
> harness reimplementing the contract's fixed-point ops exactly (truncating 1e18
> `Ratio` mul/div, square-and-multiply `pow_fixed`, ceiling scaled-debt conversion),
> reproducing the pins in `tests.rs`: the rate-curve points, the live-chain borrow
> index `1_000_316_946_018_546_683`, the mint/redeem tests, the liquidation
> bookkeeping test (`index 1.4, scaled 3 → debt 4`), and the full-seizure split pins
> (`C=125/D=100 → 5% share`, `C=105/D=100 → 4.7619% share`, `C=80/D=100 → deficit 20`).
>
> The health-factor functions (`get_health_factor`, `get_available_borrow_tusdt`,
> `is_liquidatable`, value getters) are `pub` but **not** `#[ink(message)]` — not
> queryable on-chain; UIs compute them client-side (`tusdt-app/src/lib/lendingHealth.ts`).

## Numbers and scales (read this first)

| Quantity | Scale | Example |
|---|---|---|
| Balance / token amount | `u64`, 9 decimals | 1 TAO = 1 TUSDT = 1 α = 1e9 rao |
| Internal `Ratio` (fixed-point) | 1e18 inner | 1.5 = 1.5 × 10¹⁸ |
| Parameter configs (external messages) | basis points | 4,000 bps = 40% |
| Oracle price (TUSDT/TAO) | 1e18 Ratio | 230 → inner 230 × 10¹⁸ |
| Chain alpha price (ext fn 15) | rao, 1e9 per α | 3,000,000 rao = 0.003 TAO/α |
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

Utilization is computed against the market's total liquidity (`rates.rs:57-66`):

```text
U = total_debt / (total_debt + cash)
```

`cash` for market 0 (TAO) = the pool's native balance + TAO staked on the root
subnet (1:1); for market 1 (TUSDT) = the pool's TUSDT balance.

The **annual borrow rate** (`compute_borrow_rate`, rates.rs:144-168) is piecewise,
with `U_opt` (optimal utilization) defaulting to 80%; the **annual supply rate** is
borrowers' interest after the reserve slice:

```text
U ≤ 80%:  r = base + slope1 × U / 80%          s = r × U × (1 − reserve_factor)
U > 80%:  r = base + slope1 + slope2 × (U − 80%) / 20%   (reserve default 20%)
```

Defaults: **TAO** base 0 / slope1 4% / slope2 96% / optimal 80% / reserve 20%;
**TUSDT** base 0 / slope1 3% / slope2 97% / optimal 80% / reserve 20%.

**Worked curve points** (exact, 1e18 fixed-point inners — `bps = value × 1e14`,
`U` inner = % × 1e16):

```text
U = 40% (TAO):    fraction = 40/80 = 0.5 → r = 4% × 0.5 = 2.0%/yr,   s = 0.64%/yr
U = 80% (TAO):    r = 4.0%/yr, s = 4% × 0.8 × 0.8 = 2.56%/yr
U = 90% (TAO):    excess = 10/20 = 0.5 → r = 4% + 96% × 0.5 = 52.0%/yr, s = 37.44%/yr
U = 100% (TAO):   r = 4% + 96% = 100.0%/yr, s = 100% × 1 × 0.8 = 80.0%/yr
U = 40% (TUSDT):  r = 3% × 0.5 = 1.5%/yr, s = 1.5% × 0.4 × 0.8 = 0.48%/yr
```

| U | TAO borrow | TAO supply | TUSDT borrow | TUSDT supply |
|---|---|---|---|---|
| 0% | 0.0% | 0.0% | 0.0% | 0.0% |
| 40% | 2.0% | 0.64% | 1.5% | 0.48% |
| 80% | 4.0% | 2.56% | 3.0% | 1.92% |
| 90% | 52.0% | 37.44% | 51.5% | 37.08% |
| 100% | 100.0% | 80.0% | 100.0% | 80.0% |

`get_borrow_rate` / `get_supply_rate` return these **annual** 1e18 ratios (client APY display = `inner / 1e18 × 100`); the ÷8,760 happens inside the contract.

## 2. How interest accrues: hourly compounding, first hour prepaid

`accrue_interest` (rates.rs:33-138) charges **whole hours only**:

- `dt_hours = floor(dt_ms / 3_600_000)`; if `dt_hours == 0` with live debt, nothing
  happens and `last_update` is **untouched** — the sub-hour remainder carries into
  the next window (never discarded). With no debt, the clock just tracks real time.
- No accrual while `total_scaled_debt == 0`; alpha markets (≥ 2) never accrue.

Growth over `dt_hours` whole hours:

```text
borrow_growth = (1 + r_annual / 8760) ^ dt_hours    supply_growth = (1 + s_annual / 8760) ^ dt_hours
```

- `borrow_index' = borrow_index × borrow_growth`; `exchange_rate' = exchange_rate ×
  supply_growth` (truncating 1e18 muls). Interest accrues **purely through index
  growth** — scaled totals are never mutated; face values are re-derived.
- Any borrow/repay/supply/withdraw/liquidate accrues first (lazy); anyone may call
  `accrue_market_interest()` permissionlessly.

**The first hour is prepaid at borrow time** (`charge_prepaid_hour`, rates.rs).
Because the borrow index is global, an accrual cannot single out a fresh position,
so each borrow prices one hour of interest into its scaled debt immediately:

```text
premium     = floor((r_annual / 8760) × amount)      # at post-borrow utilization
debt_booked = amount + premium
scaled      = ceil(debt_booked / borrow_index)        # unchanged ceiling
```

`debt_principal` records only `amount`, so `interest = debt − principal` reads
positive from the very first block. Repaying inside that hour does not refund it.
The premium is split like index interest: `reserve_factor` to `reserve_accrued`,
the rest to suppliers via `exchange_rate` growth.

**Full lifecycle example (TAO market, defaults).** Alice supplies 1,000 TAO, Bob
200 TAO (genesis 1:1 → 1,000 + 200 lTokens; pool cash = 1,200 TAO). Charlie
deposits alpha and borrows 800 TAO:

```text
U = 800 / (800 + 1,200) = 40%  →  borrow rate 2%/yr, supply rate 0.64%/yr
hourly rates:  r_h = 2% / 8760 = 2_283_105_022_831 inner (1e18 fixed)
               s_h = 0.64% / 8760 = 730_593_607_305 inner

at borrow (t=0):   premium = floor(800e9 × r_h / 1e18) = 1_826_484 rao = 0.001826484 TAO
                   debt    = 800.001826484 TAO immediately (interest 0.001826484 from block 0)
after 1 hour:      debt    = 800.001826484 × (1 + 0.02/8760) = 800.003652972 TAO
                   (the prepaid hour plus the first accrual hour)
after 30 days (720 h of accrual):  debt = 801.317977949 TAO (interest 1.317977949)
                   exchange rate = 1.000526165581675953; Alice's 1,000 lTokens
                   are worth 1,000.526165581 TAO → repaying 801.317977949 TAO
                   clears Charlie's position exactly
after 1 year (8,760 h):  debt = 816.162916768 TAO (interest 16.162916768)
                   Alice's 1,000 lTokens redeem = 1,006.420521407 TAO
                   effective APY: borrow (1 + 0.02/8760)^8760 − 1 = 2.0201% (quoted 2.0%); supply (1 + 0.0064/8760)^8760 − 1 = 0.6421% (quoted 0.64%)
```

Compare the 30-day figure with the pre-prepay model: `800 × (1 + 0.02/8760)^720 =
801.316148460` — the prepaid hour adds exactly one hour's growth on top, so every
"elapsed since borrow" debt is the old model's value at elapsed + 1 hour. The
premium's 20% reserve slice is booked into `reserve_accrued` at borrow time (the
supplier 80% grows the exchange rate), which is why Alice's redemption value above
matches the lifecycle's supply side unchanged.

Hourly compounding is why realized APY slightly exceeds the quoted APR (the
inners above are what the contract's step-truncating `pow_fixed` produces; a
non-truncating series differs only in the last few digits and floors to the
*identical* rao amounts).

## 3. Your debt: scaled units, index, principal

Debt is stored **scaled** (Aave model). On borrow
(`scaled = ceil(amount / borrow_index)` — ceiling, never understates debt); on every
read (`debt = floor(scaled × borrow_index / 1e18)`). The market total is derived the
same way from `total_scaled_debt`, and borrow/repay/liquidate move total and position
in **lockstep** — the last full repay drives both to exactly 0 (no ghost dust).

**Interest = debt − principal.** A `debt_principal` mapping tracks principal: `+=` on
borrow, `saturating_sub` on repay/liquidate. `get_user_debt_details(market, user)`
returns `(debt, principal)`; legacy pre-tracking positions estimate principal.

**The ceil/floor pairing, step by step** (index `1_000_316_946_018_546_683`, the
live-chain value pinned in tests):

```text
borrow 5 TUSDT:  scaled = ceil(5e9 / 1.000316946018546683) = 4_998_415_773
                 debt   = floor(4_998_415_773 × index / 1e18) = 5_000_000_000
                 (exactly; plain floor would store 4_999_999_999 — the bug this prevents)
repay 1 rao:     scaled_repaid = min(ceil(1 / index) = 1, 4_998_415_773) = 1
                 scaled → 4_998_415_772, debt → 4_999_999_999 (fell by exactly 1 rao)
full repay 4_999_999_999: scaled_repaid = min(ceil(4_999_999_999 / index),
                 4_998_415_772) = 4_998_415_772 → position 0 AND total 0. Exact.
partial at index 1.4: debt 4 (scaled 3), repay 2 → scaled_repaid = ceil(2/1.4) = 2 → remaining scaled 1 → debt 1
sub-index (tests.rs:1826): 1 rao at index 1_000_160_150_930_897_932 credits exactly 1 scaled unit — every positive repayment credits at least one unit.
```

Repay clamps to `min(amount, current debt)` — overpaying (a "MAX" button) is safe; you can never be charged more than you owe.

## 4. Supplying: lTokens and the exchange rate

Supply mints lTokens at `floor(amount / exchange_rate)` (1:1 when the market is
empty); withdrawal redeems `floor(ltokens × exchange_rate)`. `total_supplied` is
stored in scaled lToken units; its face value is `total_supplied × exchange_rate`.

**Rounding demo** (exchange rate = 1.1):

```text
supply 100 TUSDT → mint = floor(100e9 / 1.1) = 90_909_090_909 lTokens
redeem those     → 90_909_090_909 × 1.1 = 99_999_999_999 rao
The 1-rao sub-precision dust stays with the pool (redeem floors).
```

**Exchange-rate reset**: ER is monotonic and resets to 1.0 **only when the market
fully drains** (`total_supplied == 0`). A market with 1,000 lTokens at ER 1.05: the
last withdrawer redeems 1,050 TAO, ER resets to 1.0, next supply mints 1:1 again.
Utilization 0 *with supply remaining* does **not** reset ER — suppliers keep their accrued value.

**Reserve split** (market level, at the 30-day mark of the lifecycle above):

```text
debt_interest   = 801.316148460 − 800 = 1.316148460 TAO
supply_interest = ΔER 0.000526165581675849 × 1,200 = 0.631398698 TAO
reserve_delta   = 1.316148460 − 0.631398698 = 0.684749762 TAO (→ reserve_accrued,
                 permissionless claim_reserve)
Naive 0.64% × 1,200 × 30/365 = 0.631233 TAO matches exact 0.631399 TAO to 0.03% —
the difference is hourly compounding and flooring.
```

## 5. Health factor and how much you can borrow

Collateral is alpha, valued at `effective_alpha = principal` (no yield index) ×
`collateral_price = oracle × α_price_rao / 1e9`, summed over all your alpha netuids.
Debt is valued in TUSDT: TUSDT debt + `floor(TAO debt × oracle)`.

```text
HF        = maxLT × collateral_value / debt_value
available = floor(minCF × collateral_value) − debt_value
```

`maxLT` = the **max** liquidation threshold across your alpha netuids (default 60%);
`minCF` = the **min** collateral factor (default 50%). Both are **global across
markets** — never per-market. HF is a Ratio; **liquidatable ⇔ HF < 1.0 strictly**.
Borrow reverts `BorrowHealthExceeded` when `borrow value > available`;
`LiquidityInsufficient` when the market lacks cash; caps (if set) → `BorrowCapExceeded`.

**Worked example.** Alice deposits 1,000 α (α-price 3,000,000 rao = 0.003 TAO/α, oracle 230):

```text
collateral_value = 230 × 0.003 × 1,000 = 690 TUSDT
borrow capacity  = floor(0.5 × 690) = 345 TUSDT
borrow 345 TUSDT → HF = 0.6 × 690 / 345 = 1.2
borrow 345 TUSDT + 1 rao → BorrowHealthExceeded
TAO variant: 1.5 TAO → value 345 exactly → allowed; 1.5 TAO + 1 rao → BorrowHealthExceeded
```

**Interest erodes health.** If the TUSDT market runs at U = 50%
(r = 3% × 0.5/0.8 = 1.875%/yr), Alice's borrow prices the first hour up front:
premium = `floor(345e9 × (1.875%/8760))` = 738,441 rao, so her debt starts at
345.000738441 TUSDT. After 30 days (720 h of accrual) it is
`345.000738441 × (1 + 0.01875/8760)^720 = 345.532827 TUSDT`; HF drops 1.2 → 1.1981
and available borrow is **0** — the CF-50% ceiling (345) is below her debt (345.53)
even though HF is healthy: CF < LT means borrow power runs out *before* liquidation.

## 6. Liquidation: when, how, and what everyone must do

**When.** `is_liquidatable` = HF < 1.0 (strict), recomputed from live prices.
Interest on **both** debt markets is accrued at the start of `liquidate` so a stale
index can't understate the debt the liquidator pays; oracle prices are re-read and
staleness-checked.

**How.** Anyone calls `liquidate(borrower)` — payable, **one call**. The liquidator
pays the borrower's **entire debt on both markets** (TAO + TUSDT, with accrued
interest) and receives the borrower's **entire alpha collateral across every
approved netuid**, minus the platform's per-netuid `liquidation_fee` (default 5%).
The split is decided by the position totals — collateral value `C` (all netuids,
current prices) and debt value `D` (TAO debt + TUSDT debt at the oracle price):

```text
C > D:   payment = D;  platform_share = min(fee, (C − D) / C)   per netuid
C ≤ D:   payment = C;  platform_share = 0;  deficit = D − C     market deficit
```

The `min` cap at the surplus share is what guarantees the liquidator never loses
principal: the platform's cut can never exceed the surplus, so the liquidator
always keeps collateral worth at least `D` — break-even at worst. (The rounding
remainder of the share also stays with the liquidator.)

Debt repayment: TAO market → the call must carry ≥ the covered TAO as `value`
(excess refunded); TUSDT market → `transfer_from` (pre-approve the pool). The
seized alpha is transferred to the liquidator's coldkey under the pool hotkey
(`transfer_stake`, ext fn 6) — the platform's share first, then the liquidator's,
per netuid. Bookkeeping subtracts the same scaled units from position and market
total in lockstep; principal retires dollar-for-dollar; every alpha position is
cleared. `Liquidated` reports `{user, liquidator, collateral_netuids,
collateral_seized, platform_alpha, debt_covered_tao, debt_covered_tusdt, deficit}`.

**Boundary: where liquidation begins.** With 690 TUSDT collateral and 345 TUSDT
debt, HF ≥ 1 requires `0.6 × (oracle × 0.003 × 1,000) ≥ 345`, i.e. oracle ≥
345/1.8 = 191.67:

```text
oracle 192: collateral = 576 → 0.6 × 576 = 345.6 ≥ 345 → HF 1.0017 → safe
oracle 191: collateral = 573 → 0.6 × 573 = 343.8 < 345 → HF 0.9965 → LIQUIDATABLE (any oracle ≤ 191)
```

**Worked examples — the split.** The share depends only on `C`, `D`, and the
netuid's fee (`full_seizure_split`, risk.rs; values in 9-decimal TUSDT units, share
as a 1e18 ratio). These are the exact pins from `tests.rs`:

```text
healthy, fee uncapped:  C = 125, D = 100, fee 5%
  surplus share = (125 − 100) / 125 = 20% ≥ 5% → platform_share = 5%
  payment = 100;  platform cut = floor(5% × 125) = 6.25
  liquidator receives 125 − 6.25 = 118.75 of collateral for 100 paid
  → profit 18.75, i.e. +18.75% of the debt

thin surplus, cap binds:  C = 105, D = 100, fee 5%
  surplus share = floor(5 × 1e18 / 105) = 47_619_047_619_047_619 inner ≈ 4.7619%
  < 5% → platform_share capped at 4.7619%
  platform cut = floor(4.7619% × 105) = 4.999999999 → liquidator keeps 100.000000001
  → break-even plus 1 rao of rounding (the cap protects the liquidator's principal)

exact cover:  C = D = 100, fee 5%
  surplus = 0 → platform_share = 0;  payment = 100;  no deficit
  → liquidator break-even

underwater:  C = 80, D = 100, fee 5%
  payment = 80 (the collateral's worth);  platform_share = 0;  no surplus to tax
  deficit = 100 − 80 = 20 → frozen as a market deficit (`DeficitReported`)
```

> **Exact rao note:** the share cap is computed in 1e18 inner — `floor(5e9 × 1e18 /
> 105e9) = 47_619_047_619_047_619` — and the platform cut floors to whole rao
> (`4_999_999_999` in the thin-surplus example), so the liquidator's received value
> never falls below `D`.

**Full-seizure walkthrough.** Back in the §5 position: 1,000 α, α-price
3,000,000 rao (0.003 TAO/α), oracle 230 → 190, debt 345 TUSDT:

```text
oracle 190 → collateral C = 570 TUSDT;  debt D = 345 TUSDT;  HF = 0.9913 < 1
surplus = 225;  surplus share = 225 / 570 = 39.47% > fee 5% → share = 5%
payment = 345 TUSDT (the full debt, TUSDT market → transfer_from)
platform cut = 5% × 1,000 α = 50 α (worth 50 × 0.57 = 28.50 TUSDT)
liquidator receives 950 α worth 541.50 TUSDT for 345 paid → profit 196.50 (+56.96%)
post: borrower's debt 0 and alpha 0 — the ENTIRE position is closed in ONE call
```

The borrower's TUSDT debt is retired in full; if she also had TAO debt it would be
covered in the same call (payment = the full TAO debt as `value`), and every netuid
with collateral is seized — there is no partial-close choice for the liquidator to
make.

**Bad-debt write-off.** The deficit path is the underwater branch above: when the
collateral cannot cover the debt (`C ≤ D`), the liquidator pays only `C` and the
residual `D − C` is written off — removed from the ledger (utilization and rates
are not poisoned) and frozen as a market deficit that never compounds
(`DeficitReported`; e.g. C = 80, D = 100 → deficit 20). The maintainer funds it
from `reserve_accrued` via `cover_deficit`; a deficit larger than the reserve stays
on the books as an unfunded shortfall (`get_market_deficit`).

**What the borrower must do.** At oracle 190, debt 345 TUSDT (no accrual), HF = 0.9913:

- **Repay** exactly `345 − 0.6 × 570 = 345 − 342 = 3 TUSDT` → HF = 1.0 exactly (safe: strict `<`).
- **Or deposit alpha**: need `0.6 × collateral ≥ 345` → collateral ≥ 575 → α ≥ 575 / 0.57 = 1,008.77 → deposit ≈ 8.77 α.
- **Do nothing** → anyone liquidates the whole position.
- **Withdrawing alpha** is blocked when it would break health (`HealthFactorBelowThreshold`): the most she can withdraw leaves `0.6 × collateral ≥ remaining debt` — the same HF-1.0 boundary on her post-liquidation balances.

## 7. Edge cases and parameter variations

**Dust debt.** Every accrual floors to whole rao: a 2-rao debt at 100%/yr accrues
0.00023 rao/hour, so interest stays 0 until `floor(2 × growth) = 3` — ≈ 3,552
hours (148 days) at 100%/yr, ≈ 88,797 h (10.1 yr) at 4%/yr. Test with ≥ 1 TUSDT.

**Sub-hour clock.** A borrow at t=0 charges the first hour immediately (prepaid):
accrue at 30 min → nothing new (`dt_hours == 0`, `last_update` untouched); at 60 min
→ exactly 1 hour charged (`last_update = 3_600_000` — the second hour); at 61 min →
still 1 hour (the 60 s remainder carries); at 125 min → 2 hours (`last_update =
7_200_000`). Every "elapsed since borrow" debt equals the old model's debt at
elapsed + 1 hour.

**Rate-curve parameter variations** (borrow rate at U):

| U | TAO default (0/4/96, opt 80%) | TUSDT default (0/3/97, opt 80%) | Conservative (1/3/47, opt 85%) |
|---|---|---|---|
| 0% | 0.0% | 0.0% | 1.0% |
| 40% | 2.0% | 1.5% | 2.4118% |
| 80% | 4.0% | 3.0% | 3.8235% |
| 90% | 52.0% | 51.5% | 19.6667% |
| 100% | 100.0% | 100.0% | 51.0% |

Conservative curve step by step (base 1%, slope1 3%, slope2 47%, optimal 85%):

```text
U = 80%:  1% + 3% × 80/85 = 3.8235%      U = 90%:  1% + 3% + 47% × 5/15 = 19.6667%
```

**Invalid configs** (governance cannot set; `InvalidParam` / `InvalidRatio`):
- Interest: optimal 0 or > 100%; `slope1 + slope2 > 100%` (e.g. 2% + 198% — a
  "steep" 200% curve is rejected); reserve ≥ 100%; `base + slope1 + slope2 > 100%`.
- Alpha market: `CF ≥ LT` (e.g. 50/50), CF 0, LT > 100%, fee > 100%, or
  `fee + LT > 100%` (e.g. 5% fee + 96% LT — the fee must fit inside the
  liquidation room so the platform's cut can never exceed the surplus).
- Global: oracle age 0.

**Valid alternative alpha params** CF 70% / LT 80% on the §5 position (690
collateral, 345 debt): available borrow rises 0 → **138 TUSDT** (`0.7 × 690 − 345`), HF rises 1.2 → **1.6** (`0.8 × 690 / 345`).

**Caps and liquidity** (all strict `>` checks): borrow cap 100 TUSDT with existing
debt 99.999999999 → 2 rao reverts `BorrowCapExceeded`, 1 rao lands exactly on the
cap. Supply cap 500 TAO with 499.999999999 face supply → 2 rao `SupplyCapExceeded`,
1 rao ok. Cash 100 rao, borrow 101 → `LiquidityInsufficient`.

**Boundary reverts**: HF exactly 1.0 → `NotLiquidatable`; no alpha collateral or no
debt → `ZeroAmount`; repay 10,000 against a 100-rao debt → clamps to 100, clears, no
error. The legacy `CloseFactorExceeded`, `InvalidDebtMarket`, and
`CollateralAwardExceedsPosition` variants are retained for ABI stability but no
current code path returns them — the full-seizure model has no close factor, takes
no `debt_market` argument, and seizes exactly the collateral that exists.

## 8. Quick reference (defaults)

| Formula | Value |
|---|---|
| Utilization | `U = total_debt / (total_debt + cash)` |
| Borrow rate | `base + slope1 × U/80%`; above: `base + slope1 + slope2 × (U − 80%)/20%` |
| Supply rate | `r × U × (1 − reserve)` |
| Accrual | hourly `(1 + r/8760) ^ dt_hours`, whole hours, remainder carried; **first hour prepaid at borrow** |
| Debt / interest | `floor(scaled × borrow_index / 1e18)`, scaled = `ceil(amount + premium / index)`; interest = `debt − principal` |
| lTokens | mint `floor(amount / ER)`, redeem `floor(ltokens × ER)`; ER resets only on full drain |
| Health factor | `maxLT × collateral_value / debt_value` — global, liquidatable ⇔ HF < 1.0 |
| Borrow capacity | `floor(minCF × collateral) − debt` |
| Liquidation | `liquidate(borrower)` pays the full debt on both markets; platform share per netuid = `min(fee, (C−D)/C)` (default fee 5%); underwater → pay `C`, deficit `D−C` → frozen market deficit (`cover_deficit` funds from reserve) |
| Params | TAO 0/4%/96%, TUSDT 0/3%/97%, optimal 80%, reserve 20%; CF 50%, LT 60%, liquidation fee 5% |

Errors for these flows: `BorrowHealthExceeded`, `LiquidityInsufficient`,
`MintBelowPrecision`, `SupplyCapExceeded`, `BorrowCapExceeded`, `NotLiquidatable`,
`HealthFactorBelowThreshold`, `ZeroAmount`,
`TokenTransferFromFailed`, `ArithmeticError`, `OraclePriceStale`, `OraclePriceUnavailable` — see
[`../errors/lending-pool.md`](../errors/lending-pool.md).
