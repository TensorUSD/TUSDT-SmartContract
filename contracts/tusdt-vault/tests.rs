use super::vault::*;
use tusdt_oracle::PriceData;
use tusdt_primitives::{Ratio, MILLISECONDS_PER_HOUR};

fn set_caller(caller: ink::primitives::AccountId) {
    let callee = ink::env::account_id::<tusdt_env::CustomEnvironment>();
    ink::env::test::set_callee::<tusdt_env::CustomEnvironment>(callee);
    ink::env::test::set_caller::<tusdt_env::CustomEnvironment>(caller);
}

fn set_time(timestamp: u64) {
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(timestamp);
}

fn set_transferred_value(value: u64) {
    ink::env::test::set_value_transferred::<ink::env::DefaultEnvironment>(u128::from(value));
}

fn transfer_in(value: u64) {
    ink::env::test::transfer_in::<ink::env::DefaultEnvironment>(u128::from(value));
}

fn create_vault_with_collateral(
    contract: &mut TusdtVault,
    owner: ink::primitives::AccountId,
    collateral: u64,
) -> u32 {
    set_caller(owner);
    transfer_in(collateral);
    contract
        .create_vault()
        .expect("create_vault should succeed in tests")
}

#[ink::test]
fn create_vault_tracks_ids_balances_and_counts() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);

    set_time(10);
    let alice_vault_0 = create_vault_with_collateral(&mut vault, accounts.alice, 500);
    set_time(20);
    let alice_vault_1 = create_vault_with_collateral(&mut vault, accounts.alice, 700);
    set_time(30);
    let bob_vault_0 = create_vault_with_collateral(&mut vault, accounts.bob, 300);

    assert_eq!(alice_vault_0, 0);
    assert_eq!(alice_vault_1, 1);
    assert_eq!(bob_vault_0, 0);
    assert_eq!(vault.get_vaults_count(accounts.alice), 2);
    assert_eq!(vault.get_vaults_count(accounts.bob), 1);
    assert_eq!(vault.get_total_vaults_count(), 3);
    assert_eq!(vault.get_total_collateral_balance(), 1_500);
    assert_eq!(vault.get_total_debt(accounts.alice), 0);
    assert_eq!(vault.get_total_debt(accounts.bob), 0);

    let alice_v0 = vault
        .get_vault(accounts.alice, 0)
        .expect("alice vault 0 should exist");
    assert_eq!(alice_v0.owner, accounts.alice);
    assert_eq!(alice_v0.collateral_balance, 500);
    assert_eq!(alice_v0.borrowed_token_balance, 0);
    assert_eq!(alice_v0.total_interest_accrued, 0);
    assert_eq!(alice_v0.created_at, 10);
    assert_eq!(alice_v0.last_interest_accrued_at, 10);

    let bob_v0 = vault
        .get_vault(accounts.bob, 0)
        .expect("bob vault 0 should exist");
    assert_eq!(bob_v0.collateral_balance, 300);
    assert_eq!(bob_v0.created_at, 30);
}

#[ink::test]
fn add_collateral_updates_vault_and_total_collateral() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);

    create_vault_with_collateral(&mut vault, accounts.alice, 400);

    set_caller(accounts.alice);
    transfer_in(250);
    assert_eq!(vault.add_collateral(0), Ok(()));

    let updated = vault
        .get_vault(accounts.alice, 0)
        .expect("vault should exist after add_collateral");
    assert_eq!(updated.collateral_balance, 650);
    assert_eq!(vault.get_total_collateral_balance(), 650);
}

#[ink::test]
fn add_collateral_fails_for_missing_or_liquidating_vault() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);

    set_caller(accounts.bob);
    set_transferred_value(10);
    assert_eq!(vault.add_collateral(0), Err(Error::VaultNotFound));

    create_vault_with_collateral(&mut vault, accounts.alice, 200);
    vault.set_liquidation_auction_for_test(accounts.alice, 0, 42);

    set_caller(accounts.alice);
    set_transferred_value(10);
    assert_eq!(vault.add_collateral(0), Err(Error::VaultInLiquidation));
}

#[ink::test]
fn release_collateral_checks_balance_before_oracle_path() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);

    create_vault_with_collateral(&mut vault, accounts.alice, 150);

    set_caller(accounts.alice);
    assert_eq!(
        vault.release_collateral(0, 151),
        Err(Error::InsufficientCollateral)
    );
}

#[ink::test]
fn set_contract_params_enforces_governance_and_validation() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);

    let valid = VaultContractParamsConfig {
        collateral_ratio: 200,
        liquidation_ratio: 130,
        interest_rate: 7,
        liquidation_fee: 2,
        borrow_cap: 1_000_000,
        auction_duration_ms: 120_000,
        max_oracle_age_ms: 600_000,
    };

    set_caller(accounts.bob);
    assert_eq!(vault.set_contract_params(valid), Err(Error::NotGovernance));

    set_caller(accounts.alice);
    assert_eq!(
        vault.set_contract_params(VaultContractParamsConfig {
            collateral_ratio: 99,
            liquidation_ratio: 130,
            interest_rate: 7,
            liquidation_fee: 2,
            borrow_cap: 1_000_000,
            auction_duration_ms: 120_000,
            max_oracle_age_ms: 600_000,
        }),
        Err(Error::InvalidRatio)
    );
    assert_eq!(
        vault.set_contract_params(VaultContractParamsConfig {
            collateral_ratio: 200,
            liquidation_ratio: 130,
            interest_rate: 7,
            liquidation_fee: 2,
            borrow_cap: 1_000_000,
            auction_duration_ms: 59_999,
            max_oracle_age_ms: 600_000,
        }),
        Err(Error::InvalidAuctionDuration)
    );
    assert_eq!(
        vault.set_contract_params(VaultContractParamsConfig {
            collateral_ratio: 200,
            liquidation_ratio: 130,
            interest_rate: 7,
            liquidation_fee: 2,
            borrow_cap: 1_000_000,
            auction_duration_ms: 120_000,
            max_oracle_age_ms: 0,
        }),
        Err(Error::InvalidOracleMaxAge)
    );

    assert_eq!(vault.set_contract_params(valid), Ok(()));
    let params = vault.get_contract_params();
    assert_eq!(params.collateral_ratio, 200);
    assert_eq!(params.liquidation_ratio, 130);
    assert_eq!(params.interest_rate, 7);
    assert_eq!(params.liquidation_fee, 2);
    assert_eq!(params.borrow_cap, 1_000_000);
    assert_eq!(params.auction_duration_ms, 120_000);
    assert_eq!(params.max_oracle_age_ms, 600_000);
}

#[ink::test]
fn governance_can_be_updated_by_current_governance() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);
    let valid = VaultContractParamsConfig {
        collateral_ratio: 200,
        liquidation_ratio: 130,
        interest_rate: 7,
        liquidation_fee: 2,
        borrow_cap: 1_000_000,
        auction_duration_ms: 120_000,
        max_oracle_age_ms: 600_000,
    };

    set_caller(accounts.bob);
    assert_eq!(
        vault.update_governance(accounts.bob),
        Err(Error::NotGovernance)
    );

    set_caller(accounts.alice);
    assert_eq!(vault.governance(), accounts.alice);
    assert_eq!(vault.update_governance(accounts.bob), Ok(()));
    assert_eq!(vault.governance(), accounts.bob);

    assert_eq!(vault.set_contract_params(valid), Err(Error::NotGovernance));

    set_caller(accounts.bob);
    assert_eq!(vault.set_contract_params(valid), Ok(()));
}

#[ink::test]
fn price_validation_and_collateral_math_helpers_work() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let vault = TusdtVault::new_for_test(accounts.alice);
    let price = Ratio::from_integer(3);

    assert_eq!(TusdtVault::collateral_value(price, 100), Ok(300));
    assert_eq!(vault.max_borrow_allowed(price, 100), Ok(200));
    assert_eq!(vault.liquidation_limit(price, 100), Ok(250));
    assert_eq!(TusdtVault::collateral_needed_for_debt(price, 300), Ok(100));
}

#[ink::test]
fn oracle_price_validation_rejects_missing_and_stale_data() {
    let price = Ratio::from_integer(3);

    assert_eq!(
        TusdtVault::validate_price_data(None, 10, 100),
        Err(Error::OraclePriceUnavailable)
    );
    assert_eq!(
        TusdtVault::validate_price_data(
            Some(PriceData {
                round_id: 0,
                price,
                median_price: price,
                reporter_count: 3,
                committed_at: 0,
                was_overridden: false,
            }),
            3_600_001,
            3_600_000,
        ),
        Err(Error::OraclePriceStale)
    );
    assert_eq!(
        TusdtVault::validate_price_data(
            Some(PriceData {
                round_id: 1,
                price,
                median_price: price,
                reporter_count: 3,
                committed_at: 10,
                was_overridden: false,
            }),
            20,
            30,
        ),
        Ok(PriceData {
            round_id: 1,
            price,
            median_price: price,
            reporter_count: 3,
            committed_at: 10,
            was_overridden: false,
        })
    );
}

#[ink::test]
fn pagination_for_owner_and_global_vaults_works() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);

    for _ in 0..12 {
        create_vault_with_collateral(&mut vault, accounts.alice, 1);
    }

    let owner_page_0 = vault
        .get_vaults(accounts.alice, 0)
        .expect("owner page 0 should exist");
    assert_eq!(owner_page_0.len(), 10);
    assert_eq!(owner_page_0[0].id, 0);
    assert_eq!(owner_page_0[9].id, 9);

    let owner_page_1 = vault
        .get_vaults(accounts.alice, 1)
        .expect("owner page 1 should exist");
    assert_eq!(owner_page_1.len(), 2);
    assert_eq!(owner_page_1[0].id, 10);
    assert_eq!(owner_page_1[1].id, 11);

    assert!(matches!(
        vault.get_vaults(accounts.alice, 2),
        Ok(vaults) if vaults.is_empty()
    ));

    let all_page_0 = vault.get_all_vaults(0).expect("global page 0 should exist");
    assert_eq!(all_page_0.len(), 10);
    let all_page_1 = vault.get_all_vaults(1).expect("global page 1 should exist");
    assert_eq!(all_page_1.len(), 2);

    assert!(matches!(
        vault.get_all_vaults(2),
        Ok(vaults) if vaults.is_empty()
    ));
}

#[ink::test]
fn interest_accrues_after_full_hours_only() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let vault_contract = TusdtVault::new_for_test(accounts.alice);
    let mut vault = Vault {
        id: 0,
        owner: accounts.alice,
        collateral_balance: 1_000,
        borrowed_token_balance: 1_000_000,
        total_interest_accrued: 0,
        created_at: 0,
        last_interest_accrued_at: 0,
    };

    set_time(MILLISECONDS_PER_HOUR / 2);
    assert_eq!(vault_contract.accrue_interest_for_vault(&mut vault), Ok(()));
    assert_eq!(vault.borrowed_token_balance, 1_000_000);
    assert_eq!(vault.total_interest_accrued, 0);
    assert_eq!(vault.last_interest_accrued_at, 0);

    set_time(MILLISECONDS_PER_HOUR);
    assert_eq!(vault_contract.accrue_interest_for_vault(&mut vault), Ok(()));
    assert!(vault.borrowed_token_balance > 1_000_000);
    assert_eq!(
        vault.total_interest_accrued,
        vault.borrowed_token_balance - 1_000_000
    );
    assert_eq!(vault.last_interest_accrued_at, MILLISECONDS_PER_HOUR);
}

#[ink::test]
fn interest_uses_hourly_discrete_compounding() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault_contract = TusdtVault::new_for_test(accounts.alice);
    let mut vault = Vault {
        id: 0,
        owner: accounts.alice,
        collateral_balance: 1_000,
        borrowed_token_balance: 100_000,
        total_interest_accrued: 0,
        created_at: 0,
        last_interest_accrued_at: 0,
    };

    set_caller(accounts.alice);
    assert_eq!(
        vault_contract.set_contract_params(VaultContractParamsConfig {
            collateral_ratio: 200,
            liquidation_ratio: 130,
            interest_rate: 10,
            liquidation_fee: 2,
            borrow_cap: 1_000_000,
            auction_duration_ms: 120_000,
            max_oracle_age_ms: 600_000,
        }),
        Ok(())
    );

    set_time(30 * 24 * MILLISECONDS_PER_HOUR);
    assert_eq!(vault_contract.accrue_interest_for_vault(&mut vault), Ok(()));
    assert_eq!(vault.borrowed_token_balance, 100_825);
    assert_eq!(vault.total_interest_accrued, 825);
    assert_eq!(
        vault.last_interest_accrued_at,
        30 * 24 * MILLISECONDS_PER_HOUR
    );
}

#[ink::test]
fn accrue_interest_message_updates_stored_vault_balance() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault_contract = TusdtVault::new_for_test(accounts.alice);

    set_time(0);
    let vault_id = create_vault_with_collateral(&mut vault_contract, accounts.alice, 1_000);

    let mut stored_vault = vault_contract
        .get_vault(accounts.alice, vault_id)
        .expect("vault should exist");
    stored_vault.borrowed_token_balance = 100_000;
    stored_vault.total_interest_accrued = 0;
    stored_vault.last_interest_accrued_at = 0;
    assert_eq!(
        vault_contract.save_vault(accounts.alice, vault_id, &stored_vault),
        Ok(())
    );

    set_time(30 * 24 * MILLISECONDS_PER_HOUR);
    assert_eq!(
        vault_contract.accrue_interest(accounts.alice, vault_id),
        Ok(100_411)
    );

    let updated_vault = vault_contract
        .get_vault(accounts.alice, vault_id)
        .expect("vault should still exist");
    assert_eq!(updated_vault.borrowed_token_balance, 100_411);
    assert_eq!(updated_vault.total_interest_accrued, 411);
    assert_eq!(vault_contract.get_total_debt(accounts.alice), 100_411);
    assert_eq!(
        updated_vault.last_interest_accrued_at,
        30 * 24 * MILLISECONDS_PER_HOUR
    );
}

#[ink::test]
fn accrue_interest_message_rejects_missing_vault() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault_contract = TusdtVault::new_for_test(accounts.alice);

    assert_eq!(
        vault_contract.accrue_interest(accounts.alice, 0),
        Err(Error::VaultNotFound)
    );
}

#[ink::test]
fn total_debt_tracks_sum_of_owner_vault_debts() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let mut vault_contract = TusdtVault::new_for_test(accounts.alice);

    let alice_vault_0 = create_vault_with_collateral(&mut vault_contract, accounts.alice, 400);
    let alice_vault_1 = create_vault_with_collateral(&mut vault_contract, accounts.alice, 500);
    let bob_vault_0 = create_vault_with_collateral(&mut vault_contract, accounts.bob, 300);

    let mut alice_first = vault_contract
        .get_vault(accounts.alice, alice_vault_0)
        .expect("alice vault 0 should exist");
    alice_first.borrowed_token_balance = 125;
    assert_eq!(
        vault_contract.save_vault(accounts.alice, alice_vault_0, &alice_first),
        Ok(())
    );

    let mut alice_second = vault_contract
        .get_vault(accounts.alice, alice_vault_1)
        .expect("alice vault 1 should exist");
    alice_second.borrowed_token_balance = 275;
    assert_eq!(
        vault_contract.save_vault(accounts.alice, alice_vault_1, &alice_second),
        Ok(())
    );

    let mut bob_first = vault_contract
        .get_vault(accounts.bob, bob_vault_0)
        .expect("bob vault 0 should exist");
    bob_first.borrowed_token_balance = 80;
    assert_eq!(
        vault_contract.save_vault(accounts.bob, bob_vault_0, &bob_first),
        Ok(())
    );

    assert_eq!(vault_contract.get_total_debt(accounts.alice), 400);
    assert_eq!(vault_contract.get_total_debt(accounts.bob), 80);

    alice_first.borrowed_token_balance = 160;
    assert_eq!(
        vault_contract.save_vault(accounts.alice, alice_vault_0, &alice_first),
        Ok(())
    );

    assert_eq!(vault_contract.get_total_debt(accounts.alice), 435);
    assert_eq!(vault_contract.get_total_debt(accounts.bob), 80);
}

#[ink::test]
fn liquidatable_check_uses_liquidation_ratio_limit() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    let vault_contract = TusdtVault::new_for_test(accounts.alice);
    let price = Ratio::from_integer(1);

    let safe_vault = Vault {
        id: 0,
        owner: accounts.alice,
        collateral_balance: 120,
        borrowed_token_balance: 100,
        total_interest_accrued: 0,
        created_at: 0,
        last_interest_accrued_at: 0,
    };
    let unsafe_vault = Vault {
        borrowed_token_balance: 101,
        ..safe_vault.clone()
    };

    assert_eq!(
        vault_contract.is_liquidatable(price, &safe_vault),
        Ok(false)
    );
    assert_eq!(
        vault_contract.is_liquidatable(price, &unsafe_vault),
        Ok(true)
    );
}
