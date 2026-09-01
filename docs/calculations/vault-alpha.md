# tusdt-vault-alpha — Calculation Guide

Step-by-step numeric walkthroughs for the Alpha Vault (CDP engine): how much you can
borrow, when liquidation happens, and what the auction looks like — worked out in plain
human-scale numbers. Companion to the reference page
[`../contracts/vault-alpha.md`](../contracts/vault-alpha.md) and the
[error catalog](../errors/vault-alpha.md).

> **Verification**: every number on this page was recomputed with an independent Python
> script and matches the numeric pins in the contract test suite (`tests.rs`) —
> including `max_borrow_allowed_spec_example`, `is_liquidatable_boundary`, and
> `liquidation_min_bid_includes_fee`.

## Numbers and scales (read this first)

| Quantity | Scale | Example |
|---|---|---|
| Balance / token amount | `u64`, 9 decimals | 1 TUSDT = 1,000,000,000 rao |
| Internal `Ratio` (fixed-point) | 1e18 inner | 1.5 = 1,500,000,000,000,000,000 |
| Parameter configs (external messages) | basis points | 15,000 bps = 150% |
| Oracle price (TUSDT/TAO) | 1e18 `Ratio` | 230 TUSDT/TAO |
| Chain alpha price (`get_alpha_price`, ext fn 15) | rao, 1e9 per alpha | 3,000,000 rao = 0.003 TAO/alpha |

The contract works entirely in integer rao and floors at every step
(`checked_mul_value` = `floor(value × ratio / 1e18)`, `checked_div_value` =
`floor(value / ratio)`). **The vault has no interest**: debt = borrowed, repay 1:1
(`repay_token` rejects `amount > debt` with `RepayAmountTooHigh`).

## 1. Pricing: how much is my alpha worth?

Two price sources are multiplied (`current_collateral_price`, lib.rs:1693):

```text
price per alpha (TUSDT) = oracle (TUSDT/TAO) × alpha price (TAO/alpha)
collateral value (TUSDT) = price per alpha × alpha amount
```

**Worked example (used throughout this guide).** Alice deposits 1,000 alpha on netuid 1.
The oracle reports 230 TUSDT/TAO and the chain alpha price is 0.003 TAO/alpha.

```text
Step 1 — price per alpha:
  230 × 0.003 = 0.69 TUSDT per alpha

Step 2 — collateral value:
  0.69 × 1000 = 690 TUSDT
```

> **Exact on-chain math**: in rao this is 690 TUSDT = 690,000,000,000 rao, computed as
> `floor(0.69 × 1e18) × 1,000,000,000,000 / 1e18` — the same 690 TUSDT, just floored
> through the fixed-point pipeline.

## 2. How much can I borrow?

`max_borrow_allowed` (risk.rs:14) divides the collateral value by the collateral ratio
(`checked_div_value` = `value / self`):

```text
max borrow (TUSDT) = collateral value ÷ collateral ratio
```

With the default collateral ratio of 150% (1.5):

```text
max borrow = 690 ÷ 1.5 = 460 TUSDT
```

Borrowing is capped on **every** `borrow_token` call: if `borrowed + amount > max_borrow`
the call reverts with `CollateralRatioExceeded`. Releasing collateral applies the same
cap against the projected post-release balance.

Alice borrows 300 TUSDT (about two-thirds of her cap):

```text
LTV      = 300 ÷ 690 ≈ 43.5%
headroom = 460 − 300 = 160 TUSDT
```

**Reading your vault** (all derived, not stored — compute client-side):

| Quantity | Formula | Alice's values |
|---|---|---|
| Collateral value | oracle × alpha price × amount | 230 × 0.003 × 1000 = 690 TUSDT |
| Debt | borrowed | 300 TUSDT |
| LTV | debt ÷ collateral value | 43.5% |
| Borrow headroom | max borrow − debt | 160 TUSDT |
| Liquidation limit | see below | 575 TUSDT |

> **Exact on-chain math**: 690 ÷ 1.5 = 460 is exact even in rao. A floored case
> (test-pinned, `max_borrow_allowed_default_collateral_ratio`, tests.rs:457): with
> price 1.0 and 1,000 rao of collateral, `max_borrow = 1000 / 1.5 = 666.67 → 666` —
> the 0.67 rao is discarded. Same for `100 / 3 = 33.33 → 33`
> (`max_borrow_allowed_rounds_down`, tests.rs:483).

## 3. When does liquidation happen?

`liquidation_limit` (risk.rs:30) and `is_liquidatable` (risk.rs:46):

```text
liquidatable iff borrowed > collateral value ÷ liquidation ratio
```

The comparison is **strict** (`>`): sitting *exactly at* the limit is safe. With the
default liquidation ratio of 120% (1.2):

```text
liquidation limit = 690 ÷ 1.2 = 575 TUSDT
```

With 300 TUSDT of debt Alice has plenty of room (300 < 575). She becomes liquidatable
once her collateral value falls below `300 × 1.2 = 360` TUSDT:

```text
collateral < 360
  → alpha price < 360 ÷ 1000 = 0.36 TUSDT/alpha
  → alpha price < 0.36 ÷ 230 ≈ 0.001565 TAO/alpha
  → oracle < 120 TUSDT/TAO   (collateral = O × 0.003 × 1000 = 3O;
                              liquidatable when 3O ÷ 1.2 < 300 → O < 120)
```

**The strict boundary, with numbers.** At exactly the boundary she is NOT liquidatable;
one step below it she is:

```text
oracle 120: collateral = 120 × 0.003 × 1000 = 360 → limit = 360 ÷ 1.2 = 300
            borrowed 300 > 300?  NO  → safe (equal = NOT liquidatable)
oracle 119: collateral = 119 × 0.003 × 1000 = 357 → limit = 357 ÷ 1.2 = 297.5
            borrowed 300 > 297.5?  YES → LIQUIDATED
```

**Full borrow (the worst case).** If Alice borrows the full 460 TUSDT:

```text
liquidatable when collateral < 460 × 1.2 = 552
  → alpha price < 552 ÷ 1000 = 0.552 TUSDT/alpha
  → alpha price < 0.552 ÷ 230 = 0.0024 TAO/alpha  (a 20% drop from 0.003)
```

The check re-reads the oracle on every call — a stale price older than
`max_oracle_age_ms` (30 min default) makes the trigger revert with `OraclePriceStale`.

> **Exact on-chain math**: the limit is floored, so the boundary can land one rao apart
> (test-pinned, `is_liquidatable_boundary`, tests.rs:511): with price 1.0 and 1,000
> collateral, the default LR 120% gives `limit = floor(1000 / 1.2) = 833`; debt 833 →
> safe, debt 834 → liquidatable. One rao flips the vault.

## 4. The liquidation auction, step by step

Anyone may call `trigger_liquidation_auction(owner, vault_id)` (permissionless; reverts
`NotLiquidatable` if healthy, `VaultInLiquidation` if an auction is already open).
Suppose the oracle crashes to 90 TUSDT/TAO and the alpha price has fallen to
0.0024 TAO/alpha — Alice's vault is now liquidatable.

**Step 1 — trigger** (lib.rs:1315): the vault unstakes **all** 1,000 alpha via
`remove_stake` (ext fn 2) and reads the native TAO actually received from the balance
delta:

```text
TAO received = 1000 × 0.0024 = 2.4 TAO
```

The auction sells this TAO for TUSDT. Debt at trigger `D = 300 TUSDT` is frozen as
`debt_balance`, and `active_liquidation_count += 1` (this blocks `claim_excess_alpha`,
`set_vault_hotkey`, and `transfer_native_to_treasury` until settlement).

**Step 2 — minimum bid** (`liquidation_min_bid`, risk.rs:53) — debt plus the
liquidation fee (11% default), fee floored separately then added:

```text
min bid = 300 + 300 × 11% = 300 + 33 = 333 TUSDT
```

**Step 3 — bidding** (ascending bids in TUSDT; bidders must pre-approve the auction for
`transfer_from`). Bob bids the minimum 333; Charlie re-bids 350. Re-bids must
**strictly** increase (`BidAmountNotIncreased`) and pull only the delta:

```text
Bob:     333 TUSDT
Charlie: 350 TUSDT  → wins (highest bid at finalize)
```

**Step 4 — finalize** (permissionless, after `ends_at` = trigger + 1 h; needs ≥ 1 bid).

**Step 5 — settle** (`settle_liquidation_auction`, permissionless). The winning bid's
TUSDT flows to the vault, which **burns exactly the debt** (`debt_cleared = D` — the
frozen amount, *not* the full bid). The surplus stays in the vault as protocol surplus
(`claim_surplus_tusdt` → treasury). The winner pays the transaction fee on the
collateral (TAO) side:

```text
tx fee        = 0.3% × 2.4 = 0.0072 TAO     → treasury
winner gets   = 2.4 − 0.0072 = 2.3928 TAO    → Charlie
debt burned   = 300 TUSDT (exactly, no interest)
surplus       = 350 − 300 = 50 TUSDT         → vault → treasury
```

Notes: borrow and repay are **free** — the 0.3% transaction fee exists only here, at
settlement, on the collateral side. If nobody bids before the auction expires, only the
configured **admin** may bid (backstop); losing bidders withdraw their TUSDT with
`withdraw_refund`.

> **Exact on-chain math**: 333 TUSDT = 333,000,000,000 rao
> (`300,000,000,000 + floor(300,000,000,000 × 0.11)`), and the tx fee is
> `floor(0.3% × 2,400,000,000) = 7,200,000 rao` — Charlie receives
> 2,392,800,000 rao.

## 5. Parameter sensitivity: same position, different risk settings

Base position: 1,000 alpha, alpha price 0.003 TAO/alpha, oracle 230 TUSDT/TAO →
collateral value 690 TUSDT. Full debt = max borrow. The liquidation alpha price P
solves `1000 × P × 230 ÷ LR < max_borrow`, i.e. `P = max_borrow × LR ÷ 230,000`:

| Set | CR | LR | Liq fee | Max borrow | Liquidation limit | Buffer | Liq alpha price (TAO/alpha) | Min bid at full debt |
|---|---|---|---|---|---|---|---|---|
| (a) defaults | 150% | 120% | 11% | 460.00 | 575.00 | 115.00 | 0.0024 | 510.60 |
| (b) | 175% | 140% | 15% | 394.29 | 492.86 | 98.57 | 0.0024 | 453.43 |
| (c) | 200% | 150% | 25% | 345.00 | 460.00 | 115.00 | 0.00225 | 431.25 |
| (d) | 300% | 200% | 11% | 230.00 | 345.00 | 115.00 | 0.002 | 255.30 |

(TUSDT amounts; exact values: max borrow 460 / 394.2857 / 345 / 230, limit
575 / 492.8571 / 460 / 345, buffer = limit − max = 115 / 98.5714 / 115 / 115.)
Set (a) matches the full-borrow case in section 3: `P = 460 × 1.2 ÷ 230,000 = 0.0024`.
Raising the collateral ratio pushes the max borrow down — and the liquidation trigger
down with it: set (d) liquidates only after a 33% drop from 0.003 to 0.002 TAO/alpha.

**Min-bid sensitivity** (fixed debt 1 TUSDT, fee floored separately):

| Liq fee | Fee amount | Min bid |
|---|---|---|
| 0% | 0 | 1.00 |
| 5% | 0.05 | 1.05 |
| 11% (default) | 0.11 | 1.11 |
| 25% | 0.25 | 1.25 |
| 100% (max) | 1.00 | 2.00 |

## 6. Fees and boundaries at a glance

- **Creation fee** — 5,000,000 rao (0.005 TAO) payable with `create_alpha_vault`;
  excess is refunded. Send 10,000,000 → 5,000,000 kept + 5,000,000 refunded. Send
  4,999,999 → `VaultCreationFeeNotMet`, no vault created, nothing taken.
- **Param validation** (governance cannot set these, all → `InvalidRatio` /
  `InvalidAuctionDuration` / `InvalidOracleMaxAge`): CR == LR (150/150), CR < LR
  (110/120), LR 99%, liq fee 101%, tx fee 101%, auction < 60 s (30,000 ms), auction
  > 7 days, oracle age 0. CR must be **strictly** greater than LR.
- **Paused** → `ContractPaused` on create/borrow/repay/release/trigger (settlement is
  never paused).
- **Errors** you will meet in these flows: `CollateralRatioExceeded`,
  `NotLiquidatable`, `VaultInLiquidation`, `StakeTransferFailed`, `RepayAmountTooHigh`,
  `ActiveLiquidationsExist`, `OraclePriceStale`, `OraclePriceUnavailable` — see
  [`../errors/vault-alpha.md`](../errors/vault-alpha.md).

## 7. Quick reference (defaults)

| Formula | Value |
|---|---|
| price per alpha | `oracle × alpha price` — 230 × 0.003 = 0.69 TUSDT/alpha |
| collateral value | `price × amount` — 0.69 × 1000 = 690 TUSDT |
| max borrow | `collateral ÷ CR` — 690 ÷ 1.5 = 460 TUSDT (CR 150%) |
| liquidatable iff | `borrowed > collateral ÷ LR` — limit 690 ÷ 1.2 = 575, strict `>` (LR 120%) |
| min bid | `debt + debt × 11%` — 300 + 33 = 333 TUSDT |
| settle tx fee | `0.3% × collateral TAO`, at settlement, TAO side — 0.3% × 2.4 = 0.0072 TAO |
| interest | **none** — repay 1:1 |
