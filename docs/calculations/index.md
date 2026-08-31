# TUSDT Contracts — Calculation Guides

Step-by-step numeric walkthroughs of the protocol's money math, worked with the exact
integer (fixed-point) arithmetic the contracts use — no floats anywhere on-chain. These
pages are companions to the user guides in [`../contracts/`](../contracts/index.md);
every error name links into the catalog in [`../errors/`](../errors/index.md).

> **How these numbers are trustworthy**: each page's numbers were recomputed with an
> independent Python harness that reimplements the contract's `Ratio` (1e18) operations
> exactly — truncating mul/div, `value/self` operand order, ceiling scaled-debt
> conversion, and the square-and-multiply `pow_fixed` — and reproduces **every numeric
> pin in the contract test suites** (`tests.rs`) before being published.

| Guide | What it walks through |
|---|---|
| [vault-alpha.md](vault-alpha.md) | CDP vault: alpha pricing, max borrow, LTV, the strict liquidation boundary, the full auction (min bid → settle fee split), parameter sensitivity, fee/boundary cases. |
| [lending-pool.md](lending-pool.md) | Lending pool: the rate curve and APY derivation, hourly interest accrual with a prepaid first hour and a full lifecycle example, scaled debt and the ceil/floor pairing, lTokens and the exchange rate, health factor and borrow capacity, liquidation (full-seizure split, platform fee, underwater deficit write-off), and edge cases. |

## Where each question is answered

**Vault (`tusdt-vault-alpha`)**

| Question | Section |
|---|---|
| How much can I borrow? | [§2 How much can I borrow?](vault-alpha.md#2-how-much-can-i-borrow) |
| When does liquidation happen? | [§3 When does liquidation happen?](vault-alpha.md#3-when-does-liquidation-happen) |
| What happens to my collateral? | [§4 The liquidation auction, step by step](vault-alpha.md#4-the-liquidation-auction-step-by-step) |
| What if risk params differ? | [§5 Parameter sensitivity](vault-alpha.md#5-parameter-sensitivity-same-position-different-risk-settings) |

**Lending pool (`tusdt-lending-pool`)**

| Question | Section |
|---|---|
| How is APY calculated? | [§1 The interest-rate curve](lending-pool.md#1-the-interest-rate-curve-how-apy-is-calculated) |
| How does interest accrue? | [§2 Hourly compounding, first hour prepaid](lending-pool.md#2-how-interest-accrues-hourly-compounding-first-hour-prepaid) |
| How is my debt tracked? | [§3 Scaled units, index, principal](lending-pool.md#3-your-debt-scaled-units-index-principal) |
| How do lTokens earn? | [§4 Supplying: lTokens and the exchange rate](lending-pool.md#4-supplying-ltokens-and-the-exchange-rate) |
| When am I liquidated, and what must I do? | [§5 Health factor](lending-pool.md#5-health-factor-and-how-much-you-can-borrow) + [§6 Liquidation](lending-pool.md#6-liquidation-when-how-and-what-everyone-must-do) |
| Edge cases and different params | [§7 Edge cases and parameter variations](lending-pool.md#7-edge-cases-and-parameter-variations) |

## Conventions used throughout

- **Scales**: balances are `u64` with 9 decimals (1 TAO = 1 TUSDT = 1 α = 1e9 rao);
  internal ratios are 1e18 fixed-point; external config params are basis points
  (15,000 bps = 150%); the oracle TUSDT/TAO price is a 1e18 ratio; the chain alpha
  price is rao-scaled (1e9 per α).
- **Rounding**: every fixed-point step floors for positive values unless stated
  otherwise; the one ceiling is the borrow/repay/liquidate scaled-debt conversion
  (Aave `rayDivUp`), which exists so debt is never understated by dust.
- **Strict inequalities matter**: a vault is liquidatable only when debt *strictly*
  exceeds the liquidation limit, and a pool borrower only when the health factor is
  *strictly* below 1.0 — exactly at the boundary is safe in both systems.
- **No floats**: `//` is integer division (floor), `%` formatting is for display only.
