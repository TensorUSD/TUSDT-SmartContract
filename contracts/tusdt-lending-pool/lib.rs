#![cfg_attr(not(feature = "std"), no_std, no_main)]
#![allow(clippy::upper_case_acronyms)]
#![allow(clippy::enum_variant_names)]

/// Public re-exports of the lending pool's core types.
pub use self::lending_pool::{
    AlphaMarketParams, AlphaMarketParamsConfig, InterestRateParams, InterestRateParamsConfig,
    PoolGlobalParams, PoolGlobalParamsConfig, RootStakeConfig, TusdtLendingPool,
    TusdtLendingPoolRef,
};

#[ink::contract(env = tusdt_env::CustomEnvironment)]
mod lending_pool {
    use core::cmp::min;
    use ink::env::call::FromAccountId;
    use ink::prelude::vec::Vec;
    use ink::storage::{Mapping, StorageVec};
    use ink::ToAccountId;

    use tusdt_erc20::TusdtErc20Ref;
    use tusdt_oracle::TusdtOracleRef;
    use tusdt_primitives::Ratio;

    mod params {
        include!("params.rs");
    }
    mod rates {
        include!("rates.rs");
    }
    mod risk {
        include!("risk.rs");
    }

    pub(crate) use params::{
        default_alpha_params, default_global_params, default_tao_interest_params,
        default_tusdt_interest_params, PARAMS_TIMELOCK_MS,
    };
    /// Re-exports of parameter types and helpers that live in `params` so the
    /// crate-root `pub use` and the tests' `use super::lending_pool::*` keep
    /// resolving the same names at the same paths.
    pub use params::{
        AlphaMarketParams, AlphaMarketParamsConfig, InterestRateParams, InterestRateParamsConfig,
        PendingAlphaParamsUpdate, PendingGlobalParamsUpdate, PendingInterestParamsUpdate,
        PoolGlobalParams, PoolGlobalParamsConfig,
    };

    const PAGE_SIZE: u32 = 10;
    /// Minimum alpha stake amount accepted for collateral operations
    /// (100_000 in 9-decimal token units).
    #[allow(dead_code)]
    pub(crate) const MIN_STAKE: Balance = 100_000;

    /// Minimum allowed `stake_floor` for root-subnet staking — the pallet's
    /// minimum stake on the root subnet (0.002 TAO in 9-decimal units).
    pub(crate) const MIN_ROOT_STAKE_FLOOR: Balance = 2_000_000;
    /// Default `stake_buffer` for root-subnet staking (1 TAO kept liquid).
    pub(crate) const DEFAULT_STAKE_BUFFER: Balance = 1_000_000_000;

    /// Which asset a user is interacting with.
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    #[allow(dead_code)]
    pub enum Asset {
        TAO = 0,
        TUSDT = 1,
    }

    /// lTokens to mint for a supply of `amount` underlying at
    /// `exchange_rate` (floored). Minting at a grown rate hands the depositor
    /// at most their exact share of the pool, so late deposits can never
    /// dilute earlier suppliers' accrued interest. Returns `None` on a zero
    /// rate or overflow. `exchange_rate` is `>= 1e18` in practice.
    pub(crate) fn compute_mint_amount(amount: Balance, exchange_rate: Ratio) -> Option<Balance> {
        // checked_div_value(value) computes value / self — the divisor is the rate.
        exchange_rate.checked_div_value(amount.into()).and_then(|v| Balance::try_from(v).ok())
    }

    /// Underlying redeemable for `ltoken_amount` at `exchange_rate`
    /// (floored — the pool never pays out more than the lTokens are worth;
    /// the sub-rao dust stays with the pool). Returns `None` on overflow.
    pub(crate) fn compute_redeem_amount(
        ltoken_amount: Balance,
        exchange_rate: Ratio,
    ) -> Option<Balance> {
        // checked_mul_value(value) computes value * self.
        exchange_rate
            .checked_mul_value(ltoken_amount.into())
            .and_then(|v| Balance::try_from(v).ok())
    }

    /// Resets the lToken exchange rate to 1.0 once the market has fully
    /// drained (`total_supplied == 0`). The exchange rate grows in
    /// `accrue_interest` and `charge_prepaid_hour`, so without this reset the
    /// next genesis supply (which
    /// mints 1:1 at the empty-market branch) would be credited at the stale
    /// grown rate — the new supplier's lTokens would claim more underlying
    /// than they deposited, leaving the pool short. The borrow index is left
    /// untouched: it is self-consistent for scaled debt, and resetting it
    /// could corrupt drifted legacy positions.
    pub(crate) fn reset_exchange_rate_when_drained(state: &mut MarketState) {
        if state.total_supplied == 0 {
            state.exchange_rate = Ratio::one();
        }
    }

    /// Converts a scaled debt amount to its face value under a borrow index:
    /// `floor(scaled × borrow_index / 1e18)`. Single source of truth for the
    /// derived market `total_debt` — every mutation site recomputes the face
    /// total from `total_scaled_debt` through this helper so it stays exactly
    /// consistent with the per-user formula used by `get_user_debt`.
    /// Returns `None` on overflow.
    pub(crate) fn scaled_debt_to_face(scaled: Balance, index: Ratio) -> Option<Balance> {
        index.checked_mul_value(scaled.into()).and_then(|v| Balance::try_from(v).ok())
    }

    /// The portion of `reserve` the treasury may claim: everything above the
    /// market's unfunded deficit (which outranks the treasury claim — see
    /// `cover_deficit`), capped at the physical cash available. `0` when the
    /// deficit consumes the whole reserve, so a permissionless reserve claim
    /// can never strip the deficit's backing and leave suppliers short.
    pub(crate) fn reserve_claimable(reserve: Balance, deficit: Balance, cash: Balance) -> Balance {
        reserve.saturating_sub(deficit).min(cash)
    }

    /// Computes a health factor as a 1e18 `Ratio`:
    /// `(liquidation_threshold × collateral_value) / debt_value`.
    ///
    /// Extracted as a pure helper so the scale-critical arithmetic is unit
    /// testable: the full `get_health_factor` reads the oracle through a
    /// cross-contract call, which the off-chain test environment cannot serve.
    ///
    /// **Scale trap this exists to prevent**: `checked_mul_value` returns a
    /// plain rao-scale integer, NOT a `Ratio` inner. Passing that integer to
    /// `Ratio::from_inner` reinterprets it at the 1e18 scale (dividing its
    /// meaning by 1e18); the subsequent `checked_div_int` — which computes
    /// `self.0 / rhs` — then collapses the whole expression to a near-zero
    /// integer, so every position carrying debt reports a health factor of
    /// ~0 and looks liquidatable. `Ratio::from_integer` re-scales correctly.
    ///
    /// Returns `None` on overflow or when `debt_value` is zero (callers treat a
    /// zero debt as "no health factor" before reaching here).
    pub(crate) fn health_factor_from_values(
        threshold: Ratio,
        collateral_value: Balance,
        debt_value: Balance,
    ) -> Option<Ratio> {
        threshold
            .checked_mul_value(collateral_value.into())
            .and_then(|weighted| Ratio::from_integer(weighted).checked_div_int(debt_value.into()))
    }

    /// Per-market runtime accrual state. Markets 0 (TAO) and 1 (TUSDT) are supply+borrow
    /// markets with interest accrual. Markets 2+ are alpha collateral-only markets (one per
    /// approved subnet); their MarketState exists but accrual is a no-op.
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct MarketState {
        /// Total underlying supplied, tracked in lToken (scaled) units: the
        /// face value owed to suppliers is `total_supplied × exchange_rate`,
        /// and accrued interest grows it through the exchange rate alone.
        /// Supply mints `amount / exchange_rate` lTokens and withdrawal burns
        /// the lTokens themselves, so this always equals the lToken supply.
        pub total_supplied: Balance,
        /// Total outstanding debt in underlying units (includes accrued interest).
        /// Always derived as `floor(total_scaled_debt × borrow_index / 1e18)`
        /// at every mutation site — see `total_scaled_debt`.
        pub total_debt: Balance,
        /// Total outstanding debt in scaled units (the Aave `drawnShares`
        /// analog). This is the source of truth: borrow/repay/liquidate add or
        /// subtract the SAME scaled value here and on the user position, so the
        /// market total and the sum of per-user scaled positions move in exact
        /// lockstep and can never drift. Face `total_debt` is recomputed from
        /// this value on every mutation, and interest accrues into it purely
        /// through borrow_index growth (no explicit total mutation).
        pub total_scaled_debt: Balance,
        /// Accumulator for per-user scaled debt: user_debt = scaled_debt × borrow_index.
        /// Starts at 1.0 (1e18); grows at the borrow rate.
        pub borrow_index: Ratio,
        /// lToken exchange rate: underlying = ltoken_balance × exchange_rate.
        /// Starts at 1.0; grows at the supply rate.
        pub exchange_rate: Ratio,
        /// Protocol share of accrued interest, in underlying units.
        pub reserve_accrued: Balance,
        /// Timestamp (ms) of the last interest accrual.
        pub last_update: u64,
    }

    impl MarketState {
        fn new(now: u64) -> Self {
            Self {
                total_supplied: 0,
                total_debt: 0,
                total_scaled_debt: 0,
                borrow_index: Ratio::one(),
                exchange_rate: Ratio::one(),
                reserve_accrued: 0,
                last_update: now,
            }
        }
    }

    /// Per-(market, user) position.
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct Position {
        /// lToken receipt balance held by the user (supply markets only).
        pub ltoken_balance: Balance,
        /// Scaled debt: actual_debt = scaled_debt × market.borrow_index.
        pub scaled_debt: Balance,
        /// Alpha principal units deposited (alpha markets only).
        pub alpha_principal: Balance,
    }

    /// Governance configuration for idle-TAO root-subnet staking (mirrors the
    /// `set_root_stake_config` message parameters and `get_root_stake_config`
    /// return value).
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct RootStakeConfig {
        /// Hotkey the pool stakes idle TAO to on the root subnet (netuid 0).
        pub root_hotkey: AccountId,
        /// Emergency off-switch: when `false`, no new TAO is staked.
        pub staking_enabled: bool,
        /// Free native TAO the sweep must leave untouched.
        pub stake_buffer: Balance,
        /// Sweeps trigger only once the free balance exceeds buffer + threshold.
        pub sweep_threshold: Balance,
        /// Minimum stake amount (`>= MIN_ROOT_STAKE_FLOOR`); also the minimum
        /// `stake_buffer`.
        pub stake_floor: Balance,
    }

    /// Lending pool storage: roles, market state, user positions, and risk parameters.
    #[ink(storage)]
    pub struct TusdtLendingPool {
        // ── Roles ──
        governance: AccountId,
        maintainer: AccountId,
        treasury: AccountId,
        platform: AccountId,
        paused: bool,
        /// Reentrancy guard.
        busy: bool,

        // ── External refs ──
        /// TUSDT/TAO price oracle (existing instance).
        oracle: TusdtOracleRef,
        /// Existing TUSDT token (underlying borrowable + supplied asset).
        tusdt: TusdtErc20Ref,
        /// Single staking hotkey for all alpha collateral.
        pool_hotkey: AccountId,

        // ── Markets ──
        /// Market state by id (0 = TAO, 1 = TUSDT, 2+ = alpha netuids).
        markets: Mapping<u8, MarketState>,
        /// Enumeration of all market ids for pagination.
        market_keys: StorageVec<u8>,
        /// lToken (tusdt-erc20 child) address per supply market (0, 1).
        ltoken_by_market: Mapping<u8, AccountId>,
        /// Approved netuid → alpha market id.
        netuid_to_market: Mapping<u16, u8>,
        /// Alpha market id → netuid.
        market_to_netuid: Mapping<u8, u16>,
        /// Next market id for alpha markets.
        next_alpha_market_id: u8,

        // ── Per-market static params ──
        market_params: Mapping<u8, InterestRateParams>,
        alpha_params: Mapping<u16, AlphaMarketParams>,
        pending_market_params: Mapping<u8, PendingInterestParamsUpdate>,
        pending_alpha_params: Mapping<u16, PendingAlphaParamsUpdate>,

        // ── Global params + timelock ──
        global_params: PoolGlobalParams,
        pending_global_params: Option<PendingGlobalParamsUpdate>,

        // ── Alpha custody accounting ──
        /// Total alpha principal per netuid (Σ user alpha_principal).
        netuid_total_collateral: Mapping<u16, Balance>,

        // ── User positions ──
        /// Per-(market_id, user) position.
        positions: Mapping<(u8, AccountId), Position>,
        /// Position keys for paginated enumeration.
        position_keys: StorageVec<(u8, AccountId)>,
        /// Outstanding debt principal per (market, user) in underlying units.
        /// Interest owed = current debt − debt_principal. Kept in its own
        /// mapping (not inside `Position`) so the stored `Position` layout
        /// stays unchanged and any existing positions remain decodable.
        debt_principal: Mapping<(u8, AccountId), Balance>,
        /// Unfunded bad-debt per market, in face units of the market's asset,
        /// FROZEN at write-off (never multiplied by the borrow index again, so
        /// it cannot compound or poison utilization). Written when a
        /// liquidation consumes a borrower's entire collateral position with
        /// debt remaining; reduced by `cover_deficit`, which funds it from
        /// `reserve_accrued`.
        market_deficit: Mapping<u8, Balance>,

        // ── Idle TAO root-subnet staking ──
        /// Hotkey the pool stakes idle TAO to on the root subnet (netuid 0).
        root_hotkey: AccountId,
        /// Booked TAO currently staked on the root subnet.
        staked_tao: Balance,
        /// Free native TAO the sweep leaves untouched (liquidity sleeve).
        stake_buffer: Balance,
        /// Sweeps trigger only once the free balance exceeds buffer + threshold.
        sweep_threshold: Balance,
        /// Minimum root stake amount (`>= MIN_ROOT_STAKE_FLOOR`), avoids dust.
        stake_floor: Balance,
        /// Block number of the last sweep (`None` = never swept; rate limit: 1
        /// sweep per block).
        last_sweep_block: Option<BlockNumber>,
        /// Emergency off-switch: when `false`, no new TAO is staked.
        staking_enabled: bool,
    }

    /// Events emitted by the lending pool.
    #[ink(event)]
    pub struct LiquidDeposited {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
        /// User who supplied.
        #[ink(topic)]
        pub user: AccountId,
        /// Underlying amount supplied (9-decimal units).
        pub amount: Balance,
        /// lToken balance minted.
        pub ltoken_scaled: Balance,
    }

    /// Emitted when a user withdraws underlying liquidity by burning lTokens.
    #[ink(event)]
    pub struct LiquidWithdrawn {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
        /// User who withdrew.
        #[ink(topic)]
        pub user: AccountId,
        /// Underlying amount withdrawn (9-decimal units).
        pub amount: Balance,
        /// lToken balance burned.
        pub ltoken_scaled: Balance,
    }

    /// Emitted when a user borrows underlying from a market.
    #[ink(event)]
    pub struct Borrowed {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
        /// Borrower.
        #[ink(topic)]
        pub user: AccountId,
        /// Underlying amount borrowed (9-decimal units).
        pub amount: Balance,
    }

    /// Emitted when a user repays borrowed underlying.
    #[ink(event)]
    pub struct Repaid {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
        /// Borrower.
        #[ink(topic)]
        pub user: AccountId,
        /// Underlying amount repaid (9-decimal units).
        pub amount: Balance,
    }

    /// Emitted when a user deposits alpha stake as collateral.
    #[ink(event)]
    pub struct AlphaCollateralDeposited {
        /// Subnet id the collateral was deposited on.
        #[ink(topic)]
        pub netuid: u16,
        /// Depositor.
        #[ink(topic)]
        pub user: AccountId,
        /// Alpha principal deposited (9-decimal units).
        pub amount: Balance,
        /// Alpha market id assigned to the netuid.
        pub market_id: u8,
    }

    /// Emitted when a user withdraws alpha collateral to a destination coldkey.
    #[ink(event)]
    pub struct AlphaCollateralWithdrawn {
        /// Subnet id the collateral was withdrawn from.
        #[ink(topic)]
        pub netuid: u16,
        /// Withdrawer.
        #[ink(topic)]
        pub user: AccountId,
        /// Alpha principal withdrawn (9-decimal units).
        pub amount: Balance,
        /// Effective alpha withdrawn (equals the principal withdrawn).
        pub effective: Balance,
        /// Coldkey that received the transferred stake.
        #[ink(topic)]
        pub dest_coldkey: AccountId,
    }

    /// Emitted when an underwater position is liquidated in full. The
    /// liquidator pays the borrower's entire debt (with accrued interest) on
    /// both markets and receives the borrower's full alpha collateral minus
    /// the platform's liquidation fee (taken from the collateral itself).
    #[ink(event)]
    pub struct Liquidated {
        /// Borrower whose position was liquidated.
        #[ink(topic)]
        pub user: AccountId,
        /// Account that performed the liquidation.
        #[ink(topic)]
        pub liquidator: AccountId,
        /// Netuids of the seized alpha collateral.
        pub collateral_netuids: Vec<u16>,
        /// Total alpha principal seized across all netuids.
        pub collateral_seized: Balance,
        /// Platform's share of the seized alpha, transferred to the platform
        /// role account.
        pub platform_alpha: Balance,
        /// TAO debt repaid by the liquidator (9-decimal units).
        pub debt_covered_tao: Balance,
        /// TUSDT debt repaid by the liquidator (9-decimal units).
        pub debt_covered_tusdt: Balance,
        /// Face debt written off as a market deficit (0 when the collateral
        /// covers the debt).
        pub deficit: Balance,
    }

    /// Emitted when excess alpha staking is unstaked in full for a netuid and
    /// the resulting native TAO is sent to the treasury.
    #[ink(event)]
    pub struct AlphaExcessClaimed {
        /// Subnet the excess was claimed for.
        #[ink(topic)]
        pub netuid: u16,
        /// Excess alpha found beyond booked collateral, unstaked in full.
        pub excess_alpha: Balance,
        /// Native TAO received from unstaking, forwarded to the treasury.
        pub tao_received: Balance,
        /// Treasury that received the unstaked TAO.
        #[ink(topic)]
        pub recipient: AccountId,
    }

    /// Emitted when protocol reserve fees are claimed for a market.
    #[ink(event)]
    pub struct ReserveClaimed {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
        /// Reserve amount claimed (9-decimal units).
        pub amount: Balance,
    }

    /// Emitted when a liquidation consumes a borrower's entire collateral
    /// position on a netuid while debt remains on the debt market. The
    /// residual debt is removed from the ledger (so utilization and rates are
    /// not poisoned by uncollectible debt) and frozen as an unfunded market
    /// deficit that does not compound.
    #[ink(event)]
    pub struct DeficitReported {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
        /// Borrower whose position was written off.
        #[ink(topic)]
        pub user: AccountId,
        /// Face amount written off (9-decimal units), frozen at this value.
        pub amount: Balance,
    }

    /// Emitted when the maintainer covers part of a market's deficit from
    /// `reserve_accrued` (the treasury's reserve claim absorbs the loss).
    #[ink(event)]
    pub struct DeficitCovered {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
        /// Deficit amount covered (9-decimal units).
        pub amount: Balance,
    }

    /// Emitted after interest is accrued on a supply/borrow market.
    #[ink(event)]
    pub struct MarketAccrued {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
        /// Hours elapsed since the last accrual.
        pub dt_hours: u64,
        /// Utilization ratio (1e18).
        pub utilization: u128,
        /// Annual borrow rate (1e18).
        pub borrow_rate: u128,
        /// Annual supply rate (1e18).
        pub supply_rate: u128,
        /// Reserve accrued this period (9-decimal units).
        pub reserve_delta: Balance,
    }

    /// Emitted when an interest-rate params update is scheduled for a market.
    #[ink(event)]
    pub struct MarketParamsUpdateScheduled {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
        /// Timestamp (ms) after which the update can execute.
        pub execute_after: u64,
    }

    /// Emitted when a scheduled market params update is executed.
    #[ink(event)]
    pub struct MarketParamsUpdated {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
    }

    /// Emitted when a scheduled market params update is cancelled.
    #[ink(event)]
    pub struct MarketParamsUpdateCancelled {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
    }

    /// Emitted when an alpha market params update is scheduled for a netuid.
    #[ink(event)]
    pub struct AlphaParamsUpdateScheduled {
        /// Subnet id.
        #[ink(topic)]
        pub netuid: u16,
        /// Timestamp (ms) after which the update can execute.
        pub execute_after: u64,
    }

    /// Emitted when a scheduled alpha params update is executed.
    #[ink(event)]
    pub struct AlphaParamsUpdated {
        /// Subnet id.
        #[ink(topic)]
        pub netuid: u16,
    }

    /// Emitted when a scheduled alpha params update is cancelled.
    #[ink(event)]
    pub struct AlphaParamsUpdateCancelled {
        /// Subnet id.
        #[ink(topic)]
        pub netuid: u16,
    }

    /// Emitted when a global params update is scheduled.
    #[ink(event)]
    pub struct GlobalParamsUpdateScheduled {
        /// Timestamp (ms) after which the update can execute.
        pub execute_after: u64,
    }

    /// Emitted when a scheduled global params update is executed.
    #[ink(event)]
    pub struct GlobalParamsUpdated {}

    /// Emitted when a scheduled global params update is cancelled.
    #[ink(event)]
    pub struct GlobalParamsUpdateCancelled {}

    /// Emitted when a subnet is approved or removed as an alpha market.
    #[ink(event)]
    pub struct NetuidApproved {
        /// Subnet id.
        #[ink(topic)]
        pub netuid: u16,
        /// Whether the netuid was approved (true) or removed (false).
        pub approved: bool,
        /// Alpha market id assigned when approved; None when removed.
        pub market_id: Option<u8>,
    }

    /// Emitted when a market's lToken contract address is updated.
    #[ink(event)]
    pub struct LTokenAddressUpdated {
        /// Market id (0 = TAO, 1 = TUSDT).
        #[ink(topic)]
        pub market: u8,
        /// Previous lToken address.
        pub old_ltoken: AccountId,
        /// New lToken address.
        pub new_ltoken: AccountId,
    }

    /// Emitted when the governance role is transferred.
    #[ink(event)]
    pub struct PoolGovernanceUpdated {
        /// Previous governance account.
        pub previous: AccountId,
        /// New governance account.
        pub new: AccountId,
    }

    /// Emitted when the treasury account is updated.
    #[ink(event)]
    pub struct PoolTreasuryUpdated {
        /// Previous treasury account.
        pub previous: AccountId,
        /// New treasury account.
        pub new: AccountId,
    }

    /// Emitted when the platform account is updated.
    #[ink(event)]
    pub struct PoolPlatformUpdated {
        /// Previous platform account.
        pub previous: AccountId,
        /// New platform account.
        pub new: AccountId,
    }

    /// Emitted when the pool's staking hotkey is migrated.
    #[ink(event)]
    pub struct PoolHotkeyChanged {
        /// Previous staking hotkey.
        #[ink(topic)]
        pub old_hotkey: AccountId,
        /// New staking hotkey.
        #[ink(topic)]
        pub new_hotkey: AccountId,
    }

    /// Emitted when the oracle contract address is updated.
    #[ink(event)]
    pub struct PoolOracleAddressUpdated {
        /// Previous oracle address.
        pub previous: AccountId,
        /// New oracle address.
        pub new: AccountId,
    }

    /// Emitted when the TUSDT token contract address is updated.
    #[ink(event)]
    pub struct TusdtAddressUpdated {
        /// Previous TUSDT address.
        pub previous: AccountId,
        /// New TUSDT address.
        pub new: AccountId,
    }

    /// Emitted when the pool is paused (emergency pause).
    #[ink(event)]
    pub struct PoolPaused {}

    /// Emitted when the pool is unpaused.
    #[ink(event)]
    pub struct PoolUnpaused {}

    /// Emitted when surplus TUSDT is swept from the pool to the treasury.
    #[ink(event)]
    pub struct PoolSurplusTusdtClaimed {
        /// Treasury account that received the sweep.
        pub recipient: AccountId,
        /// TUSDT amount swept (9-decimal units).
        pub amount: Balance,
    }

    /// Emitted when the pool's native TAO balance is swept to the treasury.
    #[ink(event)]
    pub struct PoolNativeTransferredToTreasury {
        /// Native TAO amount transferred (9-decimal units).
        pub amount: Balance,
    }

    /// Emitted when idle TAO is staked onto the root subnet (netuid 0).
    #[ink(event)]
    pub struct StakedIdleTao {
        /// TAO amount staked (9-decimal units).
        pub amount: Balance,
    }

    /// Emitted when staked TAO is unstaked from the root subnet (netuid 0)
    /// to cover an outflow.
    #[ink(event)]
    pub struct UnstakedIdleTao {
        /// TAO amount received back into the free balance (9-decimal units).
        pub amount: Balance,
    }

    /// Emitted when the root-subnet staking configuration is updated.
    #[ink(event)]
    pub struct RootStakeConfigUpdated {
        /// Hotkey the pool stakes idle TAO to on the root subnet (netuid 0).
        pub root_hotkey: AccountId,
        /// Whether staking is enabled.
        pub staking_enabled: bool,
        /// Free native TAO the sweep leaves untouched.
        pub stake_buffer: Balance,
        /// Sweeps trigger only once the free balance exceeds buffer + threshold.
        pub sweep_threshold: Balance,
        /// Minimum root stake amount.
        pub stake_floor: Balance,
    }

    /// Emitted when the maintainer role is updated.
    #[ink(event)]
    pub struct PoolMaintainerUpdated {
        /// New maintainer account.
        #[ink(topic)]
        pub new_maintainer: AccountId,
    }

    /// Error types for the lending pool. All variants are fieldless for compact SCALE encoding.
    ///
    /// Variant groups:
    /// - **Access**: `NotGovernance`, `NotGovernanceOrPlatform`, `NotMaintainer`, `ContractPaused`, `Reentrancy`
    /// - **Amounts & liquidity**: `ZeroAmount`, `LiquidityInsufficient`, `MintBelowPrecision`, `InsufficientLTokenBalance`, `SupplyCapExceeded`, `BorrowCapExceeded`, `BorrowHealthExceeded`, `RepayAmountTooHigh` (reserved — never constructed, kept for ABI compat)
    /// - **Alpha collateral**: `UnapprovedNetuid`, `NetuidHasPositions`, `InsufficientCollateral`, `InsufficientAvailableStake`, `StakeTransferFailed`, `ChainExtensionFailed`, `NoAlphaStakeFound` (reserved — never constructed, kept for ABI compat), `TooManyNetuids`
    /// - **Pricing & health**: `OracleCallFailed`, `OraclePriceUnavailable`, `OraclePriceStale`, `HealthFactorBelowThreshold`, `NotLiquidatable`, `InvalidCollateralNetuid`, `InvalidDebtMarket`, `CloseFactorExceeded` (reserved — never constructed, kept for ABI compat), `CollateralAwardExceedsPosition` (reserved — never constructed, kept for ABI compat)
    /// - **Params / timelock**: `InvalidRatio`, `InvalidParam`, `NoPendingMarketParamsUpdate`, `NoPendingAlphaParamsUpdate`, `NoPendingGlobalParamsUpdate`, `ParamsUpdateTimelockActive`
    /// - **Cross-contract**: `TokenContractCallFailed`, `TokenTransferFromFailed`, `LTokenCallFailed`, `TransferFailed`
    /// - **General**: `MarketNotFound`, `PositionNotFound`, `ArithmeticError`
    #[derive(Debug, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    pub enum Error {
        // ── Access ──
        NotGovernance,
        NotGovernanceOrPlatform,
        NotMaintainer,
        ContractPaused,
        Reentrancy,

        // ── Amounts & liquidity ──
        ZeroAmount,
        LiquidityInsufficient,
        MintBelowPrecision,
        InsufficientLTokenBalance,
        SupplyCapExceeded,
        BorrowCapExceeded,
        BorrowHealthExceeded,
        // Reserved — never constructed (kept for ABI compat).
        RepayAmountTooHigh,

        // ── Alpha collateral ──
        UnapprovedNetuid,
        NetuidHasPositions,
        InsufficientCollateral,
        InsufficientAvailableStake,
        StakeTransferFailed,
        ChainExtensionFailed,
        // Reserved — never constructed (kept for ABI compat).
        NoAlphaStakeFound,
        TooManyNetuids,

        // ── Pricing & health ──
        OracleCallFailed,
        OraclePriceUnavailable,
        OraclePriceStale,
        HealthFactorBelowThreshold,
        NotLiquidatable,
        InvalidCollateralNetuid,
        InvalidDebtMarket,
        // Reserved — never constructed (kept for ABI compat).
        CloseFactorExceeded,
        // Reserved — never constructed (kept for ABI compat).
        CollateralAwardExceedsPosition,

        // ── Params / timelock ──
        InvalidRatio,
        InvalidParam,
        NoPendingMarketParamsUpdate,
        NoPendingAlphaParamsUpdate,
        NoPendingGlobalParamsUpdate,
        ParamsUpdateTimelockActive,

        // ── Cross-contract ──
        TokenContractCallFailed,
        TokenTransferFromFailed,
        LTokenCallFailed,
        TransferFailed,

        // ── General ──
        MarketNotFound,
        PositionNotFound,
        ArithmeticError,
    }

    /// Result type alias used across the lending pool, with [`Error`] as the
    /// error type.
    pub type Result<T> = core::result::Result<T, Error>;

    impl TusdtLendingPool {
        /// Creates a new lending pool.
        ///
        /// Spawns two lToken children (lTAO and lTUSDT) using the provided `ltoken_code_hash`
        /// (which should be the `tusdt-erc20` code hash). The deployer becomes the initial
        /// governance, platform, and maintainer.
        #[ink(constructor)]
        pub fn new(
            treasury: AccountId,
            tusdt_address: AccountId,
            oracle_address: AccountId,
            ltoken_code_hash: Hash,
            pool_hotkey: AccountId,
        ) -> Self {
            let caller = Self::env().caller();
            let contract_account = Self::env().account_id();
            let now = Self::env().block_timestamp();

            // Spawn lTAO (market 0) and lTUSDT (market 1) as child tusdt-erc20 instances.
            let ltao = TusdtErc20Ref::new(contract_account)
                .code_hash(ltoken_code_hash)
                .endowment(0)
                .salt_bytes([3u8; 32])
                .instantiate();
            let ltusdt = TusdtErc20Ref::new(contract_account)
                .code_hash(ltoken_code_hash)
                .endowment(0)
                .salt_bytes([4u8; 32])
                .instantiate();

            let mut markets = Mapping::default();
            markets.insert(0, &MarketState::new(now));
            markets.insert(1, &MarketState::new(now));

            let mut ltoken_by_market = Mapping::default();
            ltoken_by_market.insert(0, &ltao.to_account_id());
            ltoken_by_market.insert(1, &ltusdt.to_account_id());

            let mut market_keys = StorageVec::new();
            market_keys.push(&0);
            market_keys.push(&1);

            let mut market_params = Mapping::default();
            market_params.insert(0, &default_tao_interest_params());
            market_params.insert(1, &default_tusdt_interest_params());

            Self {
                governance: caller,
                maintainer: caller,
                treasury,
                platform: caller,
                paused: false,
                busy: false,
                oracle: TusdtOracleRef::from_account_id(oracle_address),
                tusdt: TusdtErc20Ref::from_account_id(tusdt_address),
                pool_hotkey,
                markets,
                market_keys,
                ltoken_by_market,
                netuid_to_market: Mapping::default(),
                market_to_netuid: Mapping::default(),
                next_alpha_market_id: 2,
                market_params,
                alpha_params: Mapping::default(),
                pending_market_params: Mapping::default(),
                pending_alpha_params: Mapping::default(),
                global_params: default_global_params(),
                pending_global_params: None,
                netuid_total_collateral: Mapping::default(),
                positions: Mapping::default(),
                position_keys: StorageVec::new(),
                debt_principal: Mapping::default(),
                market_deficit: Mapping::default(),
                root_hotkey: pool_hotkey,
                staked_tao: 0,
                stake_buffer: DEFAULT_STAKE_BUFFER,
                sweep_threshold: 0,
                stake_floor: MIN_ROOT_STAKE_FLOOR,
                last_sweep_block: None,
                staking_enabled: false,
            }
        }

        /// Upgrade constructor: deploys a new pool reusing existing lToken children.
        #[ink(constructor)]
        pub fn new_upgrade(
            treasury: AccountId,
            tusdt_address: AccountId,
            oracle_address: AccountId,
            ltao_address: AccountId,
            ltusdt_address: AccountId,
            pool_hotkey: AccountId,
        ) -> Self {
            let caller = Self::env().caller();
            let now = Self::env().block_timestamp();

            let mut markets = Mapping::default();
            markets.insert(0, &MarketState::new(now));
            markets.insert(1, &MarketState::new(now));

            let mut ltoken_by_market = Mapping::default();
            ltoken_by_market.insert(0, &ltao_address);
            ltoken_by_market.insert(1, &ltusdt_address);

            let mut market_keys = StorageVec::new();
            market_keys.push(&0);
            market_keys.push(&1);

            let mut market_params = Mapping::default();
            market_params.insert(0, &default_tao_interest_params());
            market_params.insert(1, &default_tusdt_interest_params());

            Self {
                governance: caller,
                maintainer: caller,
                treasury,
                platform: caller,
                paused: false,
                busy: false,
                oracle: TusdtOracleRef::from_account_id(oracle_address),
                tusdt: TusdtErc20Ref::from_account_id(tusdt_address),
                pool_hotkey,
                markets,
                market_keys,
                ltoken_by_market,
                netuid_to_market: Mapping::default(),
                market_to_netuid: Mapping::default(),
                next_alpha_market_id: 2,
                market_params,
                alpha_params: Mapping::default(),
                pending_market_params: Mapping::default(),
                pending_alpha_params: Mapping::default(),
                global_params: default_global_params(),
                pending_global_params: None,
                netuid_total_collateral: Mapping::default(),
                positions: Mapping::default(),
                position_keys: StorageVec::new(),
                debt_principal: Mapping::default(),
                market_deficit: Mapping::default(),
                root_hotkey: pool_hotkey,
                staked_tao: 0,
                stake_buffer: DEFAULT_STAKE_BUFFER,
                sweep_threshold: 0,
                stake_floor: MIN_ROOT_STAKE_FLOOR,
                last_sweep_block: None,
                staking_enabled: false,
            }
        }
    }

    impl TusdtLendingPool {
        // ─────────────────────────────────────────────────────────────
        // Access control
        // ─────────────────────────────────────────────────────────────

        /// Acquires the reentrancy guard. Returns `Error::Reentrancy` if the guard
        /// is already held; sets `busy = true` on success.
        pub(crate) fn ensure_idle(&mut self) -> Result<()> {
            if self.busy {
                return Err(Error::Reentrancy);
            }
            self.busy = true;
            Ok(())
        }

        /// Releases the reentrancy guard (`busy = false`). Must be called on every
        /// path after `ensure_idle`, including error paths.
        pub(crate) fn set_idle(&mut self) {
            self.busy = false;
        }

        // ─────────────────────────────────────────────────────────────
        // Position key lifecycle
        // ─────────────────────────────────────────────────────────────

        /// Remove all occurrences of a key from `position_keys` by scanning
        /// backward. On each match we swap the tail element into the match
        /// slot and pop, so the operation is O(n) but only fires when a
        /// position becomes all-zeros (rare).
        pub(crate) fn remove_position_key(&mut self, key: &(u8, AccountId)) {
            let mut i = self.position_keys.len();
            while i > 0 {
                i = i.saturating_sub(1);
                if self.position_keys.get(i) == Some(*key) {
                    let last = self.position_keys.len().saturating_sub(1);
                    if i != last {
                        if let Some(tail) = self.position_keys.get(last) {
                            self.position_keys.set(i, &tail);
                        }
                    }
                    self.position_keys.pop();
                }
            }
        }

        /// Maintains the `position_keys` invariant after any position mutation:
        /// `position_keys` holds exactly one entry per non-zero position.
        ///
        /// * If the position is now non-zero → ensure the key is present
        ///   (pushes if absent; self-heals legacy duplicates by not pushing
        ///   again when already present).
        /// * If the position is now all-zeros → remove **all** occurrences
        ///   of the key (self-heals legacy duplicates).
        pub(crate) fn update_position_key(&mut self, market_id: u8, user: AccountId) {
            let key = (market_id, user);
            let pos = self.positions.get(key).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            let is_zero =
                pos.ltoken_balance == 0 && pos.scaled_debt == 0 && pos.alpha_principal == 0;

            if is_zero {
                self.remove_position_key(&key);
            } else {
                // Ensure key is present exactly once — scan first so legacy
                // duplicates never cause a second push (self-healing).
                let mut present = false;
                for i in 0..self.position_keys.len() {
                    if self.position_keys.get(i) == Some(key) {
                        present = true;
                        break;
                    }
                }
                if !present {
                    self.position_keys.push(&key);
                }
            }
        }

        /// Reverts with `Error::NotGovernance` unless the caller is the governance
        /// account.
        pub(crate) fn ensure_governance(&self) -> Result<()> {
            if self.env().caller() != self.governance {
                return Err(Error::NotGovernance);
            }
            Ok(())
        }

        /// Reverts with `Error::NotGovernanceOrPlatform` unless the caller is the
        /// governance or the platform account.
        pub(crate) fn ensure_governance_or_platform(&self) -> Result<()> {
            let caller = self.env().caller();
            if caller != self.governance && caller != self.platform {
                return Err(Error::NotGovernanceOrPlatform);
            }
            Ok(())
        }

        /// Reverts with `Error::NotMaintainer` unless the caller is the maintainer
        /// or the governance account.
        pub(crate) fn ensure_maintainer(&self) -> Result<()> {
            let caller = self.env().caller();
            if caller != self.maintainer && caller != self.governance {
                return Err(Error::NotMaintainer);
            }
            Ok(())
        }

        /// Reverts with `Error::ContractPaused` when the pool is paused.
        pub(crate) fn ensure_not_paused(&self) -> Result<()> {
            if self.paused {
                return Err(Error::ContractPaused);
            }
            Ok(())
        }

        /// Reverts with `Error::UnapprovedNetuid` unless the netuid is an approved
        /// alpha market.
        pub(crate) fn ensure_approved_netuid(&self, netuid: u16) -> Result<()> {
            if self.netuid_to_market.get(netuid).is_none() {
                return Err(Error::UnapprovedNetuid);
            }
            Ok(())
        }

        // ─────────────────────────────────────────────────────────────
        // User-facing messages: supply / withdraw
        // ─────────────────────────────────────────────────────────────

        /// Supplies TAO into the pool. Caller must send native TAO via `transferred_value`.
        /// Mints lTAO receipt tokens at the current exchange rate.
        #[ink(message, payable)]
        pub fn supply_tao(&mut self, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;
            if amount == 0 {
                return Err(Error::ZeroAmount);
            }
            let received = self.env().transferred_value();
            if received < amount {
                return Err(Error::TransferFailed);
            }

            self.ensure_idle()?;
            // Guard is locked; every error path below MUST call set_idle() before returning.

            // Accrue interest before any state change
            self.accrue_interest(0).inspect_err(|_| {
                self.set_idle();
            })?;

            // Cap check. `total_supplied` is tracked in lToken (scaled)
            // units; project the face value at the current exchange rate so
            // the cap keeps its face-amount semantics.
            if self.global_params.supply_cap_tao > 0 {
                let state = self.markets.get(0).ok_or(Error::MarketNotFound)?;
                let face_supplied = state
                    .exchange_rate
                    .checked_mul_value(state.total_supplied.into())
                    .and_then(|v| Balance::try_from(v).ok())
                    .ok_or(Error::ArithmeticError)?;
                let projected = face_supplied.checked_add(amount).ok_or(Error::ArithmeticError)?;
                if projected > self.global_params.supply_cap_tao {
                    self.set_idle();
                    return Err(Error::SupplyCapExceeded);
                }
            }

            let state = self.markets.get(0).ok_or(Error::MarketNotFound)?;
            let ltoken_addr = self.ltoken_by_market.get(0).ok_or(Error::MarketNotFound)?;
            let mut ltoken = TusdtErc20Ref::from_account_id(ltoken_addr);

            // Compute lToken amount: amount / exchange_rate (1:1 at genesis).
            // The exchange rate grows with accrued supply interest, so a
            // deposit at a grown rate mints proportionally fewer lTokens and
            // interest accrued before the deposit stays with earlier
            // suppliers.
            let ltoken_supply = ltoken.total_supply();
            let ltoken_scaled = if ltoken_supply == 0 || state.total_supplied == 0 {
                amount
            } else {
                compute_mint_amount(amount, state.exchange_rate).ok_or(Error::ArithmeticError)?
            };
            if ltoken_scaled == 0 {
                self.set_idle();
                return Err(Error::MintBelowPrecision);
            }

            // Effects: update market state and position
            let caller = self.env().caller();
            let mut state = self.markets.get(0).ok_or(Error::MarketNotFound)?;
            if state.total_supplied == 0 {
                // Genesis supply on a fully drained market: restart the
                // exchange rate at 1.0 so the 1:1 genesis mint is backed 1:1.
                // A stale grown rate would over-credit the new supplier.
                state.exchange_rate = Ratio::one();
            }
            state.total_supplied =
                state.total_supplied.checked_add(ltoken_scaled).ok_or(Error::ArithmeticError)?;
            self.markets.insert(0, &state);

            let mut pos = self.positions.get((0, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            pos.ltoken_balance =
                pos.ltoken_balance.checked_add(ltoken_scaled).ok_or(Error::ArithmeticError)?;
            self.positions.insert((0, caller), &pos);
            self.update_position_key(0, caller);

            // Mint lTokens
            ltoken.mint(caller, ltoken_scaled).map_err(|_| Error::LTokenCallFailed)?;

            // Refund excess native transfer
            if received > amount {
                let excess = received.checked_sub(amount).ok_or(Error::ArithmeticError)?;
                self.env().transfer(caller, excess).map_err(|_| Error::TransferFailed)?;
            }

            self.env().emit_event(LiquidDeposited {
                market: 0,
                user: caller,
                amount,
                ltoken_scaled,
            });

            // Best-effort: sweep excess idle TAO to the root subnet. Never reverts
            // the supply itself.
            let _ = self.sweep_to_root();

            self.set_idle();
            Ok(())
        }

        /// Supplies TUSDT into the pool. Pulls TUSDT from caller via `transfer_from`.
        /// Mints lTUSDT receipt tokens at the current exchange rate.
        #[ink(message)]
        pub fn supply_tusdt(&mut self, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;
            if amount == 0 {
                return Err(Error::ZeroAmount);
            }

            self.ensure_idle()?;
            self.accrue_interest(1).inspect_err(|_| {
                self.set_idle();
            })?;

            // Cap check. `total_supplied` is tracked in lToken (scaled)
            // units; project the face value at the current exchange rate so
            // the cap keeps its face-amount semantics.
            if self.global_params.supply_cap_tusdt > 0 {
                let state = self.markets.get(1).ok_or(Error::MarketNotFound)?;
                let face_supplied = state
                    .exchange_rate
                    .checked_mul_value(state.total_supplied.into())
                    .and_then(|v| Balance::try_from(v).ok())
                    .ok_or(Error::ArithmeticError)?;
                let projected = face_supplied.checked_add(amount).ok_or(Error::ArithmeticError)?;
                if projected > self.global_params.supply_cap_tusdt {
                    self.set_idle();
                    return Err(Error::SupplyCapExceeded);
                }
            }

            let caller = self.env().caller();
            let pool_addr = self.env().account_id();

            // Pull TUSDT from caller
            self.tusdt
                .transfer_from(caller, pool_addr, amount)
                .map_err(|_| Error::TokenTransferFromFailed)?;

            let state = self.markets.get(1).ok_or(Error::MarketNotFound)?;
            let ltoken_addr = self.ltoken_by_market.get(1).ok_or(Error::MarketNotFound)?;
            let mut ltoken = TusdtErc20Ref::from_account_id(ltoken_addr);

            // Compute lToken amount: amount / exchange_rate (1:1 at genesis).
            // The exchange rate grows with accrued supply interest, so a
            // deposit at a grown rate mints proportionally fewer lTokens and
            // interest accrued before the deposit stays with earlier
            // suppliers.
            let ltoken_supply = ltoken.total_supply();
            let ltoken_scaled = if ltoken_supply == 0 || state.total_supplied == 0 {
                amount
            } else {
                compute_mint_amount(amount, state.exchange_rate).ok_or(Error::ArithmeticError)?
            };
            if ltoken_scaled == 0 {
                self.set_idle();
                return Err(Error::MintBelowPrecision);
            }

            // Effects
            let mut state = self.markets.get(1).ok_or(Error::MarketNotFound)?;
            if state.total_supplied == 0 {
                // Genesis supply on a fully drained market: restart the
                // exchange rate at 1.0 so the 1:1 genesis mint is backed 1:1.
                // A stale grown rate would over-credit the new supplier.
                state.exchange_rate = Ratio::one();
            }
            state.total_supplied =
                state.total_supplied.checked_add(ltoken_scaled).ok_or(Error::ArithmeticError)?;
            self.markets.insert(1, &state);

            let mut pos = self.positions.get((1, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            pos.ltoken_balance =
                pos.ltoken_balance.checked_add(ltoken_scaled).ok_or(Error::ArithmeticError)?;
            self.positions.insert((1, caller), &pos);
            self.update_position_key(1, caller);

            // Mint lTokens
            ltoken.mint(caller, ltoken_scaled).map_err(|_| Error::LTokenCallFailed)?;

            self.env().emit_event(LiquidDeposited {
                market: 1,
                user: caller,
                amount,
                ltoken_scaled,
            });

            self.set_idle();
            Ok(())
        }

        /// Withdraws TAO by burning lTAO tokens. Transfers native TAO to caller.
        #[ink(message)]
        pub fn withdraw_tao(&mut self, ltoken_amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;
            if ltoken_amount == 0 {
                return Err(Error::ZeroAmount);
            }

            self.ensure_idle()?;

            self.accrue_interest(0).inspect_err(|_| {
                self.set_idle();
            })?;

            let caller = self.env().caller();
            let state = self.markets.get(0).ok_or(Error::MarketNotFound)?;
            let ltoken_addr = self.ltoken_by_market.get(0).ok_or(Error::MarketNotFound)?;
            let mut ltoken = TusdtErc20Ref::from_account_id(ltoken_addr);

            // Check position
            let pos = self.positions.get((0, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            if pos.ltoken_balance < ltoken_amount {
                self.set_idle();
                return Err(Error::InsufficientLTokenBalance);
            }

            // Compute underlying amount: ltoken_amount × exchange_rate. The
            // exchange rate grew with accrued supply interest, so this
            // includes the supplier's proportional share of borrower interest
            // (net of the reserve slice) — not just the principal.
            let underlying = if state.total_supplied == 0 {
                0
            } else {
                compute_redeem_amount(ltoken_amount, state.exchange_rate)
                    .ok_or(Error::ArithmeticError)?
            };
            if underlying == 0 {
                self.set_idle();
                return Err(Error::MintBelowPrecision);
            }

            // Liquidity check
            let cash = self.market_cash(0)?;
            if underlying > cash {
                self.set_idle();
                return Err(Error::LiquidityInsufficient);
            }

            // Top up the free sleeve from root stake if the free balance alone cannot
            // cover the withdrawal (root unstake is synchronous and 1:1).
            let free = self.env().balance();
            if underlying > free {
                let shortfall = underlying.checked_sub(free).ok_or(Error::ArithmeticError)?;
                let received = self.top_up_free(shortfall)?;
                if received < shortfall {
                    self.set_idle();
                    return Err(Error::LiquidityInsufficient);
                }
            }

            // Note: TAO supply is NOT collateral, so no health check is needed here.
            // Only alpha collateral positions affect borrow health.

            // Effects: burn lTokens, update market state
            ltoken.burn(caller, ltoken_amount).map_err(|_| Error::LTokenCallFailed)?;

            let mut state = self.markets.get(0).ok_or(Error::MarketNotFound)?;
            state.total_supplied =
                state.total_supplied.checked_sub(ltoken_amount).ok_or(Error::ArithmeticError)?;
            reset_exchange_rate_when_drained(&mut state);
            self.markets.insert(0, &state);

            let mut pos = self.positions.get((0, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            pos.ltoken_balance =
                pos.ltoken_balance.checked_sub(ltoken_amount).ok_or(Error::ArithmeticError)?;
            self.positions.insert((0, caller), &pos);
            self.update_position_key(0, caller);

            // Transfer TAO
            self.env().transfer(caller, underlying).map_err(|_| Error::TransferFailed)?;

            self.env().emit_event(LiquidWithdrawn {
                market: 0,
                user: caller,
                amount: underlying,
                ltoken_scaled: ltoken_amount,
            });

            self.set_idle();
            Ok(())
        }

        /// Withdraws TUSDT by burning lTUSDT tokens. Transfers TUSDT to caller.
        #[ink(message)]
        pub fn withdraw_tusdt(&mut self, ltoken_amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;
            if ltoken_amount == 0 {
                return Err(Error::ZeroAmount);
            }

            self.ensure_idle()?;
            self.accrue_interest(1).inspect_err(|_| {
                self.set_idle();
            })?;

            let caller = self.env().caller();
            let state = self.markets.get(1).ok_or(Error::MarketNotFound)?;
            let ltoken_addr = self.ltoken_by_market.get(1).ok_or(Error::MarketNotFound)?;
            let mut ltoken = TusdtErc20Ref::from_account_id(ltoken_addr);

            let pos = self.positions.get((1, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            if pos.ltoken_balance < ltoken_amount {
                self.set_idle();
                return Err(Error::InsufficientLTokenBalance);
            }

            // Compute underlying amount: ltoken_amount × exchange_rate. The
            // exchange rate grew with accrued supply interest, so this
            // includes the supplier's proportional share of borrower interest
            // (net of the reserve slice) — not just the principal.
            let underlying = if state.total_supplied == 0 {
                0
            } else {
                compute_redeem_amount(ltoken_amount, state.exchange_rate)
                    .ok_or(Error::ArithmeticError)?
            };
            if underlying == 0 {
                self.set_idle();
                return Err(Error::MintBelowPrecision);
            }

            let cash = self.market_cash(1)?;
            if underlying > cash {
                self.set_idle();
                return Err(Error::LiquidityInsufficient);
            }

            // Effects
            ltoken.burn(caller, ltoken_amount).map_err(|_| Error::LTokenCallFailed)?;

            let mut state = self.markets.get(1).ok_or(Error::MarketNotFound)?;
            state.total_supplied =
                state.total_supplied.checked_sub(ltoken_amount).ok_or(Error::ArithmeticError)?;
            reset_exchange_rate_when_drained(&mut state);
            self.markets.insert(1, &state);

            let mut pos = self.positions.get((1, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            pos.ltoken_balance =
                pos.ltoken_balance.checked_sub(ltoken_amount).ok_or(Error::ArithmeticError)?;
            self.positions.insert((1, caller), &pos);
            self.update_position_key(1, caller);

            self.tusdt.transfer(caller, underlying).map_err(|_| Error::TokenContractCallFailed)?;

            self.env().emit_event(LiquidWithdrawn {
                market: 1,
                user: caller,
                amount: underlying,
                ltoken_scaled: ltoken_amount,
            });

            self.set_idle();
            Ok(())
        }

        // ─────────────────────────────────────────────────────────────
        // User-facing messages: borrow / repay
        // ─────────────────────────────────────────────────────────────

        /// Borrows TAO against the caller's alpha collateral.
        ///
        /// One hour of interest is charged up front: the position's debt
        /// exceeds `amount` from this block onward, and the next increase
        /// arrives with the market's next hourly accrual. Repaying sooner than
        /// an hour does not refund that first hour.
        #[ink(message)]
        pub fn borrow_tao(&mut self, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;
            if amount == 0 {
                return Err(Error::ZeroAmount);
            }

            self.ensure_idle()?;
            self.accrue_interest(0).inspect_err(|_| {
                self.set_idle();
            })?;

            let caller = self.env().caller();

            // Cap check
            if self.global_params.borrow_cap_tao > 0 {
                let state = self.markets.get(0).ok_or(Error::MarketNotFound)?;
                let projected =
                    state.total_debt.checked_add(amount).ok_or(Error::ArithmeticError)?;
                if projected > self.global_params.borrow_cap_tao {
                    self.set_idle();
                    return Err(Error::BorrowCapExceeded);
                }
            }

            // Liquidity check
            let cash = self.market_cash(0)?;
            if amount > cash {
                self.set_idle();
                return Err(Error::LiquidityInsufficient);
            }

            // Top up the free sleeve from root stake if the free balance alone cannot
            // cover the borrow (root unstake is synchronous and 1:1).
            let free = self.env().balance();
            if amount > free {
                let shortfall = amount.checked_sub(free).ok_or(Error::ArithmeticError)?;
                let received = self.top_up_free(shortfall)?;
                if received < shortfall {
                    self.set_idle();
                    return Err(Error::LiquidityInsufficient);
                }
            }

            // Health check: convert borrow amount to TUSDT equivalent
            let tusdt_per_tao = self.get_oracle_price().inspect_err(|_| {
                self.set_idle();
            })?;
            let borrow_value_tusdt = tusdt_per_tao
                .checked_mul_value(amount.into())
                .and_then(|v| Balance::try_from(v).ok())
                .ok_or(Error::ArithmeticError)?;

            let available = self.get_available_borrow_tusdt(caller)?;
            if borrow_value_tusdt > available {
                self.set_idle();
                return Err(Error::BorrowHealthExceeded);
            }

            // Effects
            let mut state = self.markets.get(0).ok_or(Error::MarketNotFound)?;
            // Charge the first hour of interest up front (hour-beginning
            // charging): the premium is booked into the position's debt now, so
            // the borrower owes interest from this instant and their next
            // increase arrives with the market's next hourly accrual.
            let premium = self.charge_prepaid_hour(0, &mut state, amount).inspect_err(|_| {
                self.set_idle();
            })?;
            let debt_booked = amount.checked_add(premium).ok_or(Error::ArithmeticError)?;
            // scaled = ceil(debt_booked / borrow_index). Rounding UP (Aave rayDivUp)
            // keeps the borrower's debt >= the amount they receive, so the
            // market total can never drift above the sum of user positions
            // (no ghost debt) and a later full repayment clears the position
            // exactly. checked_div_value_ceil(value) computes value / self
            // rounded up, so the Ratio must be the borrow_index (divisor).
            let scaled = state
                .borrow_index
                .checked_div_value_ceil(debt_booked.into())
                .and_then(|v| Balance::try_from(v).ok())
                .ok_or(Error::ArithmeticError)?;

            // Lockstep scaled accounting: the market total gains the SAME
            // scaled units as the position (Aave: total and user shares move
            // together), so the ledger can never drift. The face total is
            // derived from the scaled total for consumers (utilization, caps).
            state.total_scaled_debt =
                state.total_scaled_debt.checked_add(scaled).ok_or(Error::ArithmeticError)?;
            state.total_debt = scaled_debt_to_face(state.total_scaled_debt, state.borrow_index)
                .ok_or(Error::ArithmeticError)?;
            self.markets.insert(0, &state);

            let mut pos = self.positions.get((0, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            pos.scaled_debt = pos.scaled_debt.checked_add(scaled).ok_or(Error::ArithmeticError)?;
            let principal = self.debt_principal.get((0, caller)).unwrap_or(0);
            self.debt_principal
                .insert((0, caller), &principal.checked_add(amount).ok_or(Error::ArithmeticError)?);
            self.positions.insert((0, caller), &pos);
            self.update_position_key(0, caller);

            // Transfer TAO to borrower
            self.env().transfer(caller, amount).map_err(|_| Error::TransferFailed)?;

            self.env().emit_event(Borrowed { market: 0, user: caller, amount });

            self.set_idle();
            Ok(())
        }

        /// Borrows TUSDT against the caller's alpha collateral.
        ///
        /// One hour of interest is charged up front: the position's debt
        /// exceeds `amount` from this block onward, and the next increase
        /// arrives with the market's next hourly accrual. Repaying sooner than
        /// an hour does not refund that first hour.
        #[ink(message)]
        pub fn borrow_tusdt(&mut self, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;
            if amount == 0 {
                return Err(Error::ZeroAmount);
            }

            self.ensure_idle()?;
            self.accrue_interest(1).inspect_err(|_| {
                self.set_idle();
            })?;

            let caller = self.env().caller();

            // Cap check
            if self.global_params.borrow_cap_tusdt > 0 {
                let state = self.markets.get(1).ok_or(Error::MarketNotFound)?;
                let projected =
                    state.total_debt.checked_add(amount).ok_or(Error::ArithmeticError)?;
                if projected > self.global_params.borrow_cap_tusdt {
                    self.set_idle();
                    return Err(Error::BorrowCapExceeded);
                }
            }

            // Liquidity check
            let cash = self.market_cash(1)?;
            if amount > cash {
                self.set_idle();
                return Err(Error::LiquidityInsufficient);
            }

            // Health check
            let available = self.get_available_borrow_tusdt(caller)?;
            if amount > available {
                self.set_idle();
                return Err(Error::BorrowHealthExceeded);
            }

            // Effects
            let mut state = self.markets.get(1).ok_or(Error::MarketNotFound)?;
            // Charge the first hour of interest up front (see borrow_tao).
            let premium = self.charge_prepaid_hour(1, &mut state, amount).inspect_err(|_| {
                self.set_idle();
            })?;
            let debt_booked = amount.checked_add(premium).ok_or(Error::ArithmeticError)?;
            // scaled = ceil(debt_booked / borrow_index). Rounding UP (Aave rayDivUp)
            // keeps the borrower's debt >= the amount they receive, so the
            // market total can never drift above the sum of user positions
            // (no ghost debt) and a later full repayment clears the position
            // exactly. checked_div_value_ceil(value) computes value / self
            // rounded up, so the Ratio must be the borrow_index (divisor).
            let scaled = state
                .borrow_index
                .checked_div_value_ceil(debt_booked.into())
                .and_then(|v| Balance::try_from(v).ok())
                .ok_or(Error::ArithmeticError)?;

            // Lockstep scaled accounting (see borrow_tao): the market total
            // gains the SAME scaled units as the position.
            state.total_scaled_debt =
                state.total_scaled_debt.checked_add(scaled).ok_or(Error::ArithmeticError)?;
            state.total_debt = scaled_debt_to_face(state.total_scaled_debt, state.borrow_index)
                .ok_or(Error::ArithmeticError)?;
            self.markets.insert(1, &state);

            let mut pos = self.positions.get((1, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            pos.scaled_debt = pos.scaled_debt.checked_add(scaled).ok_or(Error::ArithmeticError)?;
            let principal = self.debt_principal.get((1, caller)).unwrap_or(0);
            self.debt_principal
                .insert((1, caller), &principal.checked_add(amount).ok_or(Error::ArithmeticError)?);
            self.positions.insert((1, caller), &pos);
            self.update_position_key(1, caller);

            // Transfer TUSDT to borrower
            self.tusdt.transfer(caller, amount).map_err(|_| Error::TokenContractCallFailed)?;

            self.env().emit_event(Borrowed { market: 1, user: caller, amount });

            self.set_idle();
            Ok(())
        }

        /// Repays TAO debt. Caller sends native TAO via `transferred_value`.
        #[ink(message, payable)]
        pub fn repay_tao(&mut self, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;

            self.ensure_idle()?;
            self.accrue_interest(0).inspect_err(|_| {
                self.set_idle();
            })?;

            let caller = self.env().caller();
            let state = self.markets.get(0).ok_or(Error::MarketNotFound)?;

            // Compute actual debt
            let pos = self.positions.get((0, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            let debt = if pos.scaled_debt == 0 {
                0
            } else {
                state
                    .borrow_index
                    .checked_mul_value(pos.scaled_debt.into())
                    .and_then(|v| Balance::try_from(v).ok())
                    .unwrap_or(0)
            };

            let repay_amount = min(amount, debt);
            if repay_amount == 0 {
                self.set_idle();
                return Ok(());
            }

            // scaled_repaid = repay_amount / borrow_index, rounded UP (Aave
            // rayDivUp). A full repayment clears the position exactly, since
            // floor division would otherwise strand the last sub-index unit of
            // debt forever; a partial repayment must never erase the position
            // through rounding. Clamped to the position so the subtraction can
            // never underflow.
            let scaled_repaid = if repay_amount >= debt {
                pos.scaled_debt
            } else {
                min(
                    state
                        .borrow_index
                        .checked_div_value_ceil(repay_amount.into())
                        .and_then(|v| Balance::try_from(v).ok())
                        .ok_or(Error::ArithmeticError)?,
                    pos.scaled_debt,
                )
            };
            // Defensive guard — unreachable under ceil scaling: any positive
            // repayment credits at least one scaled unit (checked_div_value_ceil).
            if scaled_repaid == 0 {
                self.set_idle();
                return Err(Error::ZeroAmount);
            }

            // Verify payment
            let received = self.env().transferred_value();
            if received < repay_amount {
                self.set_idle();
                return Err(Error::TransferFailed);
            }
            if received > repay_amount {
                let excess = received.checked_sub(repay_amount).ok_or(Error::ArithmeticError)?;
                self.env().transfer(caller, excess).map_err(|_| Error::TransferFailed)?;
            }

            // Effects
            let mut state = state;
            // Lockstep scaled accounting (see borrow_tao): the market total
            // loses the SAME scaled units as the position, so a full repayment
            // drives both to exactly zero.
            state.total_scaled_debt =
                state.total_scaled_debt.checked_sub(scaled_repaid).ok_or(Error::ArithmeticError)?;
            state.total_debt = scaled_debt_to_face(state.total_scaled_debt, state.borrow_index)
                .ok_or(Error::ArithmeticError)?;
            self.markets.insert(0, &state);

            let mut pos = pos;
            pos.scaled_debt =
                pos.scaled_debt.checked_sub(scaled_repaid).ok_or(Error::ArithmeticError)?;
            let principal = self.debt_principal.get((0, caller)).unwrap_or(0);
            self.debt_principal.insert((0, caller), &principal.saturating_sub(repay_amount));
            self.positions.insert((0, caller), &pos);
            self.update_position_key(0, caller);

            // Note: repaid TAO stays in pool as cash — no burn.

            self.env().emit_event(Repaid { market: 0, user: caller, amount: repay_amount });

            // Best-effort: sweep excess idle TAO to the root subnet. Never reverts
            // the repayment itself.
            let _ = self.sweep_to_root();

            self.set_idle();
            Ok(())
        }

        /// Repays TUSDT debt. Pulls TUSDT from caller via transfer_from.
        #[ink(message)]
        pub fn repay_tusdt(&mut self, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;

            self.ensure_idle()?;
            self.accrue_interest(1).inspect_err(|_| {
                self.set_idle();
            })?;

            let caller = self.env().caller();
            let pool_addr = self.env().account_id();
            let state = self.markets.get(1).ok_or(Error::MarketNotFound)?;

            let pos = self.positions.get((1, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            let debt = if pos.scaled_debt == 0 {
                0
            } else {
                state
                    .borrow_index
                    .checked_mul_value(pos.scaled_debt.into())
                    .and_then(|v| Balance::try_from(v).ok())
                    .unwrap_or(0)
            };

            let repay_amount = min(amount, debt);
            if repay_amount == 0 {
                self.set_idle();
                return Ok(());
            }

            // scaled_repaid = repay_amount / borrow_index, rounded UP (Aave
            // rayDivUp). A full repayment clears the position exactly, since
            // floor division would otherwise strand the last sub-index unit of
            // debt forever; a partial repayment must never erase the position
            // through rounding. Clamped to the position so the subtraction can
            // never underflow.
            let scaled_repaid = if repay_amount >= debt {
                pos.scaled_debt
            } else {
                min(
                    state
                        .borrow_index
                        .checked_div_value_ceil(repay_amount.into())
                        .and_then(|v| Balance::try_from(v).ok())
                        .ok_or(Error::ArithmeticError)?,
                    pos.scaled_debt,
                )
            };
            // Defensive guard — unreachable under ceil scaling: any positive
            // repayment credits at least one scaled unit (checked_div_value_ceil).
            if scaled_repaid == 0 {
                self.set_idle();
                return Err(Error::ZeroAmount);
            }

            // Pull TUSDT from caller
            self.tusdt
                .transfer_from(caller, pool_addr, repay_amount)
                .map_err(|_| Error::TokenTransferFromFailed)?;

            // Effects
            let mut state = state;
            // Lockstep scaled accounting (see borrow_tao): the market total
            // loses the SAME scaled units as the position, so a full repayment
            // drives both to exactly zero.
            state.total_scaled_debt =
                state.total_scaled_debt.checked_sub(scaled_repaid).ok_or(Error::ArithmeticError)?;
            state.total_debt = scaled_debt_to_face(state.total_scaled_debt, state.borrow_index)
                .ok_or(Error::ArithmeticError)?;
            self.markets.insert(1, &state);

            let mut pos = pos;
            pos.scaled_debt =
                pos.scaled_debt.checked_sub(scaled_repaid).ok_or(Error::ArithmeticError)?;
            let principal = self.debt_principal.get((1, caller)).unwrap_or(0);
            self.debt_principal.insert((1, caller), &principal.saturating_sub(repay_amount));
            self.positions.insert((1, caller), &pos);
            self.update_position_key(1, caller);

            self.env().emit_event(Repaid { market: 1, user: caller, amount: repay_amount });

            self.set_idle();
            Ok(())
        }

        /// Accrues interest for both debt markets (0 = TAO, 1 = TUSDT) in a
        /// single call. Permissionless — anyone may call it to refresh each
        /// market's borrow index, exchange rate, and reserve against the time
        /// elapsed since the last accrual, so off-chain debt calculations stay
        /// in sync with the chain. No interest accrues for a market with no
        /// debt or with less than one full hour elapsed. A debt market
        /// advances its last-update timestamp by whole hours only (the
        /// sub-hour remainder is preserved); a debt-free market tracks the
        /// latest write.
        #[ink(message)]
        pub fn accrue_market_interest(&mut self) -> Result<()> {
            self.ensure_not_paused()?;

            self.ensure_idle()?;
            self.accrue_interest(0).inspect_err(|_| {
                self.set_idle();
            })?;
            self.accrue_interest(1).inspect_err(|_| {
                self.set_idle();
            })?;

            self.set_idle();
            Ok(())
        }

        // ─────────────────────────────────────────────────────────────
        // Alpha collateral: deposit / withdraw
        // ─────────────────────────────────────────────────────────────

        /// Deposits alpha stake as collateral. Uses chain extension func 25 to atomically
        /// pull the caller's stake from under `pool_hotkey` to the pool contract.
        #[ink(message)]
        pub fn deposit_alpha(&mut self, netuid: u16, amount: Balance) -> Result<u8> {
            // All validation checks FIRST (before reentrancy guard and chain extension)
            self.ensure_not_paused()?;
            if amount == 0 {
                return Err(Error::ZeroAmount);
            }
            self.ensure_approved_netuid(netuid)?;

            // Supply cap check BEFORE the chain extension call (CEI: check, then external call, then effects)
            let params = self.alpha_params.get(netuid).unwrap_or(default_alpha_params());
            if params.supply_cap > 0 {
                let current = self.netuid_total_collateral.get(netuid).unwrap_or_default();
                let projected = current.checked_add(amount).ok_or(Error::ArithmeticError)?;
                if projected > params.supply_cap {
                    return Err(Error::SupplyCapExceeded);
                }
            }

            self.ensure_idle()?;

            // Atomic pull via caller_transfer_stake (func 25)
            self.env()
                .extension()
                .caller_transfer_stake(
                    self.env().account_id(),
                    self.pool_hotkey,
                    netuid,
                    netuid,
                    amount,
                )
                .map_err(|_| {
                    self.set_idle();
                    Error::StakeTransferFailed
                })?;

            let caller = self.env().caller();
            let market_id = self.netuid_to_market.get(netuid).ok_or(Error::UnapprovedNetuid)?;

            // Effects
            let mut pos = self.positions.get((market_id, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            pos.alpha_principal =
                pos.alpha_principal.checked_add(amount).ok_or(Error::ArithmeticError)?;
            self.positions.insert((market_id, caller), &pos);
            self.update_position_key(market_id, caller);

            let current = self.netuid_total_collateral.get(netuid).unwrap_or_default();
            self.netuid_total_collateral
                .insert(netuid, &current.checked_add(amount).ok_or(Error::ArithmeticError)?);

            self.env().emit_event(AlphaCollateralDeposited {
                netuid,
                user: caller,
                amount,
                market_id,
            });
            self.set_idle();
            Ok(market_id)
        }

        /// Withdraws alpha collateral. Only allowed if the user remains healthy after withdrawal.
        #[ink(message)]
        pub fn withdraw_alpha(
            &mut self,
            netuid: u16,
            amount: Balance,
            dest_coldkey: AccountId,
        ) -> Result<()> {
            self.ensure_not_paused()?;
            if amount == 0 {
                return Err(Error::ZeroAmount);
            }

            self.ensure_idle()?;

            let caller = self.env().caller();
            let market_id = self.netuid_to_market.get(netuid).ok_or(Error::UnapprovedNetuid)?;

            let pos = self.positions.get((market_id, caller)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            if pos.alpha_principal < amount {
                self.set_idle();
                return Err(Error::InsufficientCollateral);
            }

            // Effective amount equals the principal (no yield index).
            let effective = amount;

            // Check stake availability
            let availability = self
                .env()
                .extension()
                .get_stake_availability(self.env().account_id(), netuid)
                .map_err(|_| Error::ChainExtensionFailed)?;
            if effective > availability.available {
                self.set_idle();
                return Err(Error::InsufficientAvailableStake);
            }

            // Health check: ensure position stays healthy after withdrawal
            let has_debt = {
                let tao_pos = self.positions.get((0, caller)).unwrap_or(Position {
                    ltoken_balance: 0,
                    scaled_debt: 0,
                    alpha_principal: 0,
                });
                let tusdt_pos = self.positions.get((1, caller)).unwrap_or(Position {
                    ltoken_balance: 0,
                    scaled_debt: 0,
                    alpha_principal: 0,
                });
                tao_pos.scaled_debt > 0 || tusdt_pos.scaled_debt > 0
            };

            if has_debt {
                // Compute what health would be after withdrawal
                let mut new_pos = pos;
                new_pos.alpha_principal =
                    pos.alpha_principal.checked_sub(amount).ok_or(Error::ArithmeticError)?;
                self.positions.insert((market_id, caller), &new_pos);

                let current = self.netuid_total_collateral.get(netuid).unwrap_or_default();
                self.netuid_total_collateral
                    .insert(netuid, &current.checked_sub(amount).ok_or(Error::ArithmeticError)?);

                let health = self.get_health_factor(caller)?;
                // Revert the temporary state change
                self.positions.insert((market_id, caller), &pos);
                self.netuid_total_collateral.insert(netuid, &current);

                match health {
                    Some(hf) if hf.into_inner() < Ratio::one().into_inner() => {
                        self.set_idle();
                        return Err(Error::HealthFactorBelowThreshold);
                    },
                    _ => {},
                }
            }

            // Effects
            let mut pos = pos;
            pos.alpha_principal =
                pos.alpha_principal.checked_sub(amount).ok_or(Error::ArithmeticError)?;
            self.positions.insert((market_id, caller), &pos);
            self.update_position_key(market_id, caller);

            let current = self.netuid_total_collateral.get(netuid).unwrap_or_default();
            self.netuid_total_collateral
                .insert(netuid, &current.checked_sub(amount).ok_or(Error::ArithmeticError)?);

            // Transfer stake to destination coldkey (func 6)
            self.env()
                .extension()
                .transfer_stake(dest_coldkey, self.pool_hotkey, netuid, netuid, effective)
                .map_err(|_| {
                    // Revert position state on failure
                    let mut reverted = self.positions.get((market_id, caller)).unwrap_or(pos);
                    reverted.alpha_principal = reverted
                        .alpha_principal
                        .checked_add(amount)
                        .unwrap_or(reverted.alpha_principal);
                    self.positions.insert((market_id, caller), &reverted);
                    self.netuid_total_collateral.insert(netuid, &current);
                    Error::ChainExtensionFailed
                })?;

            self.env().emit_event(AlphaCollateralWithdrawn {
                netuid,
                user: caller,
                amount,
                effective,
                dest_coldkey,
            });

            self.set_idle();
            Ok(())
        }

        // ─────────────────────────────────────────────────────────────
        // Liquidation
        // ─────────────────────────────────────────────────────────────

        /// Liquidates an underwater position in full. The liquidator pays the
        /// borrower's ENTIRE debt on both markets — TAO and TUSDT, with
        /// accrued interest — and receives the borrower's full alpha
        /// collateral across every approved netuid, minus the platform's
        /// liquidation fee: a percentage of the collateral alpha taken by the
        /// `platform` role account, capped at the surplus share
        /// (`min(fee, (C − D) / C)`) so the liquidator never loses principal.
        /// When the collateral cannot cover the debt, the liquidator pays the
        /// collateral value (break-even) and the residual is written off as a
        /// market deficit.
        #[ink(message, payable)]
        pub fn liquidate(&mut self, borrower: AccountId) -> Result<()> {
            self.ensure_not_paused()?;
            self.ensure_idle()?;

            // Accrue interest on BOTH debt markets first: the debt the
            // liquidator pays must include accrued interest on both markets.
            self.accrue_interest(0).inspect_err(|_| {
                self.set_idle();
            })?;
            self.accrue_interest(1).inspect_err(|_| {
                self.set_idle();
            })?;

            let liquidator = self.env().caller();

            // Verify the borrower is liquidatable (strict HF < 1.0), computing
            // the health factor once.
            let health = self.get_health_factor(borrower)?;
            let liquidatable = matches!(
                health,
                Some(hf) if hf.into_inner() < Ratio::one().into_inner()
            );
            if !liquidatable {
                self.set_idle();
                return Err(Error::NotLiquidatable);
            }

            let tusdt_per_tao = self.get_oracle_price().inspect_err(|_| {
                self.set_idle();
            })?;

            // ── Compute the full position ──

            // Collateral: every approved netuid with principal, priced at
            // current oracle rates. The per-netuid platform share is computed
            // below once the totals (C and D) are known.
            let mut collateral_netuids: Vec<u16> = Vec::new();
            let mut collateral_positions: Vec<(u16, Balance)> = Vec::new();
            let mut collateral_total_value: u128 = 0;
            let market_count = self.market_keys.len();
            for i in 0..market_count {
                let market_id = self.market_keys.get(i).ok_or(Error::MarketNotFound)?;
                if market_id < 2 {
                    continue;
                }
                let netuid = self.market_to_netuid.get(market_id).ok_or(Error::MarketNotFound)?;
                let pos = self.positions.get((market_id, borrower)).unwrap_or(Position {
                    ltoken_balance: 0,
                    scaled_debt: 0,
                    alpha_principal: 0,
                });
                if pos.alpha_principal == 0 {
                    continue;
                }
                let price = self.collateral_price(netuid).inspect_err(|_| {
                    self.set_idle();
                })?;
                let value = price
                    .checked_mul_value(pos.alpha_principal.into())
                    .ok_or(Error::ArithmeticError)?;
                collateral_total_value =
                    collateral_total_value.checked_add(value).ok_or(Error::ArithmeticError)?;
                collateral_positions.push((netuid, pos.alpha_principal));
                collateral_netuids.push(netuid);
            }
            let collateral_value =
                Balance::try_from(collateral_total_value).map_err(|_| Error::ArithmeticError)?;
            if collateral_positions.is_empty() {
                // Nothing to seize — the borrower has no alpha collateral.
                self.set_idle();
                return Err(Error::ZeroAmount);
            }

            // Debt: both markets' face debts after the accrual above.
            let tao_state = self.markets.get(0).ok_or(Error::MarketNotFound)?;
            let tao_pos = self.positions.get((0, borrower)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            let tao_debt = if tao_pos.scaled_debt == 0 {
                0
            } else {
                scaled_debt_to_face(tao_pos.scaled_debt, tao_state.borrow_index)
                    .ok_or(Error::ArithmeticError)?
            };
            let tusdt_state = self.markets.get(1).ok_or(Error::MarketNotFound)?;
            let tusdt_pos = self.positions.get((1, borrower)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            let tusdt_debt = if tusdt_pos.scaled_debt == 0 {
                0
            } else {
                scaled_debt_to_face(tusdt_pos.scaled_debt, tusdt_state.borrow_index)
                    .ok_or(Error::ArithmeticError)?
            };
            let tao_debt_value = tusdt_per_tao
                .checked_mul_value(tao_debt.into())
                .and_then(|v| Balance::try_from(v).ok())
                .ok_or(Error::ArithmeticError)?;
            let debt_value =
                tao_debt_value.checked_add(tusdt_debt).ok_or(Error::ArithmeticError)?;
            if debt_value == 0 {
                // A liquidatable position always has debt; defensive guard
                // against dividing by zero below.
                self.set_idle();
                return Err(Error::ZeroAmount);
            }

            // ── Full-seizure split ──
            // Per netuid: platform_share = min(fee, (C − D)/C) when C > D,
            // else 0. The liquidator keeps the rounding remainder, so the
            // received value never falls below D.
            let underwater = collateral_value <= debt_value;
            let mut seized_entries: Vec<(u16, Balance, Ratio)> = Vec::new();
            let mut total_seized: u128 = 0;
            let mut total_platform_alpha: u128 = 0;
            for (netuid, principal) in &collateral_positions {
                let fee = self
                    .alpha_params
                    .get(*netuid)
                    .unwrap_or(default_alpha_params())
                    .liquidation_fee;
                let (_, platform_share, _) =
                    TusdtLendingPool::full_seizure_split(collateral_value, debt_value, fee)?;
                let platform_principal = platform_share
                    .checked_mul_value((*principal).into())
                    .and_then(|v| Balance::try_from(v).ok())
                    .ok_or(Error::ArithmeticError)?;
                total_seized =
                    total_seized.checked_add((*principal).into()).ok_or(Error::ArithmeticError)?;
                total_platform_alpha = total_platform_alpha
                    .checked_add(platform_principal.into())
                    .ok_or(Error::ArithmeticError)?;
                seized_entries.push((*netuid, *principal, platform_share));
            }

            // Payment per market:
            // - C > D: the full face debt of each market (TAO + TUSDT).
            // - C <= D: the collateral value, split across markets by
            //   debt-value share and floored per market, so the liquidator
            //   never pays more than C in total.
            let (payment_tao, payment_tusdt): (Balance, Balance) = if !underwater {
                (tao_debt, tusdt_debt)
            } else {
                let pay_tao_value = (collateral_value as u128)
                    .checked_mul(tao_debt_value as u128)
                    .and_then(|v| v.checked_div(debt_value as u128))
                    .and_then(|v| Balance::try_from(v).ok())
                    .ok_or(Error::ArithmeticError)?;
                let pay_tao = tusdt_per_tao
                    .checked_div_value(pay_tao_value.into())
                    .and_then(|v| Balance::try_from(v).ok())
                    .ok_or(Error::ArithmeticError)?;
                let pay_tusdt =
                    collateral_value.checked_sub(pay_tao_value).ok_or(Error::ArithmeticError)?;
                (pay_tao, pay_tusdt)
            };

            // ── Effects (all before external calls) ──

            // Retire both markets' debt in scaled lockstep: the market total
            // loses the SAME scaled units as the borrower's position. When
            // underwater and the payment does not cover a market's full debt,
            // the residual is uncollectible and is written off as a frozen
            // market deficit (it never compounds; `cover_deficit` can later
            // fund it from reserve_accrued).
            let mut total_deficit: Balance = 0;

            // Market 0 (TAO).
            let mut tao_state = tao_state;
            let mut tao_pos = tao_pos;
            let mut tao_deficit: Balance = 0;
            if tao_pos.scaled_debt > 0 {
                let full_clear = payment_tao >= tao_debt;
                let scaled_repaid = if full_clear {
                    tao_pos.scaled_debt
                } else {
                    min(
                        tao_state
                            .borrow_index
                            .checked_div_value_ceil(payment_tao.into())
                            .and_then(|v| Balance::try_from(v).ok())
                            .ok_or(Error::ArithmeticError)?,
                        tao_pos.scaled_debt,
                    )
                };
                tao_state.total_scaled_debt = tao_state
                    .total_scaled_debt
                    .checked_sub(scaled_repaid)
                    .ok_or(Error::ArithmeticError)?;
                tao_state.total_debt =
                    scaled_debt_to_face(tao_state.total_scaled_debt, tao_state.borrow_index)
                        .ok_or(Error::ArithmeticError)?;
                tao_pos.scaled_debt =
                    tao_pos.scaled_debt.checked_sub(scaled_repaid).ok_or(Error::ArithmeticError)?;
                let tao_principal = self.debt_principal.get((0, borrower)).unwrap_or(0);
                self.debt_principal
                    .insert((0, borrower), &tao_principal.saturating_sub(payment_tao));
                if underwater && tao_pos.scaled_debt > 0 {
                    tao_deficit = scaled_debt_to_face(tao_pos.scaled_debt, tao_state.borrow_index)
                        .ok_or(Error::ArithmeticError)?;
                    tao_state.total_scaled_debt = tao_state
                        .total_scaled_debt
                        .checked_sub(tao_pos.scaled_debt)
                        .ok_or(Error::ArithmeticError)?;
                    tao_state.total_debt =
                        scaled_debt_to_face(tao_state.total_scaled_debt, tao_state.borrow_index)
                            .ok_or(Error::ArithmeticError)?;
                    tao_pos.scaled_debt = 0;
                    self.debt_principal.insert((0, borrower), &0);
                }
                self.markets.insert(0, &tao_state);
                self.positions.insert((0, borrower), &tao_pos);
                self.update_position_key(0, borrower);
            }

            // Market 1 (TUSDT).
            let mut tusdt_state = tusdt_state;
            let mut tusdt_pos = tusdt_pos;
            let mut tusdt_deficit: Balance = 0;
            if tusdt_pos.scaled_debt > 0 {
                let full_clear = payment_tusdt >= tusdt_debt;
                let scaled_repaid = if full_clear {
                    tusdt_pos.scaled_debt
                } else {
                    min(
                        tusdt_state
                            .borrow_index
                            .checked_div_value_ceil(payment_tusdt.into())
                            .and_then(|v| Balance::try_from(v).ok())
                            .ok_or(Error::ArithmeticError)?,
                        tusdt_pos.scaled_debt,
                    )
                };
                tusdt_state.total_scaled_debt = tusdt_state
                    .total_scaled_debt
                    .checked_sub(scaled_repaid)
                    .ok_or(Error::ArithmeticError)?;
                tusdt_state.total_debt =
                    scaled_debt_to_face(tusdt_state.total_scaled_debt, tusdt_state.borrow_index)
                        .ok_or(Error::ArithmeticError)?;
                tusdt_pos.scaled_debt = tusdt_pos
                    .scaled_debt
                    .checked_sub(scaled_repaid)
                    .ok_or(Error::ArithmeticError)?;
                let tusdt_principal = self.debt_principal.get((1, borrower)).unwrap_or(0);
                self.debt_principal
                    .insert((1, borrower), &tusdt_principal.saturating_sub(payment_tusdt));
                if underwater && tusdt_pos.scaled_debt > 0 {
                    tusdt_deficit =
                        scaled_debt_to_face(tusdt_pos.scaled_debt, tusdt_state.borrow_index)
                            .ok_or(Error::ArithmeticError)?;
                    tusdt_state.total_scaled_debt = tusdt_state
                        .total_scaled_debt
                        .checked_sub(tusdt_pos.scaled_debt)
                        .ok_or(Error::ArithmeticError)?;
                    tusdt_state.total_debt = scaled_debt_to_face(
                        tusdt_state.total_scaled_debt,
                        tusdt_state.borrow_index,
                    )
                    .ok_or(Error::ArithmeticError)?;
                    tusdt_pos.scaled_debt = 0;
                    self.debt_principal.insert((1, borrower), &0);
                }
                self.markets.insert(1, &tusdt_state);
                self.positions.insert((1, borrower), &tusdt_pos);
                self.update_position_key(1, borrower);
            }

            // Emit the deficit events after both markets' write-offs.
            if tao_deficit > 0 {
                total_deficit =
                    total_deficit.checked_add(tao_deficit).ok_or(Error::ArithmeticError)?;
                let deficit = self.market_deficit.get(0).unwrap_or(0);
                self.market_deficit
                    .insert(0, &deficit.checked_add(tao_deficit).ok_or(Error::ArithmeticError)?);
                self.env().emit_event(DeficitReported {
                    market: 0,
                    user: borrower,
                    amount: tao_deficit,
                });
            }
            if tusdt_deficit > 0 {
                total_deficit =
                    total_deficit.checked_add(tusdt_deficit).ok_or(Error::ArithmeticError)?;
                let deficit = self.market_deficit.get(1).unwrap_or(0);
                self.market_deficit
                    .insert(1, &deficit.checked_add(tusdt_deficit).ok_or(Error::ArithmeticError)?);
                self.env().emit_event(DeficitReported {
                    market: 1,
                    user: borrower,
                    amount: tusdt_deficit,
                });
            }

            // Clear the borrower's alpha positions and the per-netuid totals.
            for (netuid, principal, _platform_share) in &seized_entries {
                let market_id =
                    self.netuid_to_market.get(*netuid).ok_or(Error::UnapprovedNetuid)?;
                let mut pos = self.positions.get((market_id, borrower)).unwrap_or(Position {
                    ltoken_balance: 0,
                    scaled_debt: 0,
                    alpha_principal: 0,
                });
                pos.alpha_principal = 0;
                self.positions.insert((market_id, borrower), &pos);
                self.update_position_key(market_id, borrower);

                let netuid_collateral =
                    self.netuid_total_collateral.get(*netuid).unwrap_or_default();
                self.netuid_total_collateral.insert(
                    *netuid,
                    &netuid_collateral.checked_sub(*principal).ok_or(Error::ArithmeticError)?,
                );
            }

            // ── External calls ──

            // Payments (debt only — the platform's cut comes out of the alpha).
            if payment_tusdt > 0 {
                self.tusdt
                    .transfer_from(liquidator, self.env().account_id(), payment_tusdt)
                    .map_err(|_| Error::TokenTransferFromFailed)?;
            }
            if payment_tao > 0 {
                let received = self.env().transferred_value();
                if received < payment_tao {
                    return Err(Error::TransferFailed);
                }
                if received > payment_tao {
                    let excess = received.checked_sub(payment_tao).ok_or(Error::ArithmeticError)?;
                    self.env().transfer(liquidator, excess).map_err(|_| Error::TransferFailed)?;
                }
            }

            // Alpha transfers: the platform's share first, then the
            // liquidator's, per netuid. ⚠️ Chain-extension effects are NOT
            // rolled back if a later call in this loop fails (storage
            // reverts, staking does not). The realistic failure mode — an
            // invalid hotkey pairing — fails the FIRST transfer before any
            // stake moves, and every netuid here was validated by the price
            // reads above, so a mid-loop failure would require a per-netuid
            // chain-extension fault (the same surface as the pre-existing
            // single-transfer liquidate).
            for (netuid, principal, platform_share) in &seized_entries {
                let platform_principal = platform_share
                    .checked_mul_value((*principal).into())
                    .and_then(|v| Balance::try_from(v).ok())
                    .ok_or(Error::ArithmeticError)?;
                let liquidator_principal =
                    principal.checked_sub(platform_principal).ok_or(Error::ArithmeticError)?;
                if platform_principal > 0 {
                    self.env()
                        .extension()
                        .transfer_stake(
                            self.platform,
                            self.pool_hotkey,
                            *netuid,
                            *netuid,
                            platform_principal,
                        )
                        .map_err(|_| Error::ChainExtensionFailed)?;
                }
                if liquidator_principal > 0 {
                    self.env()
                        .extension()
                        .transfer_stake(
                            liquidator,
                            self.pool_hotkey,
                            *netuid,
                            *netuid,
                            liquidator_principal,
                        )
                        .map_err(|_| Error::ChainExtensionFailed)?;
                }
            }

            self.env().emit_event(Liquidated {
                user: borrower,
                liquidator,
                collateral_netuids,
                collateral_seized: Balance::try_from(total_seized)
                    .map_err(|_| Error::ArithmeticError)?,
                platform_alpha: Balance::try_from(total_platform_alpha)
                    .map_err(|_| Error::ArithmeticError)?,
                debt_covered_tao: payment_tao,
                debt_covered_tusdt: payment_tusdt,
                deficit: total_deficit,
            });

            self.set_idle();
            Ok(())
        }

        // ─────────────────────────────────────────────────────────────
        // Permissionless fee claims
        // ─────────────────────────────────────────────────────────────

        /// Unstakes the full excess alpha staking for a netuid and sends the
        /// resulting native TAO to the treasury.
        ///
        /// Excess = actual available stake on the netuid minus booked
        /// collateral (`netuid_total_collateral` — every position's
        /// principal). The excess is unstaked via the chain extension and the
        /// resulting TAO is transferred directly to the treasury. The excess
        /// is unbooked value (owed to nobody), so the direct transfer cannot
        /// short-change suppliers or the reserve. Permissionless: anyone may
        /// trigger the claim; a second call finds no excess until new yield
        /// accrues.
        #[ink(message)]
        pub fn claim_alpha_excess(&mut self, netuid: u16) -> Result<()> {
            self.ensure_idle()?;
            self.ensure_approved_netuid(netuid).inspect_err(|_| {
                self.set_idle();
            })?;

            // Get actual available stake
            let availability = self
                .env()
                .extension()
                .get_stake_availability(self.env().account_id(), netuid)
                .map_err(|_| {
                    self.set_idle();
                    Error::ChainExtensionFailed
                })?;

            // Booked collateral is every position's principal (no yield index).
            let total_principal = self.netuid_total_collateral.get(netuid).unwrap_or_default();

            // Excess = actual available − booked. With zero positions (e.g.
            // orphaned stake after a hotkey migration) the whole stake is
            // excess and is recovered for the treasury.
            if availability.available <= total_principal {
                self.set_idle();
                return Ok(()); // no excess, no-op
            }
            let excess = availability
                .available
                .checked_sub(total_principal)
                .ok_or(Error::ArithmeticError)?;

            // Unstake the full excess to native TAO into the pool's free
            // balance, then forward it to the treasury.
            let balance_before = self.env().balance();
            self.env().extension().remove_stake(self.pool_hotkey, netuid, excess).map_err(
                |_| {
                    self.set_idle();
                    Error::ChainExtensionFailed
                },
            )?;
            let balance_after = self.env().balance();
            let tao_received =
                balance_after.checked_sub(balance_before).ok_or(Error::ArithmeticError)?;

            if tao_received > 0 {
                self.env().transfer(self.treasury, tao_received).map_err(|_| {
                    self.set_idle();
                    Error::TransferFailed
                })?;
            }

            self.env().emit_event(AlphaExcessClaimed {
                netuid,
                excess_alpha: excess,
                tao_received,
                recipient: self.treasury,
            });

            self.set_idle();
            Ok(())
        }

        /// Claims accumulated protocol reserve fees for a supply/borrow market.
        /// Sends the fees to the treasury. Permissionless; the claim is
        /// clamped to reserve above the market's unfunded deficit (which
        /// outranks the treasury claim) and to the physical cash available.
        #[ink(message)]
        pub fn claim_reserve(&mut self, market_id: u8) -> Result<()> {
            self.ensure_idle()?;

            self.accrue_interest(market_id).inspect_err(|_| {
                self.set_idle();
            })?;

            let mut state = self.markets.get(market_id).ok_or(Error::MarketNotFound)?;
            let claimable = state.reserve_accrued;
            if claimable == 0 {
                self.set_idle();
                return Ok(());
            }

            // Cap at physical cash available. For TAO, only the free native balance
            // above the root-staking liquidity sleeve counts — TAO staked on the
            // root subnet is not claimable without an unstake, and reserve claims
            // must never drain the liquidity sleeve.
            let cash = match market_id {
                0 => self.env().balance().saturating_sub(self.stake_buffer),
                1 => self.market_cash(1)?,
                _ => {
                    self.set_idle();
                    return Err(Error::MarketNotFound);
                },
            };
            // The deficit outranks the treasury's reserve claim (see
            // `cover_deficit`): claim only reserve above the unfunded deficit,
            // or a permissionless caller could strip the deficit's backing and
            // permanently under-back suppliers. No-op when the deficit
            // consumes the reserve.
            let deficit = self.market_deficit.get(market_id).unwrap_or(0);
            let claim = reserve_claimable(claimable, deficit, cash);
            if claim == 0 {
                self.set_idle();
                return Ok(());
            }

            state.reserve_accrued =
                state.reserve_accrued.checked_sub(claim).ok_or(Error::ArithmeticError)?;
            self.markets.insert(market_id, &state);

            match market_id {
                0 => {
                    self.env().transfer(self.treasury, claim).map_err(|_| Error::TransferFailed)?;
                },
                1 => {
                    self.tusdt
                        .transfer(self.treasury, claim)
                        .map_err(|_| Error::TokenContractCallFailed)?;
                },
                _ => {
                    self.set_idle();
                    return Err(Error::MarketNotFound);
                },
            }

            self.env().emit_event(ReserveClaimed { market: market_id, amount: claim });

            self.set_idle();
            Ok(())
        }

        /// Covers a market's unfunded bad-debt deficit from `reserve_accrued`.
        ///
        /// A deficit is the frozen residual of a write-off (see `liquidate`):
        /// debt removed from the ledger because the borrower's collateral was
        /// exhausted. Funding it from the reserve reallocates the treasury's
        /// reserve claim to close the balance-sheet hole the write-off opened,
        /// so suppliers' claims stay fully cash-backed. The treasury's
        /// permissionless `claim_reserve` can drain the reserve first — the
        /// maintainer should cover deficits before claiming. A deficit that
        /// exceeds the reserve stays on the books as an unfunded shortfall.
        /// Maintainer only; a no-op when there is nothing to cover. Errors:
        /// `Error::NotMaintainer`, `Error::MarketNotFound`,
        /// `Error::ArithmeticError`.
        #[ink(message)]
        pub fn cover_deficit(&mut self, market_id: u8) -> Result<()> {
            self.ensure_maintainer()?;
            self.ensure_idle()?;

            self.accrue_interest(market_id).inspect_err(|_| {
                self.set_idle();
            })?;

            let deficit = self.market_deficit.get(market_id).unwrap_or(0);
            if deficit == 0 {
                self.set_idle();
                return Ok(());
            }

            let mut state = self.markets.get(market_id).ok_or(Error::MarketNotFound)?;
            let cover = min(deficit, state.reserve_accrued);
            if cover == 0 {
                self.set_idle();
                return Ok(());
            }

            state.reserve_accrued =
                state.reserve_accrued.checked_sub(cover).ok_or(Error::ArithmeticError)?;
            self.markets.insert(market_id, &state);

            self.market_deficit
                .insert(market_id, &deficit.checked_sub(cover).ok_or(Error::ArithmeticError)?);

            self.env().emit_event(DeficitCovered { market: market_id, amount: cover });

            self.set_idle();
            Ok(())
        }

        /// Returns the unfunded bad-debt deficit of a market (face units of the
        /// market's asset, frozen at write-off), or `None` for an unknown
        /// market.
        #[ink(message)]
        pub fn get_market_deficit(&self, market_id: u8) -> Option<Balance> {
            self.market_deficit.get(market_id)
        }

        // ─────────────────────────────────────────────────────────────
        // Governance: alpha market management
        // ─────────────────────────────────────────────────────────────

        /// Adds or removes a subnet from the approved alpha markets list.
        #[ink(message)]
        pub fn set_approved_netuid(&mut self, netuid: u16, approved: bool) -> Result<()> {
            self.ensure_maintainer()?;

            if approved {
                if self.netuid_to_market.get(netuid).is_some() {
                    return Ok(()); // already approved
                }
                let market_id = self.next_alpha_market_id;
                self.netuid_to_market.insert(netuid, &market_id);
                self.market_to_netuid.insert(market_id, &netuid);
                self.market_keys.push(&market_id);
                self.next_alpha_market_id =
                    market_id.checked_add(1).ok_or(Error::ArithmeticError)?;
                self.env().emit_event(NetuidApproved {
                    netuid,
                    approved: true,
                    market_id: Some(market_id),
                });
            } else {
                let market_id = match self.netuid_to_market.get(netuid) {
                    Some(id) => id,
                    None => return Ok(()), // already not approved
                };
                // Refuse if any user still holds alpha on this netuid
                let total = self.netuid_total_collateral.get(netuid).unwrap_or_default();
                if total > 0 {
                    return Err(Error::NetuidHasPositions);
                }
                self.netuid_to_market.remove(netuid);
                self.market_to_netuid.remove(market_id);
                self.env().emit_event(NetuidApproved { netuid, approved: false, market_id: None });
            }
            Ok(())
        }

        /// Sets alpha market parameters (schedules timelocked update).
        #[ink(message)]
        pub fn set_alpha_params(
            &mut self,
            netuid: u16,
            config: AlphaMarketParamsConfig,
        ) -> Result<()> {
            self.ensure_maintainer()?;
            self.ensure_approved_netuid(netuid)?;

            let params = Self::alpha_params_from_config(config)?;
            Self::validate_alpha_params(&params)?;

            let execute_after = self
                .env()
                .block_timestamp()
                .checked_add(PARAMS_TIMELOCK_MS)
                .ok_or(Error::ArithmeticError)?;
            self.pending_alpha_params
                .insert(netuid, &PendingAlphaParamsUpdate { params: config, execute_after });

            self.env().emit_event(AlphaParamsUpdateScheduled { netuid, execute_after });
            Ok(())
        }

        /// Executes a scheduled alpha params update (permissionless, time-gated).
        #[ink(message)]
        pub fn execute_alpha_params_update(&mut self, netuid: u16) -> Result<()> {
            let pending =
                self.pending_alpha_params.get(netuid).ok_or(Error::NoPendingAlphaParamsUpdate)?;
            let now = self.env().block_timestamp();
            if now < pending.execute_after {
                return Err(Error::ParamsUpdateTimelockActive);
            }
            let params = Self::alpha_params_from_config(pending.params)?;
            Self::validate_alpha_params(&params)?;
            self.alpha_params.insert(netuid, &params);
            self.pending_alpha_params.remove(netuid);
            self.env().emit_event(AlphaParamsUpdated { netuid });
            Ok(())
        }

        /// Cancels a scheduled alpha params update.
        #[ink(message)]
        pub fn cancel_alpha_params_update(&mut self, netuid: u16) -> Result<()> {
            self.ensure_maintainer()?;
            if self.pending_alpha_params.get(netuid).is_none() {
                return Err(Error::NoPendingAlphaParamsUpdate);
            }
            self.pending_alpha_params.remove(netuid);
            self.env().emit_event(AlphaParamsUpdateCancelled { netuid });
            Ok(())
        }

        // ─────────────────────────────────────────────────────────────
        // Governance: interest rate params (timelocked)
        // ─────────────────────────────────────────────────────────────

        /// Schedules an interest-rate params update for a market (0 = TAO,
        /// 1 = TUSDT), executable after the timelock. Maintainer only. Errors:
        /// `Error::NotMaintainer`, `Error::MarketNotFound`, `Error::InvalidParam`,
        /// `Error::InvalidRatio`, `Error::ArithmeticError`.
        #[ink(message)]
        pub fn set_market_params(
            &mut self,
            market_id: u8,
            config: InterestRateParamsConfig,
        ) -> Result<()> {
            self.ensure_maintainer()?;
            if market_id > 1 {
                return Err(Error::MarketNotFound);
            }
            let params = Self::interest_params_from_config(config)?;
            Self::validate_interest_params(&params)?;

            let execute_after = self
                .env()
                .block_timestamp()
                .checked_add(PARAMS_TIMELOCK_MS)
                .ok_or(Error::ArithmeticError)?;
            self.pending_market_params
                .insert(market_id, &PendingInterestParamsUpdate { params: config, execute_after });
            self.env().emit_event(MarketParamsUpdateScheduled { market: market_id, execute_after });
            Ok(())
        }

        /// Executes a scheduled market params update. Permissionless, but blocked
        /// until the timelock expires. Errors:
        /// `Error::NoPendingMarketParamsUpdate`, `Error::ParamsUpdateTimelockActive`,
        /// `Error::InvalidParam`, `Error::InvalidRatio`.
        #[ink(message)]
        pub fn execute_market_params_update(&mut self, market_id: u8) -> Result<()> {
            let pending = self
                .pending_market_params
                .get(market_id)
                .ok_or(Error::NoPendingMarketParamsUpdate)?;
            let now = self.env().block_timestamp();
            if now < pending.execute_after {
                return Err(Error::ParamsUpdateTimelockActive);
            }
            let params = Self::interest_params_from_config(pending.params)?;
            Self::validate_interest_params(&params)?;
            self.market_params.insert(market_id, &params);
            self.pending_market_params.remove(market_id);
            self.env().emit_event(MarketParamsUpdated { market: market_id });
            Ok(())
        }

        /// Cancels a scheduled market params update. Maintainer only. Errors:
        /// `Error::NotMaintainer`, `Error::NoPendingMarketParamsUpdate`.
        #[ink(message)]
        pub fn cancel_market_params_update(&mut self, market_id: u8) -> Result<()> {
            self.ensure_maintainer()?;
            if self.pending_market_params.get(market_id).is_none() {
                return Err(Error::NoPendingMarketParamsUpdate);
            }
            self.pending_market_params.remove(market_id);
            self.env().emit_event(MarketParamsUpdateCancelled { market: market_id });
            Ok(())
        }

        // ─────────────────────────────────────────────────────────────
        // Governance: global params (timelocked)
        // ─────────────────────────────────────────────────────────────

        /// Schedules a global params update, executable after the timelock.
        /// Maintainer only. Errors: `Error::NotMaintainer`, `Error::InvalidParam`,
        /// `Error::InvalidRatio`, `Error::ArithmeticError`.
        #[ink(message)]
        pub fn set_global_params(&mut self, config: PoolGlobalParamsConfig) -> Result<()> {
            self.ensure_maintainer()?;
            let params = Self::global_params_from_config(config)?;
            Self::validate_global_params(&params)?;

            let execute_after = self
                .env()
                .block_timestamp()
                .checked_add(PARAMS_TIMELOCK_MS)
                .ok_or(Error::ArithmeticError)?;
            self.pending_global_params =
                Some(PendingGlobalParamsUpdate { params: config, execute_after });
            self.env().emit_event(GlobalParamsUpdateScheduled { execute_after });
            Ok(())
        }

        /// Executes a scheduled global params update. Permissionless, but blocked
        /// until the timelock expires. Errors:
        /// `Error::NoPendingGlobalParamsUpdate`, `Error::ParamsUpdateTimelockActive`,
        /// `Error::InvalidParam`, `Error::InvalidRatio`.
        #[ink(message)]
        pub fn execute_global_params_update(&mut self) -> Result<()> {
            let pending = self.pending_global_params.ok_or(Error::NoPendingGlobalParamsUpdate)?;
            let now = self.env().block_timestamp();
            if now < pending.execute_after {
                return Err(Error::ParamsUpdateTimelockActive);
            }
            let params = Self::global_params_from_config(pending.params)?;
            Self::validate_global_params(&params)?;
            self.global_params = params;
            self.pending_global_params = None;
            self.env().emit_event(GlobalParamsUpdated {});
            Ok(())
        }

        /// Cancels a scheduled global params update. Maintainer only. Errors:
        /// `Error::NotMaintainer`, `Error::NoPendingGlobalParamsUpdate`.
        #[ink(message)]
        pub fn cancel_global_params_update(&mut self) -> Result<()> {
            self.ensure_maintainer()?;
            if self.pending_global_params.is_none() {
                return Err(Error::NoPendingGlobalParamsUpdate);
            }
            self.pending_global_params = None;
            self.env().emit_event(GlobalParamsUpdateCancelled {});
            Ok(())
        }

        // ─────────────────────────────────────────────────────────────
        // Governance: role and address management
        // ─────────────────────────────────────────────────────────────

        /// Transfers the governance role. Governance only. Errors:
        /// `Error::NotGovernance`.
        #[ink(message)]
        pub fn update_governance(&mut self, new_governance: AccountId) -> Result<()> {
            self.ensure_governance()?;
            let previous = self.governance;
            self.governance = new_governance;
            self.env().emit_event(PoolGovernanceUpdated { previous, new: new_governance });
            Ok(())
        }

        /// Sets the maintainer role. Governance only. Errors:
        /// `Error::NotGovernance`.
        #[ink(message)]
        pub fn update_maintainer(&mut self, new_maintainer: AccountId) -> Result<()> {
            self.ensure_governance()?;
            self.maintainer = new_maintainer;
            self.env().emit_event(PoolMaintainerUpdated { new_maintainer });
            Ok(())
        }

        /// Returns the current maintainer account.
        #[ink(message)]
        pub fn maintainer(&self) -> AccountId {
            self.maintainer
        }

        /// Sets the treasury account. Governance only. Errors:
        /// `Error::NotGovernance`.
        #[ink(message)]
        pub fn update_treasury(&mut self, new_treasury: AccountId) -> Result<()> {
            self.ensure_governance()?;
            let previous = self.treasury;
            self.treasury = new_treasury;
            self.env().emit_event(PoolTreasuryUpdated { previous, new: new_treasury });
            Ok(())
        }

        /// Sets the platform account. Maintainer only. Errors:
        /// `Error::NotMaintainer`.
        #[ink(message)]
        pub fn update_platform(&mut self, new_platform: AccountId) -> Result<()> {
            self.ensure_maintainer()?;
            let previous = self.platform;
            self.platform = new_platform;
            self.env().emit_event(PoolPlatformUpdated { previous, new: new_platform });
            Ok(())
        }

        /// Points the pool at a new oracle contract. Maintainer only. Errors:
        /// `Error::NotMaintainer`.
        #[ink(message)]
        pub fn update_oracle_address(&mut self, new_oracle: AccountId) -> Result<()> {
            self.ensure_maintainer()?;
            let previous = self.oracle.to_account_id();
            self.oracle = TusdtOracleRef::from_account_id(new_oracle);
            self.env().emit_event(PoolOracleAddressUpdated { previous, new: new_oracle });
            Ok(())
        }

        /// Replaces the lToken contract address for a market. Maintainer only.
        /// Errors: `Error::NotMaintainer`, `Error::MarketNotFound`.
        #[ink(message)]
        pub fn update_ltoken_address(
            &mut self,
            market_id: u8,
            new_ltoken: AccountId,
        ) -> Result<()> {
            self.ensure_maintainer()?;
            let old = self.ltoken_by_market.get(market_id).ok_or(Error::MarketNotFound)?;
            self.ltoken_by_market.insert(market_id, &new_ltoken);
            self.env().emit_event(LTokenAddressUpdated {
                market: market_id,
                old_ltoken: old,
                new_ltoken,
            });
            Ok(())
        }

        /// Migrates the pool's staking hotkey, moving all alpha stake to a new hotkey.
        ///
        /// Moves the full AVAILABLE stake per netuid — booked collateral
        /// (all positions' principal) plus any unclaimed excess — not just
        /// principal. Moving principal alone would strand the excess under
        /// the old hotkey, where removals and withdrawals (which target
        /// `pool_hotkey` only) can never reach it.
        #[ink(message)]
        pub fn update_pool_hotkey(
            &mut self,
            new_hotkey: AccountId,
            netuids: Vec<u16>,
        ) -> Result<()> {
            self.ensure_maintainer()?;
            if netuids.len() > 32 {
                return Err(Error::TooManyNetuids);
            }
            let old_hotkey = self.pool_hotkey;
            for netuid in netuids {
                let availability = self
                    .env()
                    .extension()
                    .get_stake_availability(self.env().account_id(), netuid)
                    .map_err(|_| Error::ChainExtensionFailed)?;
                if availability.available > 0 {
                    self.env()
                        .extension()
                        .move_stake(old_hotkey, new_hotkey, netuid, netuid, availability.available)
                        .map_err(|_| Error::ChainExtensionFailed)?;
                }
            }
            self.pool_hotkey = new_hotkey;
            self.env().emit_event(PoolHotkeyChanged { old_hotkey, new_hotkey });
            Ok(())
        }

        // ─────────────────────────────────────────────────────────────
        // Emergency pause
        // ─────────────────────────────────────────────────────────────

        /// Pauses the pool, blocking supply/withdraw/borrow/repay/deposit
        /// operations. Governance or platform only. Errors:
        /// `Error::NotGovernanceOrPlatform`.
        #[ink(message)]
        pub fn pause(&mut self) -> Result<()> {
            self.ensure_governance_or_platform()?;
            self.paused = true;
            self.env().emit_event(PoolPaused {});
            Ok(())
        }

        /// Unpauses the pool. Maintainer only. Errors: `Error::NotMaintainer`.
        #[ink(message)]
        pub fn unpause(&mut self) -> Result<()> {
            self.ensure_maintainer()?;
            self.paused = false;
            self.env().emit_event(PoolUnpaused {});
            Ok(())
        }

        // ─────────────────────────────────────────────────────────────
        // Surplus sweeps
        // ─────────────────────────────────────────────────────────────

        /// Transfers up to `amount` of truly surplus TUSDT from the pool to the
        /// treasury. Maintainer only.
        ///
        /// "Surplus" is cash above every committed claim: supplier face value
        /// (`total_supplied × exchange_rate`), the accrued reserve, and any
        /// unfunded deficit. The amount is clamped so a sweep can never move
        /// supplier principal, the reserve, or deficit backing. With debt
        /// outstanding the pool is fully committed and the claimable surplus is
        /// zero — sweeps collect only leftovers such as dust and donations.
        /// Errors: `Error::NotMaintainer`, `Error::MarketNotFound`,
        /// `Error::TokenContractCallFailed`.
        #[ink(message)]
        pub fn claim_surplus_tusdt(&mut self, amount: Balance) -> Result<()> {
            self.ensure_maintainer()?;
            self.ensure_idle()?;

            // Accrue first so the obligations clamp reads a fresh exchange
            // rate: a stale one understates supplier face and would let the
            // sweep take cash the next accrual commits to suppliers.
            self.accrue_interest(1).inspect_err(|_| {
                self.set_idle();
            })?;

            let cash = self.market_cash(1)?;
            let obligations = self.market_obligations(1)?;
            let claim = min(amount, cash.saturating_sub(obligations));
            if claim == 0 {
                self.set_idle();
                return Ok(());
            }
            self.tusdt
                .transfer(self.treasury, claim)
                .map_err(|_| Error::TokenContractCallFailed)?;
            self.env()
                .emit_event(PoolSurplusTusdtClaimed { recipient: self.treasury, amount: claim });
            self.set_idle();
            Ok(())
        }

        /// Sweeps the pool's native TAO balance to the treasury, minus the
        /// market's committed claims (supplier face value, accrued reserve, and
        /// unfunded deficit), the root-staking liquidity sleeve
        /// (`stake_buffer`), and a 1-unit existential deposit guard. Maintainer
        /// only. Errors: `Error::NotMaintainer`, `Error::TransferFailed`.
        #[ink(message)]
        pub fn transfer_native_to_treasury(&mut self) -> Result<()> {
            self.ensure_maintainer()?;
            // Accrue first so the obligations clamp reads a fresh exchange
            // rate (see claim_surplus_tusdt).
            self.accrue_interest(0)?;
            let Some(amount) = self.treasury_sweepable() else {
                return Ok(()); // nothing above the claims + sleeve + ED guard to sweep
            };
            self.env().transfer(self.treasury, amount).map_err(|_| Error::TransferFailed)?;
            self.env().emit_event(PoolNativeTransferredToTreasury { amount });
            Ok(())
        }

        /// The market's committed claims on its cash: supplier face value
        /// (`total_supplied × exchange_rate`), the accrued reserve, and any
        /// unfunded deficit. Sweeps must never touch these. Errors:
        /// `Error::MarketNotFound`, `Error::ArithmeticError`.
        pub(crate) fn market_obligations(&self, market_id: u8) -> Result<Balance> {
            let state = self.markets.get(market_id).ok_or(Error::MarketNotFound)?;
            let supplier_face = state
                .exchange_rate
                .checked_mul_value(state.total_supplied.into())
                .and_then(|v| Balance::try_from(v).ok())
                .unwrap_or(0);
            let reserve = state.reserve_accrued;
            let deficit = self.market_deficit.get(market_id).unwrap_or(0);
            supplier_face
                .checked_add(reserve)
                .and_then(|s| s.checked_add(deficit))
                .ok_or(Error::ArithmeticError)
        }

        /// Computes how much free TAO the treasury sweep may take: everything
        /// above the market's committed claims (supplier face, reserve,
        /// deficit), the root-staking liquidity sleeve (`stake_buffer`), and a
        /// 1-unit existential deposit guard. `None` when nothing may be swept.
        pub(crate) fn treasury_sweepable(&self) -> Option<Balance> {
            let obligations = self.market_obligations(0).unwrap_or(0);
            let sweepable =
                self.env().balance().saturating_sub(obligations).saturating_sub(self.stake_buffer);
            if sweepable <= 1 {
                return None; // keep 1 as existential deposit guard
            }
            sweepable.checked_sub(1)
        }

        // ─────────────────────────────────────────────────────────────
        // Idle TAO root-subnet staking
        // ─────────────────────────────────────────────────────────────

        /// Updates the idle-TAO root-subnet staking configuration. Governance only.
        ///
        /// Enforces two invariants: `stake_floor >= MIN_ROOT_STAKE_FLOOR` (the
        /// pallet's minimum root stake) and `stake_buffer >= stake_floor` (the free
        /// sleeve always keeps at least one staking unit liquid). If `staked_tao > 0`
        /// and the hotkey changes, the pool fully unstakes its root position first so
        /// the booking never references a stale hotkey. Errors: `Error::NotGovernance`,
        /// `Error::InvalidParam`, `Error::LiquidityInsufficient` (when the required
        /// unstake fails), `Error::ArithmeticError`.
        #[ink(message)]
        pub fn set_root_stake_config(
            &mut self,
            root_hotkey: AccountId,
            staking_enabled: bool,
            stake_buffer: Balance,
            sweep_threshold: Balance,
            stake_floor: Balance,
        ) -> Result<()> {
            self.ensure_governance()?;

            // Invariants: floor >= pallet minimum, buffer >= floor.
            if stake_floor < MIN_ROOT_STAKE_FLOOR || stake_buffer < stake_floor {
                return Err(Error::InvalidParam);
            }

            // Hotkey rotation with stake outstanding: fully unstake first so the
            // bookkeeping never points at a hotkey the pool no longer stakes to.
            if self.staked_tao > 0 && root_hotkey != self.root_hotkey {
                self.ensure_idle()?;
                let booked = self.staked_tao;
                let received = self.top_up_free(booked)?;
                if received < booked {
                    self.set_idle();
                    return Err(Error::LiquidityInsufficient);
                }
                self.set_idle();
            }

            self.root_hotkey = root_hotkey;
            self.staking_enabled = staking_enabled;
            self.stake_buffer = stake_buffer;
            self.sweep_threshold = sweep_threshold;
            self.stake_floor = stake_floor;

            self.env().emit_event(RootStakeConfigUpdated {
                root_hotkey,
                staking_enabled,
                stake_buffer,
                sweep_threshold,
                stake_floor,
            });
            Ok(())
        }

        /// Permissionless keeper message: stakes excess free TAO into the root
        /// subnet (netuid 0). No-op when staking is disabled, the pool is paused,
        /// the excess is below `stake_floor`, or a sweep already ran this block.
        #[ink(message)]
        pub fn sweep(&mut self) -> Result<()> {
            self.ensure_not_paused()?;
            self.ensure_idle()?;
            let result = self.sweep_to_root();
            self.set_idle();
            result
        }

        /// Sweeps excess free TAO above `stake_buffer + sweep_threshold` into the
        /// root subnet (netuid 0). Returns `Ok(())` without doing anything when
        /// staking is disabled, the pool is paused, a sweep already ran this block,
        /// or the excess is below `stake_floor`. Callers may treat this as
        /// best-effort: a failure never corrupts pool state, and the booked amount
        /// is measured from the balance delta so silent runtime clipping cannot
        /// drift the accounting.
        pub(crate) fn sweep_to_root(&mut self) -> Result<()> {
            if !self.staking_enabled || self.paused {
                return Ok(());
            }
            let block = self.env().block_number();
            if self.last_sweep_block == Some(block) {
                return Ok(()); // rate limit: 1 sweep per block
            }
            let buffered = self.stake_buffer.saturating_add(self.sweep_threshold);
            let excess = self.env().balance().saturating_sub(buffered);
            if excess < self.stake_floor {
                return Ok(());
            }
            // CEI: rate-limit stamp and external call before booking the stake.
            self.last_sweep_block = Some(block);
            self.env()
                .extension()
                .add_stake(self.root_hotkey, 0, excess)
                .map_err(|_| Error::ChainExtensionFailed)?;
            // Root is a stable 1:1 subnet with zero fees and zero slippage, so the
            // booked amount equals the requested amount. The sweep only ever stakes
            // TAO above `stake_buffer` (>= `stake_floor` >= the existential
            // deposit), so silent ED clipping cannot bite either.
            self.staked_tao = self.staked_tao.checked_add(excess).ok_or(Error::ArithmeticError)?;
            self.env().emit_event(StakedIdleTao { amount: excess });
            Ok(())
        }

        /// Unstakes up to `shortfall` TAO from the root subnet into the free
        /// balance and returns the amount actually received (measured by balance
        /// delta). A partial exit that would leave a remainder below `stake_floor`
        /// instead exits the position fully. Maps any chain-extension failure to
        /// `Error::LiquidityInsufficient` so callers treat root stake as ordinary
        /// liquidity with a clean revert path.
        pub(crate) fn top_up_free(&mut self, shortfall: Balance) -> Result<Balance> {
            if shortfall == 0 || self.staked_tao == 0 {
                return Ok(0);
            }
            let availability = self
                .env()
                .extension()
                .get_stake_availability(self.env().account_id(), 0)
                .map_err(|_| Error::LiquidityInsufficient)?;
            let mut take = shortfall.min(availability.available).min(self.staked_tao);
            if take == 0 {
                return Ok(0);
            }
            // Dust rule: never leave a remainder below the staking floor; exit fully.
            let remainder = self.staked_tao.saturating_sub(take);
            if remainder > 0 && remainder < self.stake_floor {
                take = self.staked_tao;
            }
            self.env()
                .extension()
                .remove_stake(self.root_hotkey, 0, take)
                .map_err(|_| Error::LiquidityInsufficient)?;
            // Root is a stable 1:1 subnet with zero fees and zero slippage, so the
            // TAO received equals the alpha unstaked.
            self.staked_tao = self.staked_tao.saturating_sub(take);
            if take > 0 {
                self.env().emit_event(UnstakedIdleTao { amount: take });
            }
            Ok(take)
        }

        // ─────────────────────────────────────────────────────────────
        // View / read methods
        // ─────────────────────────────────────────────────────────────

        /// Returns the runtime state of a market, or `None` if it does not exist.
        #[ink(message)]
        pub fn get_market_state(&self, market_id: u8) -> Option<MarketState> {
            self.markets.get(market_id)
        }

        /// Returns the last interest-accrual timestamp (block timestamp in ms)
        /// for both debt markets as `(market 0, market 1)`, or `None` if either
        /// market is missing. Together with `get_borrow_index`, the market
        /// params, and each user's `scaled_debt` (via `get_all_positions`),
        /// this lets clients compute any user's current debt with accrued
        /// interest off-chain without a chain round-trip.
        #[ink(message)]
        pub fn get_last_interest_accrual_times(&self) -> Option<(u64, u64)> {
            Some((self.markets.get(0)?.last_update, self.markets.get(1)?.last_update))
        }

        /// Returns the position of a user in a market, or `None` if it does not
        /// exist.
        #[ink(message)]
        pub fn get_position(&self, market_id: u8, user: AccountId) -> Option<Position> {
            self.positions.get((market_id, user))
        }

        /// Returns the lToken exchange rate (1e18) for a market, or `None`.
        #[ink(message)]
        pub fn get_exchange_rate(&self, market_id: u8) -> Option<Ratio> {
            self.markets.get(market_id).map(|s| s.exchange_rate)
        }

        /// Returns the borrow index (1e18) for a market, or `None`.
        #[ink(message)]
        pub fn get_borrow_index(&self, market_id: u8) -> Option<Ratio> {
            self.markets.get(market_id).map(|s| s.borrow_index)
        }

        /// Returns the market utilization as `total_debt / (total_debt + cash)`
        /// (1e18 ratio), or `None`.
        #[ink(message)]
        pub fn get_utilization(&self, market_id: u8) -> Option<Ratio> {
            let state = self.markets.get(market_id)?;
            let cash = self.market_cash(market_id).ok()?;
            let total = state.total_debt.checked_add(cash)?;
            if total == 0 {
                return Some(Ratio::from_inner(0));
            }
            Ratio::from_integer(state.total_debt.into()).checked_div_int(total.into())
        }

        /// Returns the booked TAO currently staked on the root subnet (netuid 0).
        #[ink(message)]
        pub fn get_tao_staked(&self) -> Balance {
            self.staked_tao
        }

        /// Returns the current idle-TAO root-subnet staking configuration.
        #[ink(message)]
        pub fn get_root_stake_config(&self) -> RootStakeConfig {
            RootStakeConfig {
                root_hotkey: self.root_hotkey,
                staking_enabled: self.staking_enabled,
                stake_buffer: self.stake_buffer,
                sweep_threshold: self.sweep_threshold,
                stake_floor: self.stake_floor,
            }
        }

        /// Returns the current annual borrow rate (1e18) for a market, or `None`.
        #[ink(message)]
        pub fn get_borrow_rate(&self, market_id: u8) -> Option<Ratio> {
            let params = self.market_params.get(market_id)?;
            let utilization = self.get_utilization(market_id)?;
            Self::compute_borrow_rate(&params, utilization).ok()
        }

        /// Returns the current annual supply rate (1e18) for a market, or `None`.
        #[ink(message)]
        pub fn get_supply_rate(&self, market_id: u8) -> Option<Ratio> {
            let params = self.market_params.get(market_id)?;
            let utilization = self.get_utilization(market_id)?;
            let borrow_rate = Self::compute_borrow_rate(&params, utilization).ok()?;
            let one = Ratio::one();
            borrow_rate.checked_mul(utilization).and_then(|r| {
                let one_minus_rf = Ratio::from_inner(
                    one.into_inner().checked_sub(params.reserve_factor.into_inner())?,
                );
                r.checked_mul(one_minus_rf)
            })
        }

        /// Returns the underlying amount a user's lToken balance is worth in a
        /// market (principal plus their accrued share of borrower interest),
        /// or `None` for alpha markets or an unknown user/market.
        #[ink(message)]
        pub fn get_underlying_balance(&self, market_id: u8, user: AccountId) -> Option<Balance> {
            if market_id > 1 {
                return None; // alpha markets have no lToken
            }
            let pos = self.positions.get((market_id, user))?;
            let state = self.markets.get(market_id)?;
            compute_redeem_amount(pos.ltoken_balance, state.exchange_rate)
        }

        /// Returns a user's current debt in a market in underlying units
        /// (including accrued interest), or `None`.
        #[ink(message)]
        pub fn get_user_debt(&self, market_id: u8, user: AccountId) -> Option<Balance> {
            let pos = self.positions.get((market_id, user)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            let state = self.markets.get(market_id)?;
            if pos.scaled_debt == 0 {
                return Some(0);
            }
            state
                .borrow_index
                .checked_mul_value(pos.scaled_debt.into())
                .and_then(|v| Balance::try_from(v).ok())
        }

        /// Returns a user's debt breakdown in a market as `(debt, principal)`
        /// in underlying units. `debt` includes accrued interest; `principal`
        /// is the borrow principal not yet repaid. Interest owed = debt −
        /// principal. Positions created before principal tracking existed
        /// fall back to treating `scaled_debt` as principal (exact when
        /// borrowed at borrow_index = 1.0, otherwise a slight under-estimate
        /// of principal / over-estimate of interest).
        #[ink(message)]
        pub fn get_user_debt_details(
            &self,
            market_id: u8,
            user: AccountId,
        ) -> Option<(Balance, Balance)> {
            let pos = self.positions.get((market_id, user)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            let state = self.markets.get(market_id)?;
            let debt = if pos.scaled_debt == 0 {
                0
            } else {
                state
                    .borrow_index
                    .checked_mul_value(pos.scaled_debt.into())
                    .and_then(|v| Balance::try_from(v).ok())?
            };
            let tracked = self.debt_principal.get((market_id, user)).unwrap_or(0);
            // For tracked positions scaled_debt ≤ principal always (index ≥ 1),
            // so the max is exact. For legacy positions it estimates principal
            // as the scaled debt. Clamp to debt so interest is never negative.
            let principal = min(tracked.max(pos.scaled_debt), debt);
            Some((debt, principal))
        }

        /// Returns all approved alpha markets as `(netuid, params)` pairs.
        #[ink(message)]
        pub fn get_alpha_markets(&self) -> Vec<(u16, AlphaMarketParams)> {
            let mut result = Vec::new();
            let count = self.market_keys.len();
            for i in 0..count {
                let market_id = match self.market_keys.get(i) {
                    Some(id) => id,
                    None => continue,
                };
                if market_id < 2 {
                    continue;
                }
                let netuid = match self.market_to_netuid.get(market_id) {
                    Some(n) => n,
                    None => continue,
                };
                let params = self.alpha_params.get(netuid).unwrap_or(default_alpha_params());
                result.push((netuid, params));
            }
            result
        }

        /// Returns a user's alpha principal collateral for a netuid, or `None`.
        #[ink(message)]
        pub fn get_user_alpha_position(&self, user: AccountId, netuid: u16) -> Option<Balance> {
            let market_id = self.netuid_to_market.get(netuid)?;
            let pos = self.positions.get((market_id, user))?;
            Some(pos.alpha_principal)
        }

        /// Returns the total alpha principal collateral booked for a netuid, or
        /// `None`.
        #[ink(message)]
        pub fn get_netuid_total_collateral(&self, netuid: u16) -> Option<Balance> {
            self.netuid_total_collateral.get(netuid)
        }

        /// Returns `true` if the netuid is an approved alpha market.
        #[ink(message)]
        pub fn is_approved_netuid(&self, netuid: u16) -> bool {
            self.netuid_to_market.get(netuid).is_some()
        }

        /// Returns the number of approved alpha markets (market ids >= 2).
        #[ink(message)]
        pub fn get_active_netuids_count(&self) -> u32 {
            let count = self.market_keys.len();
            let mut alpha_count: u32 = 0;
            for i in 0..count {
                if let Some(id) = self.market_keys.get(i) {
                    if id >= 2 {
                        alpha_count = alpha_count.saturating_add(1);
                    }
                }
            }
            alpha_count
        }

        /// Returns up to `PAGE_SIZE` positions of a user on the given page.
        #[ink(message)]
        pub fn get_positions(&self, user: AccountId, page: u32) -> Vec<(u8, Position)> {
            let mut result = Vec::new();
            let mut seen = Vec::new();
            let total = self.position_keys.len();
            let start = page.saturating_mul(PAGE_SIZE);
            let end = min(start.saturating_add(PAGE_SIZE), total);
            for i in start..end {
                let key = match self.position_keys.get(i) {
                    Some(k) => k,
                    None => continue,
                };
                if key.1 != user {
                    continue;
                }
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                if let Some(pos) = self.positions.get(key) {
                    result.push((key.0, pos));
                }
            }
            result
        }

        /// Returns up to `PAGE_SIZE` positions across all users on the given
        /// page.
        #[ink(message)]
        pub fn get_all_positions(&self, page: u32) -> Vec<((u8, AccountId), Position)> {
            let mut result = Vec::new();
            let mut seen = Vec::new();
            let total = self.position_keys.len();
            let start = page.saturating_mul(PAGE_SIZE);
            let end = min(start.saturating_add(PAGE_SIZE), total);
            for i in start..end {
                let key = match self.position_keys.get(i) {
                    Some(k) => k,
                    None => continue,
                };
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                if let Some(pos) = self.positions.get(key) {
                    result.push((key, pos));
                }
            }
            result
        }

        /// Returns the pending alpha params update for a netuid, or `None`.
        #[ink(message)]
        pub fn get_pending_alpha_params_update(
            &self,
            netuid: u16,
        ) -> Option<PendingAlphaParamsUpdate> {
            self.pending_alpha_params.get(netuid)
        }

        /// Returns the pending market params update for a market, or `None`.
        #[ink(message)]
        pub fn get_pending_market_params_update(
            &self,
            market_id: u8,
        ) -> Option<PendingInterestParamsUpdate> {
            self.pending_market_params.get(market_id)
        }

        /// Returns the pending global params update, or `None`.
        #[ink(message)]
        pub fn get_pending_global_params_update(&self) -> Option<PendingGlobalParamsUpdate> {
            self.pending_global_params
        }

        // ─────────────────────────────────────────────────────────────
        // Role / address getters
        // ─────────────────────────────────────────────────────────────

        /// Returns the governance account.
        #[ink(message)]
        pub fn governance(&self) -> AccountId {
            self.governance
        }

        /// Returns the treasury account.
        #[ink(message)]
        pub fn treasury(&self) -> AccountId {
            self.treasury
        }

        /// Returns the platform account.
        #[ink(message)]
        pub fn platform(&self) -> AccountId {
            self.platform
        }

        /// Returns whether the pool is currently paused.
        #[ink(message)]
        pub fn paused(&self) -> bool {
            self.paused
        }

        /// Returns the oracle contract address.
        #[ink(message)]
        pub fn get_oracle_address(&self) -> AccountId {
            self.oracle.to_account_id()
        }

        /// Returns the TUSDT token contract address.
        #[ink(message)]
        pub fn get_tusdt_address(&self) -> AccountId {
            self.tusdt.to_account_id()
        }

        /// Returns the lToken contract address for a market, or `None`.
        #[ink(message)]
        pub fn get_ltoken_address(&self, market: u8) -> Option<AccountId> {
            self.ltoken_by_market.get(market)
        }

        /// Returns the pool's staking hotkey.
        #[ink(message)]
        pub fn get_pool_hotkey(&self) -> AccountId {
            self.pool_hotkey
        }

        /// Returns the current global params as an external config (ratios in
        /// basis points).
        #[ink(message)]
        pub fn get_global_params(&self) -> PoolGlobalParamsConfig {
            self.global_params.to_config()
        }

        /// Returns the current interest-rate params of a market as an external
        /// config (basis points), or `None`.
        #[ink(message)]
        pub fn get_market_params(&self, market_id: u8) -> Option<InterestRateParamsConfig> {
            self.market_params.get(market_id).map(|p| p.to_config())
        }

        /// Returns the current alpha market params of a netuid as an external
        /// config (basis points), or `None`.
        #[ink(message)]
        pub fn get_alpha_params(&self, netuid: u16) -> Option<AlphaMarketParamsConfig> {
            self.alpha_params.get(netuid).map(|p| p.to_config())
        }
    }

    #[cfg(test)]
    impl TusdtLendingPool {
        /// Test-only constructor: builds a pool with placeholder contract addresses
        /// so unit tests can run without deployed child/oracle instances.
        pub(crate) fn new_for_test(governance: AccountId) -> Self {
            use ink::env::call::FromAccountId;
            let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
            let now = 0u64;

            let mut markets = Mapping::default();
            markets.insert(0, &MarketState::new(now));
            markets.insert(1, &MarketState::new(now));

            let mut ltoken_by_market = Mapping::default();
            ltoken_by_market.insert(0, &accounts.charlie); // fake lTAO
            ltoken_by_market.insert(1, &accounts.eve); // fake lTUSDT

            let mut market_keys = StorageVec::new();
            market_keys.push(&0);
            market_keys.push(&1);

            let mut market_params = Mapping::default();
            market_params.insert(0, &default_tao_interest_params());
            market_params.insert(1, &default_tusdt_interest_params());

            Self {
                governance,
                maintainer: governance,
                treasury: governance,
                platform: governance,
                paused: false,
                busy: false,
                oracle: TusdtOracleRef::from_account_id(accounts.frank),
                tusdt: TusdtErc20Ref::from_account_id(accounts.django),
                pool_hotkey: accounts.bob,
                markets,
                market_keys,
                ltoken_by_market,
                netuid_to_market: Mapping::default(),
                market_to_netuid: Mapping::default(),
                next_alpha_market_id: 2,
                market_params,
                alpha_params: Mapping::default(),
                pending_market_params: Mapping::default(),
                pending_alpha_params: Mapping::default(),
                global_params: default_global_params(),
                pending_global_params: None,
                netuid_total_collateral: Mapping::default(),
                positions: Mapping::default(),
                position_keys: StorageVec::new(),
                debt_principal: Mapping::default(),
                market_deficit: Mapping::default(),
                root_hotkey: accounts.bob,
                staked_tao: 0,
                stake_buffer: DEFAULT_STAKE_BUFFER,
                sweep_threshold: 0,
                stake_floor: MIN_ROOT_STAKE_FLOOR,
                last_sweep_block: None,
                staking_enabled: false,
            }
        }

        /// Test-only: directly set a position in the mapping.
        pub(crate) fn debug_set_position(&mut self, market_id: u8, user: AccountId, pos: Position) {
            self.positions.insert((market_id, user), &pos);
        }

        /// Test-only: seed the root-staking bookkeeping (`staked_tao`) directly,
        /// bypassing a real sweep.
        pub(crate) fn debug_set_root_stake(&mut self, staked_tao: Balance) {
            self.staked_tao = staked_tao;
        }

        /// Test-only: directly set the tracked debt principal for a position.
        pub(crate) fn debug_set_debt_principal(
            &mut self,
            market_id: u8,
            user: AccountId,
            principal: Balance,
        ) {
            self.debt_principal.insert((market_id, user), &principal);
        }

        /// Test-only: directly set a market's state (e.g. to simulate a grown borrow index).
        pub(crate) fn debug_set_market_state(&mut self, market_id: u8, state: MarketState) {
            self.markets.insert(market_id, &state);
        }

        /// Test-only: directly set a market's unfunded bad-debt deficit.
        pub(crate) fn debug_set_market_deficit(&mut self, market_id: u8, deficit: Balance) {
            self.market_deficit.insert(market_id, &deficit);
        }

        /// Test-only: push a key into position_keys (simulates legacy
        /// append-only behaviour for testing dedup and self-healing).
        pub(crate) fn debug_push_position_key(&mut self, market_id: u8, user: AccountId) {
            self.position_keys.push(&(market_id, user));
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    include!("tests.rs");
}
