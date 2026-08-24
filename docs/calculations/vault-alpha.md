# tusdt-vault-alpha — Calculation Guide

Step-by-step numeric walkthroughs for the Alpha Vault (CDP engine): how much you can
borrow, when liquidation happens, and what the auction looks like — worked out with the
exact integer math the contract uses. Companion to the reference page
[`../contracts/vault-alpha.md`](../contracts/vault-alpha.md) and the
[error catalog](../errors/vault-alpha.md).

> **Verification**: every number on this page was recomputed with an independent Python
> harness that reimplements the contract's fixed-point operations exactly and reproduces
> every numeric pin in the contract test suite (`tests.rs`) — including
> `max_borrow_allowed_spec_example`, `is_liquidatable_boundary`, and
> `liquidation_min_bid_includes_fee`.

## Numbers and scales (read this first)

| Quantity | Scale | Example |
|---|---|---|
| Balance / token amount | `u64`, 9 decimals | 1 TUSDT = 1,000,000,000 rao |
| Internal `Ratio` (fixed-point) | 1e18 inner | 1.5 = 1,500,000,000,000,000,000 |
| Parameter configs (external messages) | basis points | 15,000 bps = 150% |
| Oracle price (TUSDT/TAO) | 1e18 Ratio | 200 TAO → inner 200 × 10¹⁸ |
| Chain alpha price (`get_alpha_price`, ext fn 15) | rao, 1e9 per α | 377,277 rao = 0.000377277 TAO/α |

Rounding rules used by the contract (all floor for positive values): `checked_mul_value`
computes `floor(value × ratio / 1e18)`, `checked_div_value` computes
`floor(value / ratio)`. **The vault has no interest**: debt = borrowed, repay 1:1
(`repay_token` rejects `amount > debt` with `RepayAmountTooHigh`).

## 1. Pricing: how much is my alpha worth?

Two price sources are multiplied (`current_collateral_price`, lib.rs:1676):

$$\text{TUSDT per } \alpha = \text{oracle}_{TUSDT/TAO} \times \frac{\text{alpha\_price\_rao}}{10^9}$$

$$\text{collateral\_value} = \left\lfloor \frac{\text{price} \times \text{collateral\_rao}}{10^{18}} \right\rfloor \quad \text{[TUSDT rao]}$$

**Worked example (used throughout this guide).** Alice has 100 α on netuid 1,
α-price = 377,277 rao, oracle = 200 TUSDT/TAO.

```text
Step 1 — alpha price as a Ratio (alpha_price_rao_to_ratio, lib.rs:1753):
  alpha_to_tao = Ratio::from_integer(377_277).checked_div_int(1e9)
               = (377_277 × 1e18) // 1e9 = 377_277_000_000_000      (= 0.000377277)

Step 2 — combine with the oracle (1e18 × 1e18, truncating mul):
  price = (200 × 1e18) × 377_277_000_000_000 // 1e18
        = 75_455_400_000_000_000                                      (= 0.0754554 TUSDT/α)

Step 3 — collateral value (risk.rs:5, checked_mul_value floors):
  collateral_value = 100_000_000_000 × 75_455_400_000_000_000 // 1e18
                   = 7_545_540_000 rao = 7.54554 TUSDT
```

## 2. How much can I borrow?

`max_borrow_allowed` (risk.rs:14) — the collateral ratio is the divisor
(`checked_div_value` = `value / self`):

$$\text{max\_borrow} = \left\lfloor \frac{\text{collateral\_value}}{\text{collateral\_ratio}} \right\rfloor$$

Borrowing is capped on **every** `borrow_token` call: if
`borrowed + amount > max_borrow` the call reverts with `CollateralRatioExceeded`.
Releasing collateral applies the same cap against the projected post-release balance.

```text
max_borrow = 7_545_540_000 × 1e18 // (15_000 bps × 1e14 = 1_500_000_000_000_000_000)
           = 5_030_360_000 rao = 5.03036 TUSDT
```

Alice borrows 60% of her maximum: `3_018_216_000 rao = 3.018216 TUSDT`.

**Where the floor bites** (test-pinned, `tests.rs:457`): with price 1.0 and 1,000 rao of
collateral, `max_borrow = 1000 / 1.5 = 666.67 → 666` — the `666` is stored, the
`0.67` rao is discarded. Same for `100 / 3 = 33.33 → 33`.

**Reading your vault** (all derived, not stored — compute client-side):

| Quantity | Formula | Alice's values |
|---|---|---|
| Collateral value | price × collateral | 7.54554 TUSDT |
| Debt | borrowed | 3.018216 TUSDT |
| LTV | debt / collateral value | 40.0% |
| Borrow headroom | max_borrow − debt | 2,012,144,000 rao = 2.012144 TUSDT |
| Liquidation limit | see below | 6.28795 TUSDT |

## 3. When does liquidation happen?

`liquidation_limit` (risk.rs:30) and `is_liquidatable` (risk.rs:46):

$$\text{liquidatable} \iff \text{borrowed} > \left\lfloor \frac{\text{collateral\_value}}{\text{liquidation\_ratio}} \right\rfloor$$

The comparison is **strict** (`>`): sitting *exactly at* the limit is safe. The check
re-reads the oracle on every call — a stale price older than `max_oracle_age_ms`
(30 min default) makes the trigger revert with `OraclePriceStale`.

```text
liquidation_limit = 7_545_540_000 × 1e18 // (12_000 bps × 1e14 = 1_200_000_000_000_000_000)
                  = 6_287_950_000 rao = 6.28795 TUSDT

Alice's debt 3_018_216_000 < 6_287_950_000  →  safe, plenty of room.
```

**Solving for the liquidation price.** Alice's collateral value at oracle price $P$ is
`value(P) = floor(P × 377_277 × 100_000_000_000 / 1e18 / 1e9) = P × 37_727.7`. She is
liquidated when `3_018_216_000 > floor(value(P) / 1.2)`:

```text
P = 96:  value = 96 × 37_727.7 = 3_621_859_200 → limit = 3_018_216_000
         borrowed 3_018_216_000 > 3_018_216_000?  NO  → safe (equal = NOT liquidatable)
P = 95:  value = 95 × 37_727.7 = 3_584_131_500 → limit = 2_986_776_250
         borrowed 3_018_216_000 > 2_986_776_250?  YES → LIQUIDATED
```

Her liquidation price is oracle = **95** — a 52.5% drop from 200. At 96 she is safe, at
95 she is liquidatable, and the boundary demonstrates the strict `>`: exactly at the
limit (limit == debt) she is safe.

**The strict boundary, smallest scale** (test-pinned, `tests.rs:511`): a position worth
1,000,000 rao with LR 120% has `limit = 833,333`. Debt `833,333` → safe; debt
`833,334` → liquidatable. **One rao flips the vault.**

## 4. The liquidation auction, step by step

Anyone may call `trigger_liquidation_auction(owner, vault_id)` (permissionless;
reverts `NotLiquidatable` if healthy, `VaultInLiquidation` if an auction is open).
Assume the oracle has crashed to 90 so Alice's vault is underwater.

**Step 1 — trigger** (lib.rs:1306): the vault unstakes **all** 100 α via `remove_stake`
(ext fn 2) and reads the native TAO actually received from the balance delta:

```text
tao_received = 100 α × 377_277 rao/α = 37_727_700 rao = 0.0377277 TAO
```

The auction sells this TAO for TUSDT. Debt at trigger `D = 3_018_216_000 rao` is frozen
as `debt_balance`. `active_liquidation_count += 1` (this blocks `claim_excess_alpha`,
`set_vault_hotkey`, `transfer_native_to_treasury` until settlement).

**Step 2 — minimum bid** (`liquidation_min_bid`, risk.rs:53) — the liquidation fee is
floored separately, then added:

$$\text{min\_bid} = \text{debt} + \left\lfloor \text{debt} \times \text{liquidation\_fee} \right\rfloor$$

```text
min_bid = 3_018_216_000 + 3_018_216_000 × (1_100 bps × 1e14) // 1e18
        = 3_018_216_000 + 331_998_760 = 3_350_219_760 rao = 3.35021976 TUSDT
```

**Step 3 — bidding** (auction contract, ascending bids in TUSDT; bidders must
pre-approve the auction for `transfer_from`). Bob bids the minimum; Charlie re-bids 5%
higher — re-bids must **strictly** increase (`BidAmountNotIncreased`) and pull only the
delta:

```text
Charlie's bid = 3_350_219_760 + floor(3_350_219_760 × 0.05) = 3_517_730_748 rao
```

**Step 4 — finalize** (permissionless, after `ends_at` = trigger + 1 h; needs ≥ 1 bid).

**Step 5 — settle** (`settle_liquidation_auction`, permissionless). The winning bid's
TUSDT flows to the vault, which **burns exactly the debt** (`debt_cleared = D` — the
frozen amount, *not* the full bid). The surplus stays in the vault as protocol surplus
(`claim_surplus_tusdt` → treasury). The winner pays the transaction fee on the
collateral (TAO) side:

```text
tx fee        = floor(0.003 × 37_727_700) = 113_183 rao TAO        → treasury
winner gets   = 37_727_700 − 113_183 = 37_614_517 rao TAO          → Charlie
debt burned   = 3_018_216_000 rao TUSDT (exactly, no interest)
surplus       = 3_517_730_748 − 3_018_216_000 = 499_514_748 rao    → vault → treasury
```

Notes: borrow and repay are **free** — the 0.3% transaction fee exists only here, at
settlement, on the collateral side. If nobody bids before the auction expires, only the
configured **admin** may bid (backstop); losing bidders withdraw their TUSDT with
`withdraw_refund`.

## 5. Parameter sensitivity: same position, different risk settings

Base position: 100 α, α-price 377,277 rao, oracle 200 → collateral value
7,545,540,000 rao (7.54554 TUSDT). Full debt = max borrow.

| Set | CR | LR | Liq fee | Max borrow | Liquidation limit | Buffer (limit − max) | Liq. oracle | Min bid (full debt) |
|---|---|---|---|---|---|---|---|---|
| (a) defaults | 150% | 120% | 11% | 5.03036 | 6.28795 | 1.25759 | ≤ 159 | 5.5836996 |
| (b) | 175% | 140% | 15% | 4.311737142 | 5.389671428 | 1.077934286 | ≤ 159 | 4.958497713 |
| (c) | 200% | 150% | 25% | 3.77277 | 5.03036 | 1.25759 | ≤ 149 | 4.7159625 |
| (d) | 300% | 200% | 11% | 2.51518 | 3.77277 | 1.25759 | ≤ 133 | 2.7918498 |

(TUSDT amounts; exact rao values: max borrow 5,030,360,000 / 4,311,737,142 /
3,772,770,000 / 2,515,180,000.) The "liq. oracle" column is the highest oracle price
at which a position borrowed to the **full max** is liquidatable: for set (a),
`limit(159) = 4,998,920,250 < 5,030,360,000` → liquidated; `limit(160) = 5,030,360,000`
→ safe. Raising CR pushes the max borrow down and the liquidation trigger down with it.

**Min-bid sensitivity** (fixed debt 1,000,000,000 rao):

| Liq fee | Fee amount | Min bid |
|---|---|---|
| 0% | 0 | 1,000,000,000 |
| 5% | 50,000,000 | 1,050,000,000 |
| 11% (default) | 110,000,000 | 1,110,000,000 |
| 25% | 250,000,000 | 1,250,000,000 |
| 100% (max) | 1,000,000,000 | 2,000,000,000 |

## 6. Fees and boundaries at a glance

- **Creation fee** — 5,000,000 rao (0.005 TAO) payable with `create_alpha_vault`; excess
  is refunded. Send 10,000,000 → 5,000,000 kept + 5,000,000 refunded. Send 4,999,999 →
  `VaultCreationFeeNotMet`, no vault created, nothing taken.
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
| price_per_alpha | `oracle × α_price_rao / 1e9` |
| collateral_value | `floor(price × collateral_rao / 1e18)` |
| max_borrow | `floor(collateral_value / CR)` — CR 150% |
| liquidatable iff | `borrowed > floor(collateral_value / LR)` — LR 120%, strict `>` |
| min_bid | `debt + floor(debt × 11%)` |
| settle tx fee | `floor(0.3% × collateral_TAO)` at settlement, TAO side |
| interest | **none** — repay 1:1 |
