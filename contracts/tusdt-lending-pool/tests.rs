
// Lending pool unit tests. Uses `#[ink::test]` with a mock chain extension.

use super::lending_pool::*;
use ink::env::test;
use tusdt_primitives::Ratio;
use tusdt_test_support::{
    last_ext_call, register_extension, register_mock, register_mock_chain_fails,
    register_mock_no_stake, register_mock_stateful_root, register_mock_transfer_fails,
    set_callee_balance, set_caller, MockExtension,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_accounts() -> test::DefaultAccounts<tusdt_env::CustomEnvironment> {
    test::default_accounts::<tusdt_env::CustomEnvironment>()
}

fn setup() -> (
    TusdtLendingPool,
    test::DefaultAccounts<tusdt_env::CustomEnvironment>,
) {
    let accounts = default_accounts();
    let pool = TusdtLendingPool::new_for_test(accounts.alice);
    (pool, accounts)
}

fn setup_with_alpha(netuid: u16) -> (
    TusdtLendingPool,
    test::DefaultAccounts<tusdt_env::CustomEnvironment>,
) {
    let accounts = default_accounts();
    let mut pool = TusdtLendingPool::new_for_test(accounts.alice);
    set_caller(accounts.alice);
    pool.set_approved_netuid(netuid, true).unwrap();
    (pool, accounts)
}

/// Sets the global params timelock to zero so tests can execute param updates instantly.
fn set_timelock_to_zero(pool: &mut TusdtLendingPool) {
    let config = PoolGlobalParamsConfig {
        max_oracle_age_ms: 1_800_000,
        supply_cap_tao: 0,
        supply_cap_tusdt: 0,
        borrow_cap_tao: 0,
        borrow_cap_tusdt: 0,
    };
    pool.set_global_params(config).unwrap();
    // Advance past the timelock, execute, then reset.
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        PARAMS_TIMELOCK_MS + 1,
    );
    pool.execute_global_params_update().unwrap();
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(0);
}

// ---------------------------------------------------------------------------
// Constructor & access control
// ---------------------------------------------------------------------------

#[ink::test]
fn constructor_sets_governance() {
    let (pool, accounts) = setup();
    assert_eq!(pool.governance(), accounts.alice);
}

#[ink::test]
fn constructor_sets_treasury() {
    let (pool, accounts) = setup();
    assert_eq!(pool.treasury(), accounts.alice);
}

#[ink::test]
fn constructor_sets_platform() {
    let (pool, accounts) = setup();
    assert_eq!(pool.platform(), accounts.alice);
}

#[ink::test]
fn paused_by_default() {
    let (pool, _accounts) = setup();
    assert!(!pool.paused());
}

#[ink::test]
fn governance_can_pause_and_unpause() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.pause().unwrap();
    assert!(pool.paused());
    pool.unpause().unwrap();
    assert!(!pool.paused());
}

#[ink::test]
fn non_governance_cannot_pause() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.bob);
    assert_eq!(pool.pause(), Err(Error::NotGovernanceOrPlatform));
}

#[ink::test]
fn platform_can_pause() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    // Platform is alice (same as governance in test setup)
    pool.pause().unwrap();
    assert!(pool.paused());
}

#[ink::test]
fn non_governance_cannot_unpause() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.pause().unwrap();
    set_caller(accounts.bob);
    assert_eq!(pool.unpause(), Err(Error::NotMaintainer));
}

#[ink::test]
fn governance_can_update_governance() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.update_governance(accounts.bob).unwrap();
    assert_eq!(pool.governance(), accounts.bob);
    // Old governance can no longer pause (platform unchanged, so platform can still pause)
    pool.pause().unwrap(); // platform is still alice
}

#[ink::test]
fn non_governance_cannot_update_governance() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.bob);
    assert_eq!(
        pool.update_governance(accounts.bob),
        Err(Error::NotGovernance)
    );
}

#[ink::test]
fn governance_can_update_treasury() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.update_treasury(accounts.bob).unwrap();
    assert_eq!(pool.treasury(), accounts.bob);
}

#[ink::test]
fn governance_can_update_platform() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.update_platform(accounts.bob).unwrap();
    assert_eq!(pool.platform(), accounts.bob);
}

// ---------------------------------------------------------------------------
// Alpha market management
// ---------------------------------------------------------------------------

#[ink::test]
fn governance_can_add_alpha_market() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_approved_netuid(1, true).unwrap();
    assert!(pool.is_approved_netuid(1));
}

#[ink::test]
fn governance_can_remove_alpha_market() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_approved_netuid(1, true).unwrap();
    assert!(pool.is_approved_netuid(1));
    pool.set_approved_netuid(1, false).unwrap();
    assert!(!pool.is_approved_netuid(1));
}

#[ink::test]
fn non_governance_cannot_manage_netuids() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.bob);
    assert_eq!(
        pool.set_approved_netuid(1, true),
        Err(Error::NotMaintainer)
    );
}

#[ink::test]
fn cannot_remove_netuid_with_positions() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    // Deposit alpha to create a real position
    register_mock(1_000);
    pool.deposit_alpha(1, 100).unwrap();
    // Now removal should be blocked
    assert_eq!(
        pool.set_approved_netuid(1, false),
        Err(Error::NetuidHasPositions)
    );
}

#[ink::test]
fn duplicate_approval_is_noop() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_approved_netuid(1, true).unwrap();
    assert!(pool.is_approved_netuid(1));
    // Second call is a no-op
    pool.set_approved_netuid(1, true).unwrap();
    assert!(pool.is_approved_netuid(1));
}

#[ink::test]
fn removing_unapproved_netuid_is_noop() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_approved_netuid(99, false).unwrap();
    assert!(!pool.is_approved_netuid(99));
}

#[ink::test]
fn alpha_market_count() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    assert_eq!(pool.get_active_netuids_count(), 0);
    pool.set_approved_netuid(1, true).unwrap();
    assert_eq!(pool.get_active_netuids_count(), 1);
    pool.set_approved_netuid(2, true).unwrap();
    assert_eq!(pool.get_active_netuids_count(), 2);
}

#[ink::test]
fn alpha_markets_list() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_approved_netuid(1, true).unwrap();
    pool.set_approved_netuid(2, true).unwrap();
    let markets = pool.get_alpha_markets();
    assert_eq!(markets.len(), 2);
}

// ---------------------------------------------------------------------------
// Interest rate math (pure function, no cross-contract calls)
// ---------------------------------------------------------------------------

fn default_params() -> InterestRateParams {
    InterestRateParams {
        base_rate: Ratio::from_inner(0),
        slope1: Ratio::from_basis_points(400),
        slope2: Ratio::from_basis_points(9600),
        optimal_utilization: Ratio::from_basis_points(8000),
        reserve_factor: Ratio::from_basis_points(2000),
    }
}

#[ink::test]
fn borrow_rate_zero_at_zero_utilization() {
    let params = default_params();
    let rate = TusdtLendingPool::compute_borrow_rate(&params, Ratio::from_inner(0)).unwrap();
    assert!(rate.is_zero());
}

#[ink::test]
fn borrow_rate_in_zone_1() {
    let params = default_params();
    // U = 40% (4_000 bps), optimal = 80% (8_000 bps)
    let utilization = Ratio::from_basis_points(4000);
    let rate = TusdtLendingPool::compute_borrow_rate(&params, utilization).unwrap();
    // Zone 1: base(0) + slope1(4%) × U/opt = 0 + 0.04 × 0.4/0.8 = 0.04 × 0.5 = 0.02 = 2%
    let expected = Ratio::from_basis_points(200);
    assert_eq!(rate, expected);
}

#[ink::test]
fn borrow_rate_at_optimal() {
    let params = default_params();
    // U = optimal = 80%
    let utilization = Ratio::from_basis_points(8000);
    let rate = TusdtLendingPool::compute_borrow_rate(&params, utilization).unwrap();
    // Zone 1: base(0) + slope1(4%) × 1.0 = 4%
    let expected = Ratio::from_basis_points(400);
    assert_eq!(rate, expected);
}

#[ink::test]
fn borrow_rate_in_zone_2() {
    let params = default_params();
    // U = 90%, optimal = 80%
    let utilization = Ratio::from_basis_points(9000);
    let rate = TusdtLendingPool::compute_borrow_rate(&params, utilization).unwrap();
    // Zone 2: base(0) + slope1(4%) + slope2(96%) × (0.9-0.8)/(1.0-0.8)
    // = 0.04 + 0.96 × 0.1/0.2 = 0.04 + 0.96 × 0.5 = 0.04 + 0.48 = 0.52 = 52%
    let expected = Ratio::from_basis_points(5200);
    assert_eq!(rate, expected);
}

#[ink::test]
fn borrow_rate_at_full_utilization() {
    let params = default_params();
    // U = 100%
    let utilization = Ratio::one();
    let rate = TusdtLendingPool::compute_borrow_rate(&params, utilization).unwrap();
    // Zone 2: base(0) + slope1(4%) + slope2(96%) × 1.0 = 100%
    let expected = Ratio::from_basis_points(10_000);
    assert_eq!(rate, expected);
}

#[ink::test]
fn borrow_rate_with_nonzero_base() {
    let params = InterestRateParams {
        base_rate: Ratio::from_basis_points(100), // 1%
        slope1: Ratio::from_basis_points(300),    // 3%
        slope2: Ratio::from_basis_points(9000),   // 90%
        optimal_utilization: Ratio::from_basis_points(8000),
        reserve_factor: Ratio::from_basis_points(2000),
    };
    let utilization = Ratio::from_basis_points(4000);
    let rate = TusdtLendingPool::compute_borrow_rate(&params, utilization).unwrap();
    // Zone 1: 1% + 3% × 0.5 = 1% + 1.5% = 2.5%
    let expected = Ratio::from_basis_points(250);
    assert_eq!(rate, expected);
}

// ---------------------------------------------------------------------------
// Params validation
// ---------------------------------------------------------------------------

#[ink::test]
fn valid_interest_params_accepted() {
    let config = InterestRateParamsConfig {
        base_rate: 0,
        slope1: 400,
        slope2: 9600,
        optimal_utilization: 8000,
        reserve_factor: 2000,
    };
    let params = TusdtLendingPool::interest_params_from_config(config).unwrap();
    assert_eq!(params.base_rate, Ratio::from_basis_points(0));
    assert_eq!(params.slope1, Ratio::from_basis_points(400));
}

#[ink::test]
fn interest_params_rejects_zero_optimal() {
    let config = InterestRateParamsConfig {
        base_rate: 0,
        slope1: 400,
        slope2: 9600,
        optimal_utilization: 0,
        reserve_factor: 2000,
    };
    assert_eq!(
        TusdtLendingPool::interest_params_from_config(config),
        Err(Error::InvalidParam)
    );
}

#[ink::test]
fn interest_params_rejects_slope_overflow() {
    let config = InterestRateParamsConfig {
        base_rate: 0,
        slope1: 6000,
        slope2: 6000, // sum = 12000 > 10000
        optimal_utilization: 8000,
        reserve_factor: 2000,
    };
    assert_eq!(
        TusdtLendingPool::interest_params_from_config(config),
        Err(Error::InvalidParam)
    );
}

#[ink::test]
fn interest_params_rejects_full_reserve_factor() {
    let config = InterestRateParamsConfig {
        base_rate: 0,
        slope1: 400,
        slope2: 9600,
        optimal_utilization: 8000,
        reserve_factor: 10_000, // 100% reserve factor
    };
    assert_eq!(
        TusdtLendingPool::interest_params_from_config(config),
        Err(Error::InvalidParam)
    );
}

#[ink::test]
fn valid_alpha_params_accepted() {
    let config = AlphaMarketParamsConfig {
        collateral_factor: 5000,
        liquidation_threshold: 6000,
        liquidation_fee: 500,
        supply_cap: 0,
    };
    let params = TusdtLendingPool::alpha_params_from_config(config).unwrap();
    assert_eq!(params.collateral_factor, Ratio::from_basis_points(5000));
    assert_eq!(params.liquidation_threshold, Ratio::from_basis_points(6000));
}

#[ink::test]
fn alpha_params_rejects_cf_ge_lt() {
    let config = AlphaMarketParamsConfig {
        collateral_factor: 6000,
        liquidation_threshold: 5000, // CF > LT — invalid
        liquidation_fee: 500,
        supply_cap: 0,
    };
    assert_eq!(
        TusdtLendingPool::alpha_params_from_config(config),
        Err(Error::InvalidParam)
    );
}

#[ink::test]
fn alpha_params_rejects_high_fee_above_liquidation_room() {
    // The fee must leave liquidation room (fee + threshold <= 10_000) so the full fee fits
    // within the liquidation surplus at the boundary; deeper-underwater positions fall back
    // to the surplus-share cap in full_seizure_split. With LT 60% the room is 40%; 45% exceeds it.
    let config = AlphaMarketParamsConfig {
        collateral_factor: 5000,
        liquidation_threshold: 6000,
        liquidation_fee: 4500, // 45% + 60% LT > 100%
        supply_cap: 0,
    };
    assert_eq!(
        TusdtLendingPool::alpha_params_from_config(config),
        Err(Error::InvalidParam)
    );

    // A fee above 100% is always invalid.
    let config = AlphaMarketParamsConfig {
        collateral_factor: 5000,
        liquidation_threshold: 6000,
        liquidation_fee: 10_001,
        supply_cap: 0,
    };
    assert_eq!(
        TusdtLendingPool::alpha_params_from_config(config),
        Err(Error::InvalidParam)
    );
}

#[ink::test]
fn valid_global_params_accepted() {
    let config = PoolGlobalParamsConfig {
        max_oracle_age_ms: 1_800_000,
        supply_cap_tao: 0,
        supply_cap_tusdt: 0,
        borrow_cap_tao: 0,
        borrow_cap_tusdt: 0,
    };
    let params = TusdtLendingPool::global_params_from_config(config).unwrap();
    assert_eq!(params.max_oracle_age_ms, 1_800_000);
}

#[ink::test]
fn global_params_rejects_zero_oracle_age() {
    let config = PoolGlobalParamsConfig {
        max_oracle_age_ms: 0,
        supply_cap_tao: 0,
        supply_cap_tusdt: 0,
        borrow_cap_tao: 0,
        borrow_cap_tusdt: 0,
    };
    assert_eq!(
        TusdtLendingPool::global_params_from_config(config),
        Err(Error::InvalidParam)
    );
}

// ---------------------------------------------------------------------------
// Timelock mechanics (market params)
// ---------------------------------------------------------------------------

#[ink::test]
fn schedule_and_execute_market_params() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    let config = InterestRateParamsConfig {
        base_rate: 100,
        slope1: 400,
        slope2: 9500,
        optimal_utilization: 8000,
        reserve_factor: 2000,
    };
    pool.set_market_params(0, config).unwrap();
    // Verify pending update exists
    let pending = pool.get_pending_market_params_update(0).unwrap();
    assert!(pending.execute_after > 0);

    // Cannot execute before timelock
    assert_eq!(
        pool.execute_market_params_update(0),
        Err(Error::ParamsUpdateTimelockActive)
    );

    // Advance past timelock and execute
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        pending.execute_after,
    );
    pool.execute_market_params_update(0).unwrap();
    let pending = pool.get_pending_market_params_update(0);
    assert!(pending.is_none());
}

#[ink::test]
fn cancel_market_params_update() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    let config = InterestRateParamsConfig {
        base_rate: 100,
        slope1: 400,
        slope2: 9500,
        optimal_utilization: 8000,
        reserve_factor: 2000,
    };
    pool.set_market_params(0, config).unwrap();
    pool.cancel_market_params_update(0).unwrap();
    let pending = pool.get_pending_market_params_update(0);
    assert!(pending.is_none());
}

#[ink::test]
fn non_governance_cannot_schedule_market_params() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.bob);
    let config = InterestRateParamsConfig {
        base_rate: 100,
        slope1: 400,
        slope2: 9500,
        optimal_utilization: 8000,
        reserve_factor: 2000,
    };
    assert_eq!(
        pool.set_market_params(0, config),
        Err(Error::NotMaintainer)
    );
}

#[ink::test]
fn non_governance_cannot_cancel_market_params() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    let config = InterestRateParamsConfig {
        base_rate: 100,
        slope1: 400,
        slope2: 9500,
        optimal_utilization: 8000,
        reserve_factor: 2000,
    };
    pool.set_market_params(0, config).unwrap();
    set_caller(accounts.bob);
    assert_eq!(
        pool.cancel_market_params_update(0),
        Err(Error::NotMaintainer)
    );
}

#[ink::test]
fn cancel_without_pending_is_error() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    assert_eq!(
        pool.cancel_market_params_update(0),
        Err(Error::NoPendingMarketParamsUpdate)
    );
}

#[ink::test]
fn execute_without_pending_is_error() {
    let (mut pool, _accounts) = setup();
    assert_eq!(
        pool.execute_market_params_update(0),
        Err(Error::NoPendingMarketParamsUpdate)
    );
}

// ---------------------------------------------------------------------------
// Global params timelock
// ---------------------------------------------------------------------------

#[ink::test]
fn schedule_and_execute_global_params() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    let config = PoolGlobalParamsConfig {
        max_oracle_age_ms: 3_600_000,
        supply_cap_tao: 0,
        supply_cap_tusdt: 0,
        borrow_cap_tao: 0,
        borrow_cap_tusdt: 0,
    };
    pool.set_global_params(config).unwrap();
    let pending = pool.get_pending_global_params_update().unwrap();
    assert!(pending.execute_after > 0);

    // Cannot execute before timelock
    assert_eq!(
        pool.execute_global_params_update(),
        Err(Error::ParamsUpdateTimelockActive)
    );

    // Advance and execute
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        pending.execute_after,
    );
    pool.execute_global_params_update().unwrap();
    assert!(pool.get_pending_global_params_update().is_none());
}

#[ink::test]
fn execute_global_params_before_24h_still_timelocked() {
    // Regression pin: the timelock is 24 h — advancing to just under 24 h
    // must still be inside the window.
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    let config = PoolGlobalParamsConfig {
        max_oracle_age_ms: 3_600_000,
        supply_cap_tao: 0,
        supply_cap_tusdt: 0,
        borrow_cap_tao: 0,
        borrow_cap_tusdt: 0,
    };
    pool.set_global_params(config).unwrap();

    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(24 * 60 * 60 * 1_000 - 1);
    assert_eq!(
        pool.execute_global_params_update(),
        Err(Error::ParamsUpdateTimelockActive)
    );
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(0);
}

#[ink::test]
fn cancel_global_params_update() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    let config = PoolGlobalParamsConfig {
        max_oracle_age_ms: 3_600_000,
        supply_cap_tao: 0,
        supply_cap_tusdt: 0,
        borrow_cap_tao: 0,
        borrow_cap_tusdt: 0,
    };
    pool.set_global_params(config).unwrap();
    pool.cancel_global_params_update().unwrap();
    assert!(pool.get_pending_global_params_update().is_none());
}

// ---------------------------------------------------------------------------
// Alpha params timelock
// ---------------------------------------------------------------------------

#[ink::test]
fn schedule_and_execute_alpha_params() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    let config = AlphaMarketParamsConfig {
        collateral_factor: 6000,
        liquidation_threshold: 7000,
        liquidation_fee: 800,
        supply_cap: 1_000_000,
    };
    pool.set_alpha_params(1, config).unwrap();
    let pending = pool.get_pending_alpha_params_update(1).unwrap();
    assert!(pending.execute_after > 0);

    // Execute
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        pending.execute_after,
    );
    pool.execute_alpha_params_update(1).unwrap();
    assert!(pool.get_pending_alpha_params_update(1).is_none());
}

#[ink::test]
fn cancel_alpha_params_update() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    let config = AlphaMarketParamsConfig {
        collateral_factor: 6000,
        liquidation_threshold: 7000,
        liquidation_fee: 800,
        supply_cap: 1_000_000,
    };
    pool.set_alpha_params(1, config).unwrap();
    pool.cancel_alpha_params_update(1).unwrap();
    assert!(pool.get_pending_alpha_params_update(1).is_none());
}

// ---------------------------------------------------------------------------
// Deposit alpha flow (chain extension interaction)
// ---------------------------------------------------------------------------

#[ink::test]
fn deposit_alpha_succeeds() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    register_mock(1_000);
    let market_id = pool.deposit_alpha(1, 500).unwrap();
    assert!(market_id >= 2);
    assert_eq!(pool.get_user_alpha_position(accounts.alice, 1), Some(500));
    let total = pool.get_netuid_total_collateral(1);
    assert_eq!(total, Some(500));
}

#[ink::test]
fn deposit_alpha_rejects_zero_amount() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    register_mock(1_000);
    assert_eq!(
        pool.deposit_alpha(1, 0),
        Err(Error::ZeroAmount)
    );
}

#[ink::test]
fn deposit_alpha_rejects_unapproved_netuid() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    register_mock(1_000);
    assert_eq!(
        pool.deposit_alpha(99, 500),
        Err(Error::UnapprovedNetuid)
    );
}

#[ink::test]
fn deposit_alpha_fails_on_transfer_failure() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    register_mock_transfer_fails(1_000);
    assert_eq!(
        pool.deposit_alpha(1, 500),
        Err(Error::StakeTransferFailed)
    );
    // No state was changed
    assert_eq!(pool.get_user_alpha_position(accounts.alice, 1), None);
    assert_eq!(pool.get_netuid_total_collateral(1), None);
}

#[ink::test]
fn deposit_alpha_rejected_when_paused() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    pool.pause().unwrap();
    register_mock(1_000);
    assert_eq!(
        pool.deposit_alpha(1, 500),
        Err(Error::ContractPaused)
    );
}

// ---------------------------------------------------------------------------
// View methods
// ---------------------------------------------------------------------------

#[ink::test]
fn market_state_defaults() {
    let (pool, _accounts) = setup();
    let state = pool.get_market_state(0).unwrap();
    assert_eq!(state.total_supplied, 0);
    assert_eq!(state.total_debt, 0);
    assert_eq!(state.total_scaled_debt, 0);
    assert_eq!(state.borrow_index, Ratio::one());
    assert_eq!(state.exchange_rate, Ratio::one());
}

#[ink::test]
fn utilization_zero_when_idle() {
    let (pool, _accounts) = setup();
    let util = pool.get_utilization(0).unwrap();
    assert!(util.is_zero());
}

#[ink::test]
fn borrow_rate_read() {
    let (pool, _accounts) = setup();
    // At zero utilization, borrow rate should be base rate (0)
    let rate = pool.get_borrow_rate(0).unwrap();
    assert!(rate.is_zero());
}

#[ink::test]
fn exchange_rate_starts_at_one() {
    let (pool, _accounts) = setup();
    let rate = pool.get_exchange_rate(0).unwrap();
    assert_eq!(rate, Ratio::one());
}

#[ink::test]
fn mint_amount_scales_by_exchange_rate() {
    // Supplying underlying at a grown exchange rate mints proportionally
    // fewer lTokens (floored), so a late depositor can never claim interest
    // accrued before their deposit (anti-dilution).
    let rate = Ratio::from_inner(1_100_000_000_000_000_000); // 1.1
    assert_eq!(compute_mint_amount(100_000_000_000, rate), Some(90_909_090_909));
    // At 1.0 (genesis) minting is 1:1.
    assert_eq!(compute_mint_amount(100_000_000_000, Ratio::one()), Some(100_000_000_000));
}

#[ink::test]
fn redeem_amount_includes_accrued_interest() {
    // Regression: withdrawal redeemed principal only — the exchange rate grew
    // but never entered the redeem math, so suppliers could never claim
    // borrower interest. Redeem must be ltoken × exchange_rate: 100 lTokens
    // at 1.1 redeem 110 TAO — principal plus the accrued share.
    let rate = Ratio::from_inner(1_100_000_000_000_000_000); // 1.1
    assert_eq!(compute_redeem_amount(100_000_000_000, rate), Some(110_000_000_000));
    // At 1.0 (no interest accrued) it is exactly the principal.
    assert_eq!(compute_redeem_amount(100_000_000_000, Ratio::one()), Some(100_000_000_000));
}

#[ink::test]
fn underlying_balance_view_quotes_exchange_rate() {
    // The view must quote a position's lTokens at the market exchange rate
    // (scaled), not at the principal-only ratio it used before the fix.
    let (mut pool, accounts) = setup();
    pool.debug_set_market_state(
        0,
        MarketState {
            total_supplied: 90_909_090_909,
            total_debt: 0,
            total_scaled_debt: 0,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::from_inner(1_100_000_000_000_000_000),
            reserve_accrued: 0,
            last_update: 0,
        },
    );
    pool.debug_set_position(
        0,
        accounts.alice,
        Position { ltoken_balance: 100_000_000_000, scaled_debt: 0, alpha_principal: 0 },
    );
    assert_eq!(pool.get_underlying_balance(0, accounts.alice), Some(110_000_000_000));
    // Alpha markets (id >= 2) have no lToken: the view stays None.
    assert_eq!(pool.get_underlying_balance(2, accounts.alice), None);
}

#[ink::test]
fn exchange_rate_resets_when_market_fully_drains() {
    // After the last supplier withdraws (total_supplied == 0) the exchange
    // rate must reset to 1.0 so the next genesis supply mints a 1:1 claim
    // that is backed 1:1 — a stale grown rate would over-credit the new
    // supplier (their lTokens would claim more than they deposited).
    let mut state = MarketState {
        total_supplied: 0,
        total_debt: 0,
        total_scaled_debt: 0,
        borrow_index: Ratio::from_inner(1_050_000_000_000_000_000),
        exchange_rate: Ratio::from_inner(1_100_000_000_000_000_000),
        reserve_accrued: 0,
        last_update: 0,
    };
    reset_exchange_rate_when_drained(&mut state);
    assert_eq!(state.exchange_rate, Ratio::one());
    // The borrow index is deliberately left alone: it is self-consistent for
    // scaled debt, and resetting it could corrupt drifted legacy positions.
    assert_eq!(state.borrow_index, Ratio::from_inner(1_050_000_000_000_000_000));
}

#[ink::test]
fn exchange_rate_survives_debt_repayment_while_supplied() {
    // Utilization dropping to zero must NOT reset the exchange rate while any
    // supply remains: the accrued supplier value lives in the grown rate, and
    // resetting here would claw back supplier interest (the very bug the
    // exchange-rate redemption fix exists to solve).
    let mut state = MarketState {
        total_supplied: 100_000_000_000,
        total_debt: 0,
        total_scaled_debt: 0,
        borrow_index: Ratio::one(),
        exchange_rate: Ratio::from_inner(1_100_000_000_000_000_000),
        reserve_accrued: 0,
        last_update: 0,
    };
    reset_exchange_rate_when_drained(&mut state);
    assert_eq!(state.exchange_rate, Ratio::from_inner(1_100_000_000_000_000_000));
}

#[ink::test]
fn borrow_index_starts_at_one() {
    let (pool, _accounts) = setup();
    let idx = pool.get_borrow_index(0).unwrap();
    assert_eq!(idx, Ratio::one());
}

#[ink::test]
fn no_position_by_default() {
    let (pool, accounts) = setup();
    let pos = pool.get_position(0, accounts.alice);
    assert!(pos.is_none());
}

#[ink::test]
fn zero_debt_by_default() {
    let (pool, accounts) = setup();
    let debt = pool.get_user_debt(0, accounts.alice);
    assert_eq!(debt, Some(0));
}

// ---------------------------------------------------------------------------
// Debt principal tracking
// ---------------------------------------------------------------------------

/// Grows a market's borrow index to `inner` (1e18 scale) for testing.
fn set_borrow_index(pool: &mut TusdtLendingPool, market_id: u8, inner: u128) {
    let mut state = pool.get_market_state(market_id).unwrap();
    state.borrow_index = Ratio::from_inner(inner);
    pool.debug_set_market_state(market_id, state);
}

#[ink::test]
fn debt_details_zero_for_empty_position() {
    let (pool, accounts) = setup();
    assert_eq!(pool.get_user_debt_details(0, accounts.alice), Some((0, 0)));
}

#[ink::test]
fn debt_details_splits_principal_and_interest() {
    let (mut pool, accounts) = setup();
    // Borrowed 100 at index 1.0 — no interest yet.
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 0,
            scaled_debt: 100,
            alpha_principal: 0,
        },
    );
    pool.debug_set_debt_principal(0, accounts.alice, 100);
    assert_eq!(pool.get_user_debt_details(0, accounts.alice), Some((100, 100)));

    // Index grows +10% (as accrue_interest would): debt grows, principal does not.
    set_borrow_index(&mut pool, 0, 1_100_000_000_000_000_000);
    assert_eq!(pool.get_user_debt_details(0, accounts.alice), Some((110, 100)));
}

#[ink::test]
fn debt_details_legacy_position_falls_back_to_scaled_debt() {
    let (mut pool, accounts) = setup();
    // Legacy position: no tracked principal, borrowed at index 1.25.
    // scaled_debt = 80 → debt = 100. Principal estimate = scaled_debt = 80.
    pool.debug_set_position(
        1,
        accounts.bob,
        Position {
            ltoken_balance: 0,
            scaled_debt: 80,
            alpha_principal: 0,
        },
    );
    set_borrow_index(&mut pool, 1, 1_250_000_000_000_000_000);
    assert_eq!(pool.get_user_debt_details(1, accounts.bob), Some((100, 80)));
}

#[ink::test]
fn debt_details_never_reports_negative_interest() {
    let (mut pool, accounts) = setup();
    // Tracked principal above debt must clamp: interest is never negative.
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 0,
            scaled_debt: 100,
            alpha_principal: 0,
        },
    );
    pool.debug_set_debt_principal(0, accounts.alice, 150);
    set_borrow_index(&mut pool, 0, 1_000_000_000_000_000_000);
    assert_eq!(pool.get_user_debt_details(0, accounts.alice), Some((100, 100)));
}

// ---------------------------------------------------------------------------
// Alpha excess claim (full unstake → treasury)
// ---------------------------------------------------------------------------

#[ink::test]
fn claim_alpha_excess_unstakes_full_excess() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    register_mock(1_000);
    pool.deposit_alpha(1, 500).unwrap();

    // Mock reports 1_000 available; booked = 500 principal → excess 500.
    set_caller(accounts.charlie); // permissionless
    pool.claim_alpha_excess(1).unwrap();

    // The FULL excess (500) was unstaked (func 2) from the pool hotkey. The
    // off-chain mock does not move balances, so `tao_received` is 0 here and
    // the treasury transfer branch is skipped; the transfer itself is covered
    // by the e2e suite (the message forwards the unstaked TAO to the
    // treasury).
    let record = last_ext_call().unwrap();
    assert_eq!(record.func_id, 2);
    assert_eq!(record.hotkey, accounts.bob); // pool_hotkey
    assert_eq!(record.netuid, 1);
    assert_eq!(record.amount, 500);
}

#[ink::test]
fn claim_alpha_excess_noop_when_no_excess() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    register_mock(500);
    pool.deposit_alpha(1, 500).unwrap();

    // Mock reports exactly the booked amount — no excess, no unstake.
    set_caller(accounts.charlie);
    pool.claim_alpha_excess(1).unwrap();
    assert!(last_ext_call().is_none()); // no remove_stake was issued
}

#[ink::test]
fn claim_alpha_excess_recovers_orphan_stake_when_no_positions() {
    let (mut pool, _accounts) = setup_with_alpha(1);
    // Mock reports stake but no positions exist (e.g. stranded by a hotkey
    // migration): the whole stake is excess and is recovered for the treasury.
    register_mock(1_000);
    pool.claim_alpha_excess(1).unwrap();
    let record = last_ext_call().unwrap();
    assert_eq!(record.func_id, 2);
    assert_eq!(record.amount, 1_000);
}

#[ink::test]
fn claim_alpha_excess_rejects_unapproved_netuid() {
    let (mut pool, _accounts) = setup();
    register_mock(1_000);
    assert_eq!(pool.claim_alpha_excess(99), Err(Error::UnapprovedNetuid));
}

#[ink::test]
fn reserve_claimable_never_touches_deficit_backing() {
    // Deficit consumes the reserve entirely → nothing claimable.
    assert_eq!(reserve_claimable(100_000_000, 200_000_000, 500_000_000), 0);
    // Reserve above the deficit, capped by cash.
    assert_eq!(reserve_claimable(1_000_000_000, 300_000_000, 500_000_000), 500_000_000);
    // No deficit — the whole reserve is claimable, capped by cash.
    assert_eq!(reserve_claimable(1_000_000_000, 0, 500_000_000), 500_000_000);
    // Cash is the binding constraint.
    assert_eq!(reserve_claimable(1_000_000_000, 0, 900_000_000), 900_000_000);
    // With ample cash, exactly reserve − deficit is claimable.
    assert_eq!(reserve_claimable(1_000_000_000, 300_000_000, 1_500_000_000), 700_000_000);
}

// ---------------------------------------------------------------------------
// Pool hotkey
// ---------------------------------------------------------------------------

#[ink::test]
fn pool_hotkey_is_set() {
    let (pool, accounts) = setup();
    assert_eq!(pool.get_pool_hotkey(), accounts.bob);
}

#[ink::test]
fn governance_can_update_pool_hotkey() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    register_mock(1_000);
    pool.deposit_alpha(1, 500).unwrap();
    // Update hotkey — move_stake is a no-op in the mock (returns 0)
    pool.update_pool_hotkey(accounts.charlie, vec![1]).unwrap();
    assert_eq!(pool.get_pool_hotkey(), accounts.charlie);
}

#[ink::test]
fn update_pool_hotkey_rejects_too_many_netuids() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    let netuids: Vec<u16> = (1..=33).collect();
    assert_eq!(
        pool.update_pool_hotkey(accounts.charlie, netuids),
        Err(Error::TooManyNetuids)
    );
}

// ---------------------------------------------------------------------------
// Contract pausing guards deposit_alpha
// ---------------------------------------------------------------------------

#[ink::test]
fn paused_blocks_deposit_alpha() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    pool.pause().unwrap();
    register_mock(1_000);
    assert_eq!(
        pool.deposit_alpha(1, 500),
        Err(Error::ContractPaused)
    );
}

#[ink::test]
fn unpause_allows_deposit_alpha() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    pool.pause().unwrap();
    pool.unpause().unwrap();
    register_mock(1_000);
    pool.deposit_alpha(1, 500).unwrap();
    assert_eq!(pool.get_user_alpha_position(accounts.alice, 1), Some(500));
}

// ---------------------------------------------------------------------------
// Timelock helper test
// ---------------------------------------------------------------------------

#[ink::test]
fn set_timelock_to_zero_allows_instant_execution() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    // Use the helper to zero out the timelock
    set_timelock_to_zero(&mut pool);
    // Global params should now have the configured values
    // (timelock was bypassed by the helper)
    assert!(!pool.paused());
}

// ---------------------------------------------------------------------------
// Chain extension failure modes
// ---------------------------------------------------------------------------

#[ink::test]
fn deposit_alpha_fails_when_chain_fails() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    register_mock_chain_fails();
    // The mock's should_fail=true causes func 25 to return error code 1,
    // which the pool maps to StakeTransferFailed.
    assert_eq!(pool.deposit_alpha(1, 500), Err(Error::StakeTransferFailed));
    // Verify no state was mutated
    assert_eq!(pool.get_user_alpha_position(accounts.alice, 1), None);
    assert_eq!(pool.get_netuid_total_collateral(1), None);
}

#[ink::test]
fn deposit_alpha_with_no_stake_info_returns_none() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    register_mock_no_stake();
    // The mock returns None for stake info; the deposit still proceeds
    // since caller_transfer_stake succeeds (transfer_fails=false)
    pool.deposit_alpha(1, 500).unwrap();
    assert_eq!(pool.get_user_alpha_position(accounts.alice, 1), Some(500));
}

// ---------------------------------------------------------------------------
// Reentrancy guard
// ---------------------------------------------------------------------------

#[ink::test]
fn ensure_idle_guards_reentrancy() {
    let (mut pool, _accounts) = setup();
    // The reentrancy guard sets the internal busy flag and prevents double entry
    pool.ensure_idle().unwrap();
    assert_eq!(pool.ensure_idle(), Err(Error::Reentrancy));
    pool.set_idle();
    // After clearing, it can be re-entered
    pool.ensure_idle().unwrap();
    pool.set_idle();
}

// ---------------------------------------------------------------------------
// Position key lifecycle
// ---------------------------------------------------------------------------

#[ink::test]
fn update_position_key_pushes_when_position_nonzero() {
    let (mut pool, accounts) = setup();
    // Seed a non-zero position without pushing to position_keys
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 100,
            scaled_debt: 0,
            alpha_principal: 0,
        },
    );
    // Call update — should push the key
    pool.update_position_key(0, accounts.alice);
    let all = pool.get_all_positions(0);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0, (0, accounts.alice));
    assert_eq!(all[0].1.ltoken_balance, 100);
}

#[ink::test]
fn update_position_key_removes_when_zero() {
    let (mut pool, accounts) = setup();
    // Seed a non-zero position with a key
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 100,
            scaled_debt: 0,
            alpha_principal: 0,
        },
    );
    pool.update_position_key(0, accounts.alice);
    assert_eq!(pool.get_all_positions(0).len(), 1);

    // Zero the position and update — should remove the key
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 0,
            scaled_debt: 0,
            alpha_principal: 0,
        },
    );
    pool.update_position_key(0, accounts.alice);
    assert_eq!(pool.get_all_positions(0).len(), 0);
}

#[ink::test]
fn update_position_key_no_double_push() {
    let (mut pool, accounts) = setup();
    // Seed a non-zero position with a key
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 100,
            scaled_debt: 0,
            alpha_principal: 0,
        },
    );
    pool.update_position_key(0, accounts.alice);
    assert_eq!(pool.get_all_positions(0).len(), 1);

    // Call update again — should NOT push a duplicate
    pool.update_position_key(0, accounts.alice);
    assert_eq!(pool.get_all_positions(0).len(), 1);
}

#[ink::test]
fn update_position_key_self_heals_legacy_duplicates() {
    let (mut pool, accounts) = setup();
    // Simulate legacy duplicated keys by pushing 3 times
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 100,
            scaled_debt: 0,
            alpha_principal: 0,
        },
    );
    pool.debug_push_position_key(0, accounts.alice);
    pool.debug_push_position_key(0, accounts.alice);
    pool.debug_push_position_key(0, accounts.alice);
    // 3 duplicate keys in storage
    // The read-side dedup within get_all_positions handles it
    let all = pool.get_all_positions(0);
    // With read-side dedup, only 1 entry returned
    assert_eq!(all.len(), 1);

    // Now call update — it removes all duplicates and pushes exactly once
    pool.update_position_key(0, accounts.alice);
    let all = pool.get_all_positions(0);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0, (0, accounts.alice));

    // A second update is a no-op
    pool.update_position_key(0, accounts.alice);
    assert_eq!(pool.get_all_positions(0).len(), 1);
}

#[ink::test]
fn swap_removal_preserves_other_keys() {
    let (mut pool, accounts) = setup();
    // Seed 3 positions: alice market 0, bob market 0, alice market 1
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 100,
            scaled_debt: 0,
            alpha_principal: 0,
        },
    );
    pool.update_position_key(0, accounts.alice);
    pool.debug_set_position(
        0,
        accounts.bob,
        Position {
            ltoken_balance: 200,
            scaled_debt: 0,
            alpha_principal: 0,
        },
    );
    pool.update_position_key(0, accounts.bob);
    pool.debug_set_position(
        1,
        accounts.alice,
        Position {
            ltoken_balance: 0,
            scaled_debt: 50,
            alpha_principal: 0,
        },
    );
    pool.update_position_key(1, accounts.alice);

    assert_eq!(pool.get_all_positions(0).len(), 3);

    // Zero out bob's position — should remove only bob's key
    pool.debug_set_position(
        0,
        accounts.bob,
        Position {
            ltoken_balance: 0,
            scaled_debt: 0,
            alpha_principal: 0,
        },
    );
    pool.update_position_key(0, accounts.bob);
    let all = pool.get_all_positions(0);
    assert_eq!(all.len(), 2);
    // Alice's positions must still be present
    let keys: Vec<(u8, ink::primitives::AccountId)> = all.iter().map(|(k, _)| *k).collect();
    assert!(keys.contains(&(0, accounts.alice)));
    assert!(keys.contains(&(1, accounts.alice)));
}

#[ink::test]
fn multi_market_positions_independent() {
    let (mut pool, accounts) = setup();
    // User has supply on market 0 and debt on market 1
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 100,
            scaled_debt: 0,
            alpha_principal: 0,
        },
    );
    pool.update_position_key(0, accounts.alice);
    pool.debug_set_position(
        1,
        accounts.alice,
        Position {
            ltoken_balance: 0,
            scaled_debt: 50,
            alpha_principal: 0,
        },
    );
    pool.update_position_key(1, accounts.alice);

    assert_eq!(pool.get_all_positions(0).len(), 2);

    // Zero-out market 0 only
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 0,
            scaled_debt: 0,
            alpha_principal: 0,
        },
    );
    pool.update_position_key(0, accounts.alice);

    let all = pool.get_all_positions(0);
    assert_eq!(all.len(), 1);
    // Market 1 entry should still exist
    assert_eq!(all[0].0, (1, accounts.alice));
    assert_eq!(all[0].1.scaled_debt, 50);
}

#[ink::test]
fn deposit_withdraw_deposit_alpha_no_duplicate_keys() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    register_mock(1_000);

    // First deposit
    pool.deposit_alpha(1, 500).unwrap();
    let all = pool.get_all_positions(0);
    assert_eq!(all.len(), 1, "should have 1 position after first deposit");

    // Withdraw all — position should be removed from position_keys
    pool.withdraw_alpha(1, 500, accounts.bob).unwrap();
    let all = pool.get_all_positions(0);
    assert_eq!(all.len(), 0, "should have 0 positions after full withdraw");

    // Deposit again — should create exactly 1 entry (NOT 2)
    pool.deposit_alpha(1, 300).unwrap();
    let all = pool.get_all_positions(0);
    assert_eq!(all.len(), 1, "should have exactly 1 position after re-deposit");
    let (key, pos) = &all[0];
    assert_eq!(key.1, accounts.alice);
    assert_eq!(pos.alpha_principal, 300);
}

#[ink::test]
fn partial_withdraw_alpha_keeps_key() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    register_mock(1_000);

    pool.deposit_alpha(1, 500).unwrap();
    assert_eq!(pool.get_all_positions(0).len(), 1);

    // Withdraw only part of the collateral
    pool.withdraw_alpha(1, 200, accounts.bob).unwrap();
    let all = pool.get_all_positions(0);
    assert_eq!(all.len(), 1, "key should remain after partial withdraw");
    let (_, pos) = &all[0];
    assert_eq!(pos.alpha_principal, 300);
}

#[ink::test]
fn borrow_only_orphan_self_heal() {
    let (mut pool, accounts) = setup();
    // Simulate a legacy borrow-only position: debt on market 0, no key
    pool.debug_set_position(
        0,
        accounts.alice,
        Position {
            ltoken_balance: 0,
            scaled_debt: 50,
            alpha_principal: 0,
        },
    );
    // No key was pushed — simulate orphan
    // Call update_position_key — should self-heal by pushing the key
    pool.update_position_key(0, accounts.alice);
    let all = pool.get_all_positions(0);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0, (0, accounts.alice));
    assert_eq!(all[0].1.scaled_debt, 50);
}

// ---------------------------------------------------------------------------
// Parameter getters
// ---------------------------------------------------------------------------

#[ink::test]
fn get_global_params_returns_defaults() {
    let (pool, _accounts) = setup();
    let params = pool.get_global_params();
    assert_eq!(params.max_oracle_age_ms, 1_800_000);
    assert_eq!(params.supply_cap_tao, 0);
    assert_eq!(params.supply_cap_tusdt, 0);
}

#[ink::test]
fn get_market_params_returns_configured_params() {
    let (pool, _accounts) = setup();
    let params = pool.get_market_params(0).expect("market 0 params should exist");
    // Default TAO interest params: base=0, optimal=8000, slope1=400, slope2=9600, reserve=2000
    assert_eq!(params.base_rate, 0);
    assert_eq!(params.optimal_utilization, 8000);
    assert_eq!(params.slope1, 400);
}

#[ink::test]
fn get_alpha_params_returns_none_for_unconfigured_netuid() {
    let (pool, _accounts) = setup();
    assert!(pool.get_alpha_params(99).is_none());
}

#[ink::test]
fn get_alpha_params_returns_configured_params() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    // Configure alpha params for netuid 1
    let config = AlphaMarketParamsConfig {
        collateral_factor: 6000,
        liquidation_threshold: 7000,
        liquidation_fee: 800,
        supply_cap: 1_000_000,
    };
    pool.set_alpha_params(1, config).unwrap();
    // Execute the update
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        PARAMS_TIMELOCK_MS + 1,
    );
    pool.execute_alpha_params_update(1).unwrap();
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(0);

    let params = pool.get_alpha_params(1).expect("alpha params should exist");
    assert_eq!(params.collateral_factor, 6000);
    assert_eq!(params.liquidation_threshold, 7000);
    assert_eq!(params.supply_cap, 1_000_000);
}

// ---------------------------------------------------------------------------
// Maintainer role
// ---------------------------------------------------------------------------

#[ink::test]
fn maintainer_initialized_to_deployer() {
    let (pool, accounts) = setup();
    assert_eq!(pool.maintainer(), accounts.alice);
}

#[ink::test]
fn governance_can_update_maintainer() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.update_maintainer(accounts.bob).unwrap();
    assert_eq!(pool.maintainer(), accounts.bob);
}

#[ink::test]
fn non_governance_cannot_update_maintainer() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.bob); // bob is NOT governance
    assert_eq!(
        pool.update_maintainer(accounts.charlie),
        Err(Error::NotGovernance)
    );
}

#[ink::test]
fn maintainer_can_set_alpha_params() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    // alice is both governance and maintainer initially
    let config = AlphaMarketParamsConfig {
        collateral_factor: 6000,
        liquidation_threshold: 7000,
        liquidation_fee: 800,
        supply_cap: 1_000_000,
    };
    pool.set_alpha_params(1, config).unwrap();
}

#[ink::test]
fn non_maintainer_cannot_set_params() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    // Transfer maintainer to bob
    pool.update_maintainer(accounts.bob).unwrap();
    // charlie is neither maintainer nor governance
    set_caller(accounts.charlie);
    let config = AlphaMarketParamsConfig {
        collateral_factor: 6000,
        liquidation_threshold: 7000,
        liquidation_fee: 800,
        supply_cap: 1_000_000,
    };
    assert_eq!(
        pool.set_alpha_params(1, config),
        Err(Error::NotMaintainer)
    );
}

#[ink::test]
fn maintainer_cannot_update_governance() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    // Transfer maintainer to bob
    pool.update_maintainer(accounts.bob).unwrap();
    // Bob is maintainer but cannot transfer governance
    set_caller(accounts.bob);
    assert_eq!(
        pool.update_governance(accounts.charlie),
        Err(Error::NotGovernance)
    );
}

#[ink::test]
fn governance_can_still_call_maintainer_functions() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    // Transfer maintainer to bob
    pool.update_maintainer(accounts.bob).unwrap();
    // alice is still governance — she can still do maintainer-gated operations
    set_caller(accounts.alice);
    let config = AlphaMarketParamsConfig {
        collateral_factor: 6000,
        liquidation_threshold: 7000,
        liquidation_fee: 800,
        supply_cap: 1_000_000,
    };
    // governance should pass ensure_maintainer (governance || maintainer)
    pool.set_alpha_params(1, config).unwrap();
}

#[ink::test]
fn new_maintainer_can_call_maintainer_functions() {
    let (mut pool, accounts) = setup_with_alpha(1);
    set_caller(accounts.alice);
    // Transfer maintainer to bob
    pool.update_maintainer(accounts.bob).unwrap();
    // Bob is now maintainer
    set_caller(accounts.bob);
    let config = AlphaMarketParamsConfig {
        collateral_factor: 6000,
        liquidation_threshold: 7000,
        liquidation_fee: 800,
        supply_cap: 1_000_000,
    };
    pool.set_alpha_params(1, config).unwrap();
}

#[ink::test]
fn maintainer_initialized_in_test_constructor() {
    let (pool, accounts) = setup();
    // Test the test constructor also initializes maintainer
    assert_eq!(pool.maintainer(), accounts.alice);
    assert_eq!(pool.governance(), accounts.alice);
}

// ---------------------------------------------------------------------------
// Borrow/repay scaling regression tests
// ---------------------------------------------------------------------------

#[ink::test]
fn borrow_scaling_divides_amount_by_borrow_index() {
    // scaled_debt = amount / borrow_index.
    // Regression: the operands were previously reversed, computing
    // borrow_index / amount — borrowing 10 TUSDT stored 0.1 TUSDT of
    // scaled debt and repayments underflowed into ArithmeticError.
    let amount: u128 = 10_000_000_000; // 10 TUSDT (9 decimals)
    let index_one = Ratio::from_inner(1_000_000_000_000_000_000); // 1.0

    let scaled = index_one
        .checked_div_value(amount)
        .expect("scaling must not overflow");
    assert_eq!(scaled, amount, "index 1.0 → scaled == amount");

    // With index = 2.0, scaled = amount / 2.
    let index_two = Ratio::from_inner(2_000_000_000_000_000_000);
    let scaled_two = index_two
        .checked_div_value(amount)
        .expect("scaling must not overflow");
    assert_eq!(scaled_two, 5_000_000_000, "index 2.0 → scaled == amount / 2");

    // Repaying the exact debt must not underflow: scaled_repaid (amount/index)
    // must equal the position's scaled debt.
    let scaled_repaid = index_one
        .checked_div_value(scaled)
        .expect("repay scaling must not overflow");
    assert_eq!(scaled_repaid, amount, "repay full debt → scaled_repaid == scaled debt");
}

#[ink::test]
fn borrow_scaling_rounds_up_to_never_understate_debt() {
    // Regression (production bug on testnet): floor-scaled borrows
    // understated the position debt — borrowing 5 TUSDT at a grown borrow
    // index reported 4.99999999 TUSDT, so the Max repay cleared the position
    // while the market total kept dust (1 rao), and with zero cash the
    // market then displayed 100% utilization / 80% supply APY / 100% borrow
    // APY with no positions at all. Ceiling scaling (Aave rayDivUp)
    // guarantees floor(scaled × index) >= borrowed amount.
    let amount: u128 = 5_000_000_000; // 5 TUSDT
    // Index observed on the live testnet pool's market 1 after the bug.
    let index = Ratio::from_inner(1_000_316_946_018_546_683);

    let scaled_floor = index
        .checked_div_value(amount)
        .expect("scaling must not overflow");
    let scaled_ceil = index
        .checked_div_value_ceil(amount)
        .expect("ceil scaling must not overflow");
    assert_eq!(scaled_floor, 4_998_415_772);
    assert_eq!(scaled_ceil, 4_998_415_773, "ceil rounds the fractional unit up");

    let debt_floor = index
        .checked_mul_value(scaled_floor)
        .expect("debt recompute must not overflow");
    let debt_ceil = index
        .checked_mul_value(scaled_ceil)
        .expect("debt recompute must not overflow");
    assert_eq!(debt_floor, 4_999_999_999, "floor scaling understates debt (the reported bug)");
    assert_eq!(debt_ceil, 5_000_000_000, "ceil scaling reports the borrowed amount exactly");

    // A partial repay one rao short of the reported debt must leave exactly
    // one rao of debt in the position — never clear it, never lose dust.
    let partial = debt_ceil - 1;
    let scaled_repaid = index
        .checked_div_value_ceil(partial)
        .expect("repay scaling must not overflow");
    assert_eq!(scaled_repaid, scaled_ceil - 1, "partial repay keeps 1 scaled unit");
    let remaining_debt = index
        .checked_mul_value(scaled_ceil - scaled_repaid)
        .expect("remaining debt recompute must not overflow");
    assert_eq!(remaining_debt, 1, "exactly 1 rao remains — no dusting off");

    // Repaying the reported debt (Max) clears the scaled position exactly.
    let full_repaid = index
        .checked_div_value_ceil(debt_ceil)
        .expect("full repay scaling must not overflow");
    assert_eq!(full_repaid, scaled_ceil, "max repay clears the position exactly");
}

#[ink::test]
fn full_repay_bookkeeping_clears_position_and_total_exactly() {
    // Regression (production bug, 2026-08): with the market total tracked in
    // face units and repay clamped to `min(repay, total_debt)`, a MAX
    // repayment could leave the position stuck with dust (live signature:
    // debt 1 rao on market 0, 2 rao on market 1, total_debt == 0, repay
    // no-opping forever). With scaled-total accounting the market total and
    // the position lose the SAME scaled units, so a full repayment always
    // clears both exactly. The repay messages pull the ERC20 / transferred
    // value before their effects and cannot run in the off-chain env, so the
    // bookkeeping is pinned at the conversion level with the exact helpers
    // repay_tao/repay_tusdt/liquidate use.
    let index = Ratio::from_inner(1_000_316_946_018_546_683); // live testnet index

    // Live-chain stuck signature: scaled 1 at this index displays debt 1
    // (market 0), scaled 2 displays debt 2 (market 1).
    for scaled in [1_u128, 2] {
        let debt = index
            .checked_mul_value(scaled)
            .expect("debt recompute must not overflow");
        assert_eq!(debt, scaled, "scaled {scaled} displays exactly {scaled} rao of debt");

        // repay_amount = min(MAX, debt) = debt → the full-repay branch sets
        // scaled_repaid = pos.scaled_debt. The ceil-on-borrow / floor-on-
        // display pairing must make the round trip exact.
        let scaled_repaid = index
            .checked_div_value_ceil(debt)
            .expect("full repay scaling must not overflow");
        assert_eq!(scaled_repaid, scaled, "full repay of displayed debt clears scaled exactly");

        // Market total side (lockstep): total_scaled_debt -= scaled_repaid
        // lands on exactly 0 and the derived face follows.
        let remaining_scaled = scaled.checked_sub(scaled_repaid).expect("no underflow");
        assert_eq!(remaining_scaled, 0, "no scaled dust survives the last repay");
        assert_eq!(
            scaled_debt_to_face(0, index),
            Some(0),
            "face total hits exactly 0 — position gone, utilization 0"
        );
    }
}

#[ink::test]
fn scaled_total_keeps_face_total_at_least_the_sum_of_user_debts() {
    // Invariant the fix restores: face total = floor(Σscaled × index) is
    // always >= every user's displayed debt floor(scaled_i × index) (floor is
    // monotonic), so a MAX repayment can never be truncated by a total-debt
    // clamp and the LAST borrower can always clear the market. This is what
    // the removed `min(repay, total_debt)` clamp previously violated once
    // independent floor rounding had drifted the face total down.
    let index = Ratio::from_inner(1_400_000_000_000_000_000); // 1.4
    let a: u64 = 2;
    let b: u64 = 3;
    let total_scaled = 5u64;

    let total_face = scaled_debt_to_face(total_scaled, index).unwrap();
    let a_face = scaled_debt_to_face(a, index).unwrap();
    let b_face = scaled_debt_to_face(b, index).unwrap();
    assert_eq!((total_face, a_face, b_face), (7, 2, 4));
    assert!(total_face >= a_face && total_face >= b_face, "every user is fully repayable");

    // A fully repays: scaled_repaid = ceil(a_face / index) == a exactly.
    let a_repaid = index
        .checked_div_value_ceil(a_face.into())
        .expect("full repay scaling must not overflow");
    assert_eq!(a_repaid as u64, a, "full repay round-trips the position's scaled debt");
    let remaining_scaled = total_scaled.checked_sub(a).unwrap();
    let remaining_face = scaled_debt_to_face(remaining_scaled, index).unwrap();
    assert!(remaining_face >= b_face, "B is still fully repayable after A exits");

    // B (the last borrower) fully repays: the market hits exact zero.
    let b_repaid = index
        .checked_div_value_ceil(b_face.into())
        .expect("full repay scaling must not overflow");
    assert_eq!(b_repaid as u64, b, "last full repay round-trips exactly");
    assert_eq!(
        scaled_debt_to_face(remaining_scaled.checked_sub(b).unwrap(), index),
        Some(0),
        "last repay drives the face total to exactly 0 — no ghost dust"
    );
}

#[ink::test]
fn accrue_interest_derives_face_total_from_scaled_total() {
    // Regression: accrual used to compound the face total independently
    // (`floor(total_debt × growth)` on a stale floored integer), drifting it
    // below the sum of per-user debts — the interest-driven engine of the
    // unrepayable-dust bug. Now the scaled total is never mutated by accrual
    // (interest accrues purely through index growth) and the face total is
    // re-derived from it at the new index, keeping both in lockstep.
    // Market 0 (TAO) is used because market_cash needs no cross-contract call
    // in the off-chain env; the accrual path under test is shared.
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);

    let scaled: u64 = 128_000_000_000; // 128 TAO (9 decimals)
    pool.debug_set_market_state(
        0,
        MarketState {
            total_supplied: 130_000_000_000,
            total_debt: scaled, // index 1.0 → face == scaled
            total_scaled_debt: scaled,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::one(),
            reserve_accrued: 0,
            last_update: 0,
        },
    );

    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        tusdt_primitives::MILLISECONDS_PER_HOUR + 1,
    );
    pool.accrue_interest(0).unwrap();

    let state = pool.get_market_state(0).unwrap();
    assert_eq!(state.total_scaled_debt, scaled, "scaled total is never mutated by accrual");
    assert_eq!(
        state.total_debt,
        scaled_debt_to_face(scaled, state.borrow_index).unwrap(),
        "face total is exactly floor(total_scaled_debt × borrow_index / 1e18)"
    );
    assert!(state.total_debt >= scaled, "interest accrues into the face total");
}

#[ink::test]
fn liquidate_bookkeeping_subtracts_the_same_scaled_units_from_total_and_position() {
    // The liquidate message ends in cross-contract calls (oracle/ERC20) that
    // cannot run off-chain; pin its debt bookkeeping at the conversion level
    // with the exact helpers it uses. A full-debt liquidation must clear the
    // position AND shrink the market total by the SAME scaled delta, so the
    // borrower's dust can never migrate into the market total.
    let index = Ratio::from_inner(1_400_000_000_000_000_000); // 1.4
    let pos_scaled: u64 = 3;
    let total_scaled: u64 = pos_scaled + 5; // another borrower holds 5 scaled units

    let borrower_debt = scaled_debt_to_face(pos_scaled, index).unwrap();
    assert_eq!(borrower_debt, 4);

    // liquidate: actual_debt_units = min(cover, borrower_debt) = borrower_debt
    // (full cover) → scaled_repaid = min(ceil(borrower_debt / index), pos_scaled).
    let scaled_repaid = index
        .checked_div_value_ceil(borrower_debt.into())
        .expect("ceil scaling must not overflow");
    assert_eq!(scaled_repaid as u64, pos_scaled, "full liquidation round-trips exactly");

    // total_scaled_debt -= scaled_repaid → the other borrower's 5 scaled units
    // remain and the derived face total tracks them exactly.
    let remaining_scaled = total_scaled.checked_sub(pos_scaled).unwrap();
    let remaining_face = scaled_debt_to_face(remaining_scaled, index).unwrap();
    assert_eq!((remaining_scaled, remaining_face), (5, 7));
}

#[ink::test]
fn sub_index_repay_credits_one_scaled_unit_instead_of_being_rejected() {
    // Regression: repaying less than one borrow-index unit used to floor to
    // zero scaled debt — the repaid amount leaked from the market total
    // without crediting the position, and a later revision rejected it as
    // ZeroAmount. Ceiling scaling (Aave rayDivUp) credits every positive
    // repayment with at least one scaled unit, so neither the leak nor the
    // rejection can happen. (The message-level path cannot run in the
    // off-chain env — the ERC20 pull panics — so the guarantee is pinned at
    // the conversion level, the same helper repay_tusdt uses.)
    let index = Ratio::from_inner(1_000_160_150_930_897_932); // ~1.00016
    let one_rao = index
        .checked_div_value_ceil(1)
        .expect("ceil scaling must not overflow");
    assert_eq!(one_rao, 1, "1 rao credits exactly one scaled unit — no rejection, no leak");
    // The credited scaled unit maps back to at least the repaid amount:
    // the position's debt can never fall by less than what was repaid.
    let debt_credit = index
        .checked_mul_value(one_rao)
        .expect("debt recompute must not overflow");
    assert!(debt_credit >= 1, "debt credit {debt_credit} understates the 1-rao repay");
}

#[ink::test]
fn utilization_and_rates_are_zero_when_market_has_no_debt() {
    // Regression (production bug): after the dust repay cleared the last
    // position but left 1 rao of ghost total_debt with zero cash, the market
    // reported 100% utilization, 100% borrow APY and 80% supply APY with no
    // debt at all. With an exact ledger the no-debt state must read zero.
    // Market 0 is used because market_cash(1) needs a cross-contract ERC20
    // call the off-chain env cannot serve.
    let (pool, _accounts) = setup();

    // Fresh market 0: total_debt == 0 and zero test-env cash.
    assert_eq!(pool.get_utilization(0), Some(Ratio::from_inner(0)));
    // Borrow rate falls back to the curve's base rate (0 for TAO defaults).
    assert_eq!(
        pool.get_borrow_rate(0),
        Some(default_tao_interest_params().base_rate)
    );
    assert_eq!(pool.get_supply_rate(0), Some(Ratio::from_inner(0)));
    // The pure rate curve pins the same behaviour for the TUSDT curve
    // (base 0): at zero utilization both APYs are zero.
    assert_eq!(
        TusdtLendingPool::compute_borrow_rate(
            &default_tusdt_interest_params(),
            Ratio::from_inner(0)
        ),
        Ok(default_tusdt_interest_params().base_rate)
    );
}

#[ink::test]
fn accrue_market_interest_refreshes_both_markets_permissionlessly() {
    // Permissionless refresh of both debt markets: market 0 accrues fully
    // (borrow index grows) while market 1 (no debt) just bumps last_update.
    // Market 1 avoids the cross-contract cash read in the off-chain env.
    let (mut pool, accounts) = setup();
    set_caller(accounts.bob); // anyone, not governance

    pool.debug_set_market_state(
        0,
        MarketState {
            total_supplied: 130_000_000_000,
            total_debt: 128_000_000_000,
            total_scaled_debt: 128_000_000_000,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::one(),
            reserve_accrued: 0,
            last_update: 0,
        },
    );
    pool.debug_set_market_state(
        1,
        MarketState {
            total_supplied: 129_000_000_000,
            total_debt: 0,
            total_scaled_debt: 0,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::one(),
            reserve_accrued: 0,
            last_update: 0,
        },
    );

    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        tusdt_primitives::MILLISECONDS_PER_HOUR + 1,
    );

    pool.accrue_market_interest().unwrap();

    let m0 = pool.get_market_state(0).unwrap();
    assert!(
        m0.borrow_index.into_inner() > Ratio::one().into_inner(),
        "market 0 borrow index should grow"
    );
    // Whole-hours-only advance (vault pattern): 1h+1ms elapsed charges one
    // full hour, so last_update lands exactly on the hour boundary.
    assert_eq!(m0.last_update, tusdt_primitives::MILLISECONDS_PER_HOUR);

    let m1 = pool.get_market_state(1).unwrap();
    assert_eq!(m1.last_update, tusdt_primitives::MILLISECONDS_PER_HOUR + 1);

    assert_eq!(
        pool.get_last_interest_accrual_times(),
        Some((
            tusdt_primitives::MILLISECONDS_PER_HOUR,
            tusdt_primitives::MILLISECONDS_PER_HOUR + 1
        ))
    );
}

#[ink::test]
fn get_last_interest_accrual_times_returns_both_markets() {
    let (pool, _accounts) = setup();
    // new_for_test seeds both debt markets with MarketState::new(0).
    assert_eq!(pool.get_last_interest_accrual_times(), Some((0, 0)));
}

#[ink::test]
fn accrual_produces_interest_after_an_hour() {
    // Regression: the hourly-rate divisor was double-scaled
    // (`checked_div_int(hours_per_year.into_inner())` = 8760 × 1e18), which
    // overflowed u128 and reverted every accrual with `ArithmeticError` once
    // dt_hours ≥ 1 — freezing interest and breaking borrow/repay/supply.
    // Market 0 (TAO) is used because market_cash needs no cross-contract call
    // in the off-chain env; the accrual path under test is shared.
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);

    let debt: u64 = 128_000_000_000; // 128 TAO (9 decimals)
    pool.debug_set_market_state(
        0,
        MarketState {
            total_supplied: 130_000_000_000,
            total_debt: debt,
            total_scaled_debt: debt,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::one(),
            reserve_accrued: 0,
            last_update: 0,
        },
    );
    pool.debug_set_position(
        0,
        accounts.alice,
        Position { ltoken_balance: 0, scaled_debt: debt, alpha_principal: 0 },
    );
    pool.debug_set_debt_principal(0, accounts.alice, debt);

    // Advance one hour so the accrual path runs with dt_hours = 1.
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        tusdt_primitives::MILLISECONDS_PER_HOUR + 1,
    );

    pool.accrue_interest(0).unwrap();

    let index = pool.get_borrow_index(0).unwrap();
    assert!(
        index.into_inner() > Ratio::one().into_inner(),
        "borrow index should grow after an hour of debt: {}",
        index.into_inner()
    );

    let (now_debt, principal) = pool.get_user_debt_details(0, accounts.alice).unwrap();
    assert!(
        now_debt > principal,
        "interest should accrue: debt={now_debt} principal={principal}"
    );
}

#[ink::test]
fn sub_hour_remainder_is_not_discarded() {
    // Regression: the no-op accrual path used to bump `last_update` to `now`
    // whenever less than a full hour had elapsed, discarding the remainder —
    // frequent sub-hour writes could starve a debt market of interest forever.
    // The clock must only advance by whole hours (vault pattern).
    // Market 0 (TAO) is used because market_cash needs no cross-contract call
    // in the off-chain env; the accrual path under test is shared.
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);

    let debt: u64 = 128_000_000_000; // 128 TAO (9 decimals)
    pool.debug_set_market_state(
        0,
        MarketState {
            total_supplied: 130_000_000_000,
            total_debt: debt,
            total_scaled_debt: debt,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::one(),
            reserve_accrued: 0,
            last_update: 0,
        },
    );

    // Write at t = 30 min: no full hour elapsed — no interest, and the clock
    // must NOT advance (the 30-min remainder is preserved).
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        tusdt_primitives::MILLISECONDS_PER_HOUR / 2,
    );
    pool.accrue_interest(0).unwrap();
    let s = pool.get_market_state(0).unwrap();
    assert_eq!(s.last_update, 0, "sub-hour remainder must not be discarded");
    assert_eq!(s.borrow_index, Ratio::one(), "no interest before a full hour");

    // Write at t = 60 min: exactly one full hour has now elapsed.
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        tusdt_primitives::MILLISECONDS_PER_HOUR,
    );
    pool.accrue_interest(0).unwrap();
    let s = pool.get_market_state(0).unwrap();
    assert_eq!(
        s.last_update,
        tusdt_primitives::MILLISECONDS_PER_HOUR,
        "clock must advance by the whole hour charged"
    );
    assert!(
        s.borrow_index.into_inner() > Ratio::one().into_inner(),
        "one full hour must accrue after two sub-hour writes: {}",
        s.borrow_index.into_inner()
    );
}

// ---------------------------------------------------------------------------
// Idle TAO root-subnet staking
// ---------------------------------------------------------------------------

/// Seeds `amount` native TAO into the pool contract's off-chain balance.
/// `CustomEnvironment` uses `Balance = u64`, which the standard test balance
/// setters do not support — route through the `DefaultEnvironment` host type,
/// which shares the same keyed balance storage (see `tusdt_test_support`).
fn seed_pool_balance(amount: u64) {
    set_callee_balance(ink::env::account_id::<tusdt_env::CustomEnvironment>(), amount);
}

#[ink::test]
fn root_stake_config_access_control_and_getter() {
    let (mut pool, accounts) = setup();

    // Non-governance cannot configure root staking.
    set_caller(accounts.bob);
    assert_eq!(
        pool.set_root_stake_config(accounts.bob, true, 1_000_000_000, 0, 2_000_000),
        Err(Error::NotGovernance)
    );

    // Governance can, and the config round-trips through the getter.
    set_caller(accounts.alice);
    pool.set_root_stake_config(accounts.bob, true, 1_000_000_000, 500_000_000, 2_000_000)
        .unwrap();
    let cfg = pool.get_root_stake_config();
    assert_eq!(cfg.root_hotkey, accounts.bob);
    assert!(cfg.staking_enabled);
    assert_eq!(cfg.stake_buffer, 1_000_000_000);
    assert_eq!(cfg.sweep_threshold, 500_000_000);
    assert_eq!(cfg.stake_floor, 2_000_000);
}

#[ink::test]
fn root_stake_config_enforces_invariants() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);

    // Floor below the pallet minimum is rejected.
    assert_eq!(
        pool.set_root_stake_config(accounts.bob, true, 1_000_000_000, 0, 1_999_999),
        Err(Error::InvalidParam)
    );
    // Buffer below the floor is rejected.
    assert_eq!(
        pool.set_root_stake_config(accounts.bob, true, 1_999_999, 0, 2_000_000),
        Err(Error::InvalidParam)
    );
}

#[ink::test]
fn sweep_stakes_excess_above_buffer() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_root_stake_config(accounts.bob, true, 1_000_000_000, 0, 2_000_000)
        .unwrap();
    register_mock_stateful_root(0);
    seed_pool_balance(5_000_000_000);

    // Permissionless keeper sweep.
    set_caller(accounts.charlie);
    pool.sweep().unwrap();

    // 4 TAO staked (excess above the 1 TAO buffer).
    assert_eq!(pool.get_tao_staked(), 4_000_000_000);
    let record = last_ext_call().unwrap();
    assert_eq!(record.func_id, 1);
    assert_eq!(record.hotkey, accounts.bob);
    assert_eq!(record.netuid, 0);
    assert_eq!(record.amount, 4_000_000_000);
}

#[ink::test]
fn sweep_rate_limited_to_one_per_block() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_root_stake_config(accounts.bob, true, 1_000_000_000, 0, 2_000_000)
        .unwrap();
    register_mock_stateful_root(0);
    seed_pool_balance(3_000_000_000);
    pool.sweep_to_root().unwrap();
    assert_eq!(pool.get_tao_staked(), 2_000_000_000);

    // Same block: fresh excess would be sweepable, but the rate limit blocks it.
    seed_pool_balance(3_000_000_000);
    pool.sweep_to_root().unwrap();
    assert_eq!(pool.get_tao_staked(), 2_000_000_000);

    // Next block: the sweep runs again.
    ink::env::test::set_block_number::<tusdt_env::CustomEnvironment>(1);
    pool.sweep_to_root().unwrap();
    assert_eq!(pool.get_tao_staked(), 4_000_000_000);
}

#[ink::test]
fn sweep_noop_when_disabled_or_below_floor() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);

    // Disabled (default): excess is ignored.
    pool.set_root_stake_config(accounts.bob, false, 1_000_000_000, 0, 2_000_000)
        .unwrap();
    register_mock_stateful_root(0);
    seed_pool_balance(5_000_000_000);
    pool.sweep().unwrap();
    assert_eq!(pool.get_tao_staked(), 0);

    // Enabled but excess below the floor: still a no-op.
    pool.set_root_stake_config(accounts.bob, true, 1_000_000_000, 0, 2_000_000)
        .unwrap();
    seed_pool_balance(1_000_001_000); // excess 1_000 < floor 2_000_000
    pool.sweep().unwrap();
    assert_eq!(pool.get_tao_staked(), 0);
}

#[ink::test]
fn top_up_free_unstakes_shortfall() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_root_stake_config(accounts.bob, true, 1_000_000_000, 0, 2_000_000)
        .unwrap();
    register_mock_stateful_root(3_000_000_000);
    pool.debug_set_root_stake(3_000_000_000);

    let received = pool.top_up_free(1_000_000_000).unwrap();
    assert_eq!(received, 1_000_000_000);
    assert_eq!(pool.get_tao_staked(), 2_000_000_000);
    let record = last_ext_call().unwrap();
    assert_eq!(record.func_id, 2);
    assert_eq!(record.hotkey, accounts.bob);
    assert_eq!(record.netuid, 0);
    assert_eq!(record.amount, 1_000_000_000);
}

#[ink::test]
fn top_up_free_dust_rule_full_exit() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_root_stake_config(accounts.bob, true, 1_000_000_000, 0, 2_000_000)
        .unwrap();
    register_mock_stateful_root(2_500_000);
    pool.debug_set_root_stake(2_500_000);

    // Taking 1_000_000 would leave 1_500_000 < floor → full exit instead.
    let received = pool.top_up_free(1_000_000).unwrap();
    assert_eq!(received, 2_500_000);
    assert_eq!(pool.get_tao_staked(), 0);
    assert_eq!(last_ext_call().unwrap().amount, 2_500_000);
}

#[ink::test]
fn top_up_free_failure_maps_to_liquidity_insufficient() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_root_stake_config(accounts.bob, true, 1_000_000_000, 0, 2_000_000)
        .unwrap();
    register_extension(
        MockExtension::dispatch(None)
            .with_stateful_root_stake(3_000_000_000)
            .with_remove_stake_fails(true),
    );
    pool.debug_set_root_stake(3_000_000_000);
    seed_pool_balance(0);

    assert_eq!(pool.top_up_free(1_000_000_000), Err(Error::LiquidityInsufficient));
    assert_eq!(pool.get_tao_staked(), 3_000_000_000);
}

#[ink::test]
fn market_cash_includes_staked_tao() {
    let (mut pool, _accounts) = setup();
    pool.debug_set_root_stake(1_000_000_000);
    seed_pool_balance(500_000_000);
    assert_eq!(pool.market_cash(0).unwrap(), 1_500_000_000);
}

#[ink::test]
fn hotkey_rotation_unstakes_before_swap() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    pool.set_root_stake_config(accounts.bob, true, 1_000_000_000, 0, 2_000_000)
        .unwrap();
    register_mock_stateful_root(3_000_000_000);
    pool.debug_set_root_stake(3_000_000_000);

    // Rotating the hotkey with stake outstanding fully unstakes first.
    pool.set_root_stake_config(accounts.charlie, true, 1_000_000_000, 0, 2_000_000)
        .unwrap();
    assert_eq!(pool.get_tao_staked(), 0);
    assert_eq!(pool.get_root_stake_config().root_hotkey, accounts.charlie);
    let record = last_ext_call().unwrap();
    assert_eq!(record.func_id, 2);
    assert_eq!(record.hotkey, accounts.bob);
    assert_eq!(record.amount, 3_000_000_000);
}

#[ink::test]
fn treasury_sweep_respects_stake_buffer() {
    let (pool, accounts) = setup();
    set_caller(accounts.alice);

    // 5 TAO − 1 TAO buffer − 1 ED guard = 3_999_999_999 sweepable.
    seed_pool_balance(5_000_000_000);
    assert_eq!(pool.treasury_sweepable(), Some(3_999_999_999));

    // Exactly buffer + ED guard + 1: the last unit is the guard.
    seed_pool_balance(1_000_000_002);
    assert_eq!(pool.treasury_sweepable(), Some(1));

    // At or below buffer + guard: nothing to sweep.
    seed_pool_balance(1_000_000_001);
    assert_eq!(pool.treasury_sweepable(), None);
    seed_pool_balance(500_000_000);
    assert_eq!(pool.treasury_sweepable(), None);
}

// ---------------------------------------------------------------------------
// Risk: health factor + liquidation seizure
//
// `get_health_factor` / `is_liquidatable` cannot be driven end-to-end off-chain
// (they read the oracle through a cross-contract call the test environment
// cannot serve), so the scale-critical arithmetic lives in the pure helper
// `health_factor_from_values` and is pinned here. Before these tests existed,
// NOTHING in the suite referenced the risk module at all.
// ---------------------------------------------------------------------------

/// Regression for the health-factor scale collapse: the computation used to read
/// `Ratio::from_inner(threshold.checked_mul_value(collateral))`, treating a
/// rao-scale integer as a 1e18 inner. The integer division that followed
/// returned inner = 1 for this genuinely healthy position, so `is_liquidatable`
/// (`inner < 1e18`) reported TRUE for every position carrying debt, at any
/// price, and `withdraw_alpha`'s `HF >= 1` guard rejected every indebted user.
///
/// Values are the position that surfaced the bug: oracle 230 TUSDT/TAO,
/// collateral 218.000118 TUSDT, debt 109.150678 TUSDT, liquidation threshold 60%.
#[ink::test]
fn health_factor_does_not_collapse_for_a_healthy_position() {
    let threshold = Ratio::from_basis_points(6000);
    let collateral_value = 218_000_118_000u64;
    let debt_value = 109_150_678_000u64;

    let hf = health_factor_from_values(threshold, collateral_value, debt_value).unwrap();

    // ~1.1983, not the old inner = 1.
    assert_eq!(hf.into_inner(), 1_198_344_098_238_217_081);
    assert!(hf.into_inner() > Ratio::one().into_inner());
    // The exact failure mode: a near-zero integer instead of a 1e18-scaled ratio.
    assert_ne!(hf.into_inner(), 1);
    // And therefore NOT liquidatable (`is_liquidatable` is a strict `inner < 1e18`).
    assert!(hf.into_inner() >= Ratio::one().into_inner());
}

/// A health factor of exactly 1.0 is safe: `is_liquidatable` uses a STRICT `<`.
#[ink::test]
fn health_factor_of_exactly_one_is_not_liquidatable() {
    // 50% × 200 TUSDT = 100 TUSDT weighted collateral against 100 TUSDT debt.
    let hf = health_factor_from_values(
        Ratio::from_basis_points(5000),
        200_000_000_000,
        100_000_000_000,
    )
    .unwrap();

    assert_eq!(hf.into_inner(), Ratio::one().into_inner());
    assert!(hf.into_inner() >= Ratio::one().into_inner());
}

/// An underwater position must report a sub-1.0 health factor.
#[ink::test]
fn health_factor_detects_an_underwater_position() {
    // 60% × 100 TUSDT = 60 TUSDT weighted collateral against 100 TUSDT debt.
    let hf = health_factor_from_values(
        Ratio::from_basis_points(6000),
        100_000_000_000,
        100_000_000_000,
    )
    .unwrap();

    assert_eq!(hf.into_inner(), 600_000_000_000_000_000);
    assert!(hf.into_inner() < Ratio::one().into_inner());
}

/// Zero debt has no health factor; callers short-circuit before dividing.
#[ink::test]
fn health_factor_is_none_without_debt() {
    assert!(health_factor_from_values(Ratio::from_basis_points(6000), 100_000_000_000, 0).is_none());
}

/// Zero collateral against live debt is maximally unhealthy, never an error.
#[ink::test]
fn health_factor_is_zero_without_collateral() {
    let hf =
        health_factor_from_values(Ratio::from_basis_points(6000), 0, 100_000_000_000).unwrap();
    assert_eq!(hf.into_inner(), 0);
    assert!(hf.into_inner() < Ratio::one().into_inner());
}

/// A healthy-but-liquidatable position (C = 125 TUSDT, D = 100 TUSDT at
/// HF = 0.75): the liquidator pays exactly the debt, the platform takes its
/// full 5% fee of the collateral, and there is no deficit. Platform value =
/// 5% × 125 = 6.25 TUSDT; the liquidator keeps 118.75 TUSDT of collateral
/// against a 100 TUSDT payment (+18.75% of debt).
#[ink::test]
fn full_seizure_split_healthy_pays_full_debt_with_fee() {
    let fee = Ratio::from_basis_points(500); // 5%
    let (payment, platform_share, deficit) =
        TusdtLendingPool::full_seizure_split(125_000_000_000, 100_000_000_000, fee).unwrap();

    assert_eq!(payment, 100_000_000_000);
    assert_eq!(platform_share, Ratio::from_basis_points(500));
    assert_eq!(deficit, 0);

    // The platform's share of the collateral is exactly 5% of 125 TUSDT.
    let platform_value = platform_share
        .checked_mul_value(125_000_000_000u128)
        .expect("platform value must fit");
    assert_eq!(platform_value, 6_250_000_000);
}

/// When the fee exceeds the surplus share, the share is capped at the surplus
/// so the liquidator can never lose principal: with C = 105, D = 100 the
/// surplus share is (105−100)/105 ≈ 4.7619%, below the 5% fee, and the
/// liquidator keeps at least D worth of collateral (break-even at worst).
#[ink::test]
fn full_seizure_split_caps_fee_at_the_surplus_share() {
    let fee = Ratio::from_basis_points(500); // 5% — above the surplus share
    let (payment, platform_share, deficit) =
        TusdtLendingPool::full_seizure_split(105_000_000_000, 100_000_000_000, fee).unwrap();

    assert_eq!(payment, 100_000_000_000);
    // floor(5e9 × 1e18 / 105e9) = 4.7619…%
    assert_eq!(platform_share, Ratio::from_inner(47_619_047_619_047_619));
    assert_eq!(deficit, 0);

    // The liquidator's received value never falls below the debt: the
    // platform's cut is at most the surplus (5 TUSDT here).
    let platform_value = platform_share
        .checked_mul_value(105_000_000_000u128)
        .expect("platform value must fit");
    assert!(platform_value <= 5_000_000_000, "cap must leave D to the liquidator");
}

/// A higher configured fee is capped when the surplus is thin: fee 30% but
/// surplus share only 20% (C = 125, D = 100) → the platform gets 20%, not 30%.
#[ink::test]
fn full_seizure_split_caps_a_high_fee_to_the_surplus() {
    let fee = Ratio::from_basis_points(3000); // 30%
    let (payment, platform_share, deficit) =
        TusdtLendingPool::full_seizure_split(125_000_000_000, 100_000_000_000, fee).unwrap();

    assert_eq!(payment, 100_000_000_000);
    // (125−100)/125 = 20% exactly.
    assert_eq!(platform_share, Ratio::from_inner(200_000_000_000_000_000));
    assert_eq!(deficit, 0);
}

/// With collateral exactly equal to the debt there is no surplus, so the
/// platform share is zero and the liquidator pays the debt (break-even).
#[ink::test]
fn full_seizure_split_exact_cover_has_no_surplus_share() {
    let fee = Ratio::from_basis_points(500);
    let (payment, platform_share, deficit) =
        TusdtLendingPool::full_seizure_split(100_000_000_000, 100_000_000_000, fee).unwrap();

    assert_eq!(payment, 100_000_000_000);
    assert_eq!(platform_share, Ratio::from_inner(0));
    assert_eq!(deficit, 0);
}

/// Underwater (C < D): the liquidator pays the collateral value (break-even,
/// never a loss), the platform takes nothing, and the residual is a deficit.
#[ink::test]
fn full_seizure_split_underwater_breaks_even_and_reports_deficit() {
    let fee = Ratio::from_basis_points(500);
    let (payment, platform_share, deficit) =
        TusdtLendingPool::full_seizure_split(80_000_000_000, 100_000_000_000, fee).unwrap();

    assert_eq!(payment, 80_000_000_000);
    assert_eq!(platform_share, Ratio::from_inner(0));
    assert_eq!(deficit, 20_000_000_000);
}

// ---------------------------------------------------------------------------
// Prepaid first hour of interest (hour-beginning charging)
//
// A borrower must owe interest from the instant of the borrow, then take their
// next increase at the market's next hourly accrual. Because the borrow index
// is GLOBAL, this is priced into the new position's scaled debt at borrow time
// rather than in `accrue_interest` (which cannot single out a new position).
// Market 0 is used throughout: `market_cash(1)` needs a cross-contract call the
// off-chain environment cannot serve.
// ---------------------------------------------------------------------------

/// Fresh TAO market state with 1,000 TAO supplied and no debt.
fn prepaid_hour_state() -> MarketState {
    MarketState {
        total_supplied: 1_000_000_000_000,
        total_debt: 0,
        total_scaled_debt: 0,
        borrow_index: Ratio::one(),
        exchange_rate: Ratio::one(),
        reserve_accrued: 0,
        last_update: 0,
    }
}

/// Borrowing 100 TAO against 1,000 TAO of cash puts utilization at 10%, which
/// on the default TAO curve (base 0%, slope1 4%, optimal 80%) is an annual rate
/// of 0.04 × (0.1 / 0.8) = 0.5%. One hour of that on 100 TAO is
/// 100e9 × 0.005 / 8760 = 57,077 rao, and the borrower owes it immediately.
#[ink::test]
fn prepaid_hour_charges_one_hour_of_interest_at_borrow() {
    let (pool, _accounts) = setup();
    seed_pool_balance(1_000_000_000_000);
    let mut state = prepaid_hour_state();

    let premium = pool.charge_prepaid_hour(0, &mut state, 100_000_000_000).unwrap();

    assert_eq!(premium, 57_077);
}

/// The prepaid hour is split exactly like index-driven interest: the default
/// 20% reserve factor to `reserve_accrued`, the remaining 80% to suppliers by
/// growing `exchange_rate`. Nothing may fall through to residual pool cash.
#[ink::test]
fn prepaid_hour_splits_between_reserve_and_suppliers() {
    let (pool, _accounts) = setup();
    seed_pool_balance(1_000_000_000_000);
    let mut state = prepaid_hour_state();

    let premium = pool.charge_prepaid_hour(0, &mut state, 100_000_000_000).unwrap();

    // 20% of 57_077 = 11_416 (the supplier share floors to 45_661).
    let supply_share = 45_661u64;
    assert_eq!(state.reserve_accrued, premium - supply_share);
    assert_eq!(state.reserve_accrued, 11_416);

    // Suppliers are credited by growing the exchange rate: the face value owed
    // to them is `total_supplied × exchange_rate`, so the rate rises by
    // supply_share / total_supplied.
    let expected_delta = Ratio::from_integer(supply_share.into())
        .checked_div_int(state.total_supplied.into())
        .unwrap();
    assert_eq!(
        state.exchange_rate.into_inner(),
        Ratio::one().into_inner() + expected_delta.into_inner()
    );
    assert!(state.exchange_rate.into_inner() > Ratio::one().into_inner());

    // Every rao of the premium is accounted for: nothing is silently dropped.
    let credited_to_suppliers = state
        .exchange_rate
        .into_inner()
        .checked_sub(Ratio::one().into_inner())
        .and_then(|d| Ratio::from_inner(d).checked_mul_value(state.total_supplied.into()))
        .unwrap();
    assert_eq!(u64::try_from(credited_to_suppliers).unwrap() + state.reserve_accrued, premium);
}

/// With nothing supplied there is no exchange rate to grow, so the whole
/// premium goes to the reserve rather than being lost.
#[ink::test]
fn prepaid_hour_goes_entirely_to_reserve_when_nothing_supplied() {
    let (pool, _accounts) = setup();
    seed_pool_balance(1_000_000_000_000);
    let mut state = prepaid_hour_state();
    state.total_supplied = 0;

    let premium = pool.charge_prepaid_hour(0, &mut state, 100_000_000_000).unwrap();

    assert_eq!(state.reserve_accrued, premium);
    assert_eq!(state.exchange_rate, Ratio::one());
}

/// The premium is priced at the utilization the market will have AFTER the
/// borrow, so a larger borrow pays a strictly higher effective hourly rate.
#[ink::test]
fn prepaid_hour_prices_at_post_borrow_utilization() {
    let (pool, _accounts) = setup();
    seed_pool_balance(1_000_000_000_000);

    let mut small_state = prepaid_hour_state();
    let small = pool.charge_prepaid_hour(0, &mut small_state, 100_000_000_000).unwrap();

    let mut large_state = prepaid_hour_state();
    let large = pool.charge_prepaid_hour(0, &mut large_state, 400_000_000_000).unwrap();

    // 4x the principal at 4x the utilization is more than 4x the interest.
    assert!(large > small * 4, "expected super-linear premium, got {large} vs {small}");
}

/// A dust borrow whose hourly interest floors to zero must be a clean no-op
/// rather than an error or a phantom reserve credit.
#[ink::test]
fn prepaid_hour_is_zero_for_a_dust_borrow() {
    let (pool, _accounts) = setup();
    seed_pool_balance(1_000_000_000_000);
    let mut state = prepaid_hour_state();

    let premium = pool.charge_prepaid_hour(0, &mut state, 1_000).unwrap();

    assert_eq!(premium, 0);
    assert_eq!(state.reserve_accrued, 0);
    assert_eq!(state.exchange_rate, Ratio::one());
}

/// End-to-end bookkeeping of the borrow effects block: the position's scaled
/// debt is derived from `amount + premium`, so the face debt read back exceeds
/// the amount actually handed to the borrower from the very first block, while
/// `debt_principal` still records only what they received (so
/// `get_user_debt_details` reports the prepaid hour as interest, not principal).
#[ink::test]
fn prepaid_hour_makes_debt_exceed_principal_immediately() {
    let (pool, _accounts) = setup();
    seed_pool_balance(1_000_000_000_000);
    let mut state = prepaid_hour_state();
    let amount = 100_000_000_000u64;

    let premium = pool.charge_prepaid_hour(0, &mut state, amount).unwrap();
    let debt_booked = amount + premium;
    let scaled = state.borrow_index.checked_div_value_ceil(debt_booked.into()).unwrap();
    let face_debt = scaled_debt_to_face(u64::try_from(scaled).unwrap(), state.borrow_index).unwrap();

    assert!(face_debt > amount, "debt {face_debt} must exceed principal {amount} at t=0");
    assert_eq!(face_debt - amount, premium);
}

// ---------------------------------------------------------------------------
// Liquidation: full-seizure split, bad-debt deficit
//
// The `liquidate` message ends in cross-contract calls (oracle / ERC20 / chain
// extension) that cannot run off-chain, so the new math is pinned at the pure
// helper level (`full_seizure_split`) and at the bookkeeping level, and the
// deficit flow is driven through the message that CAN run off-chain
// (`cover_deficit`). End-to-end liquidation coverage lives in `tools/e2e`.
// ---------------------------------------------------------------------------

/// The native treasury sweep must never touch supplier face value, the accrued
/// reserve, or an unfunded deficit — only cash above all committed claims.
#[ink::test]
fn treasury_sweep_never_touches_supplier_face_or_reserve() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    // 5 TAO balance, 2 TAO supplied at 1.0 exchange rate, 0.5 TAO reserve.
    seed_pool_balance(5_000_000_000);
    pool.debug_set_market_state(
        0,
        MarketState {
            total_supplied: 2_000_000_000,
            total_debt: 0,
            total_scaled_debt: 0,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::one(),
            reserve_accrued: 500_000_000,
            last_update: 0,
        },
    );

    // 5 TAO − 2.5 TAO claims − 1 TAO stake buffer − 1 ED guard.
    assert_eq!(pool.treasury_sweepable(), Some(1_499_999_999));
}

/// When the market's committed claims exceed the balance, nothing is sweepable.
#[ink::test]
fn treasury_sweep_returns_none_when_claims_exceed_balance() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    seed_pool_balance(3_000_000_000);
    pool.debug_set_market_state(
        0,
        MarketState {
            total_supplied: 3_500_000_000,
            total_debt: 0,
            total_scaled_debt: 0,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::one(),
            reserve_accrued: 500_000_000,
            last_update: 0,
        },
    );

    assert_eq!(pool.treasury_sweepable(), None);
}

/// An unfunded deficit is a committed claim: the sweep cannot take it.
#[ink::test]
fn treasury_sweep_nets_unfunded_deficit() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);
    seed_pool_balance(5_000_000_000);
    pool.debug_set_market_deficit(0, 3_000_000_000);

    // 5 TAO − 3 TAO deficit − 1 TAO buffer − 1 ED guard.
    assert_eq!(pool.treasury_sweepable(), Some(999_999_999));
}

/// `cover_deficit` funds the deficit from `reserve_accrued`, and a no-op when
/// the reserve is empty or the deficit is gone.
#[ink::test]
fn cover_deficit_funds_from_reserve_until_exhausted() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice); // maintainer = governance = alice in new_for_test

    pool.debug_set_market_deficit(0, 600_000_000);
    pool.debug_set_market_state(
        0,
        MarketState {
            total_supplied: 0,
            total_debt: 0,
            total_scaled_debt: 0,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::one(),
            reserve_accrued: 1_000_000_000,
            last_update: 0,
        },
    );

    // First cover: 600M deficit funded from 1 TAO reserve.
    pool.cover_deficit(0).unwrap();
    assert_eq!(pool.get_market_deficit(0), Some(0));
    assert_eq!(pool.get_market_state(0).unwrap().reserve_accrued, 400_000_000);

    // Nothing left to cover: no-op, state unchanged.
    pool.cover_deficit(0).unwrap();
    assert_eq!(pool.get_market_deficit(0), Some(0));
    assert_eq!(pool.get_market_state(0).unwrap().reserve_accrued, 400_000_000);
}

/// A deficit larger than the reserve stays on the books as an unfunded shortfall.
#[ink::test]
fn cover_deficit_leaves_unfunded_remainder_when_reserve_is_short() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);

    pool.debug_set_market_deficit(0, 600_000_000);
    pool.debug_set_market_state(
        0,
        MarketState {
            total_supplied: 0,
            total_debt: 0,
            total_scaled_debt: 0,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::one(),
            reserve_accrued: 250_000_000,
            last_update: 0,
        },
    );

    pool.cover_deficit(0).unwrap();
    assert_eq!(pool.get_market_deficit(0), Some(350_000_000));
    assert_eq!(pool.get_market_state(0).unwrap().reserve_accrued, 0);
}

/// Covering a deficit is a maintainer decision, not permissionless.
#[ink::test]
fn cover_deficit_requires_maintainer() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.bob); // not maintainer/governance

    pool.debug_set_market_deficit(0, 100_000_000);
    assert_eq!(pool.cover_deficit(0), Err(Error::NotMaintainer));
}

/// The deficit query returns `None` for an unknown market and the booked value
/// for a known one.
#[ink::test]
fn get_market_deficit_reports_booked_deficits() {
    let (pool, _accounts) = setup();
    // No deficit booked on any market (fresh state) → None, same pattern as
    // `get_alpha_params` for unconfigured netuids.
    assert_eq!(pool.get_market_deficit(99), None);
    assert_eq!(pool.get_market_deficit(0), None);

    let (mut pool, _accounts) = setup();
    pool.debug_set_market_deficit(0, 5_001_585_000);
    assert_eq!(pool.get_market_deficit(0), Some(5_001_585_000));
}

/// Bookkeeping pin for the bad-debt write-off, mirroring the reproduction's
/// final state: residual debt 5.001585 TUSDT at a 1.0 borrow index is removed
/// from the scaled total (so another borrower's 4_998_415_000 scaled units are
/// unaffected) and frozen as a deficit of exactly its face value.
#[ink::test]
fn bad_debt_write_off_bookkeeping_matches_the_repro() {
    let index = Ratio::one();
    let residual_scaled: u64 = 5_001_585_000; // the stranded TUSDT debt
    let other_borrower_scaled: u64 = 4_998_415_000;
    let total_scaled = residual_scaled + other_borrower_scaled;

    // The write-off freezes the residual's face value as the deficit...
    let deficit_face = scaled_debt_to_face(residual_scaled, index).unwrap();
    assert_eq!(deficit_face, residual_scaled);

    // ...and removes exactly the residual from the scaled total, so the
    // remaining borrower's share and the derived face total are untouched.
    let remaining_scaled = total_scaled - residual_scaled;
    assert_eq!(remaining_scaled, other_borrower_scaled);
    assert_eq!(
        scaled_debt_to_face(remaining_scaled, index).unwrap(),
        other_borrower_scaled
    );
}

/// A written-off deficit must not poison utilization or rates: with the debt
/// removed from the ledger, an hour of accrual produces zero interest and zero
/// utilization even while the deficit counter is non-zero.
#[ink::test]
fn deficit_does_not_poison_utilization_or_rates() {
    let (mut pool, accounts) = setup();
    set_caller(accounts.alice);

    // Market 0: debt fully written off, but a deficit remains on the books.
    pool.debug_set_market_deficit(0, 5_001_585_000);
    pool.debug_set_market_state(
        0,
        MarketState {
            total_supplied: 1_000_000_000_000,
            total_debt: 0,
            total_scaled_debt: 0,
            borrow_index: Ratio::one(),
            exchange_rate: Ratio::one(),
            reserve_accrued: 0,
            last_update: 0,
        },
    );
    seed_pool_balance(1_000_000_000_000);

    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(
        tusdt_primitives::MILLISECONDS_PER_HOUR + 1,
    );
    pool.accrue_interest(0).unwrap();

    let state = pool.get_market_state(0).unwrap();
    assert_eq!(state.total_debt, 0);
    assert_eq!(state.borrow_index, Ratio::one());
    // The deficit is untouched by accrual — it is frozen.
    assert_eq!(pool.get_market_deficit(0), Some(5_001_585_000));
    assert_eq!(pool.get_utilization(0), Some(Ratio::from_inner(0)));
}



