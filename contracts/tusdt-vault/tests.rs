use super::vault::*;
use tusdt_primitives::MILLISECONDS_PER_DAY;

fn set_caller(caller: ink::primitives::AccountId) {
    let callee = ink::env::account_id::<ink::env::DefaultEnvironment>();
    ink::env::test::set_callee::<ink::env::DefaultEnvironment>(callee);
    ink::env::test::set_caller::<ink::env::DefaultEnvironment>(caller);
}

fn set_time(timestamp: u64) {
    ink::env::test::set_block_timestamp::<ink::env::DefaultEnvironment>(timestamp);
}

fn set_transferred_value(value: u128) {
    ink::env::test::set_value_transferred::<ink::env::DefaultEnvironment>(value);
}

fn transfer_in(value: u128) {
    ink::env::test::transfer_in::<ink::env::DefaultEnvironment>(value);
}

fn create_vault_with_collateral(
    contract: &mut TusdtVault,
    owner: ink::primitives::AccountId,
    collateral: u128,
) -> u32 {
    set_caller(owner);
    transfer_in(collateral);
    contract
        .create_vault()
        .expect("create_vault should succeed in tests")
}

#[ink::test]
fn create_vault_tracks_ids_balances_and_counts() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
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

    let alice_v0 = vault
        .get_vault(accounts.alice, 0)
        .expect("alice vault 0 should exist");
    assert_eq!(alice_v0.owner, accounts.alice);
    assert_eq!(alice_v0.collateral_balance, 500);
    assert_eq!(alice_v0.borrowed_token_balance, 0);
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
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
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
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
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
fn release_collateral_works_and_updates_total_collateral() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);

    create_vault_with_collateral(&mut vault, accounts.alice, 600);

    set_caller(accounts.alice);
    assert_eq!(vault.release_collateral(0, 250), Ok(()));

    let updated = vault
        .get_vault(accounts.alice, 0)
        .expect("vault should exist after release");
    assert_eq!(updated.collateral_balance, 350);
    assert_eq!(vault.get_total_collateral_balance(), 350);
}

#[ink::test]
fn release_collateral_checks_balance_and_ratio() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);

    create_vault_with_collateral(&mut vault, accounts.alice, 150);

    set_caller(accounts.alice);
    assert_eq!(
        vault.release_collateral(0, 151),
        Err(Error::InsufficientCollateral)
    );

    vault
        .set_vault_borrowed_balance_for_test(accounts.alice, 0, 100)
        .expect("test setup should set borrowed balance");
    assert_eq!(
        vault.release_collateral(0, 1),
        Err(Error::CollateralRatioExceeded)
    );
}

#[ink::test]
fn set_contract_params_enforces_owner_and_validation() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);

    let valid = VaultContractParamsPercentage {
        collateral_ratio: 200,
        liquidation_ratio: 130,
        interest_rate: 7,
        liquidation_fee: 2,
        auction_duration_ms: 120_000,
    };

    set_caller(accounts.bob);
    assert_eq!(
        vault.set_contract_params(valid),
        Err(Error::NotContractOwner)
    );

    set_caller(accounts.alice);
    assert_eq!(
        vault.set_contract_params(VaultContractParamsPercentage {
            collateral_ratio: 99,
            liquidation_ratio: 130,
            interest_rate: 7,
            liquidation_fee: 2,
            auction_duration_ms: 120_000,
        }),
        Err(Error::InvalidRatio)
    );
    assert_eq!(
        vault.set_contract_params(VaultContractParamsPercentage {
            collateral_ratio: 200,
            liquidation_ratio: 130,
            interest_rate: 7,
            liquidation_fee: 2,
            auction_duration_ms: 59_999,
        }),
        Err(Error::InvalidAuctionDuration)
    );

    assert_eq!(vault.set_contract_params(valid), Ok(()));
    assert_eq!(vault.get_contract_params().collateral_ratio, 200);
    assert_eq!(vault.get_contract_params().liquidation_ratio, 130);
    assert_eq!(vault.get_contract_params().interest_rate, 7);
    assert_eq!(vault.get_contract_params().liquidation_fee, 2);
    assert_eq!(vault.get_contract_params().auction_duration_ms, 120_000);
}

#[ink::test]
fn price_update_and_collateral_value_queries_work() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut vault = TusdtVault::new_for_test(accounts.alice);

    set_caller(accounts.bob);
    assert_eq!(
        vault.set_collateral_token_price_for_testing(2),
        Err(Error::NotContractOwner)
    );

    set_caller(accounts.alice);
    assert_eq!(
        vault.set_collateral_token_price_for_testing(0),
        Err(Error::InvalidRatio)
    );
    assert_eq!(vault.set_collateral_token_price_for_testing(3), Ok(()));
    assert_eq!(vault.get_collateral_token_price_for_testing(), 3);

    create_vault_with_collateral(&mut vault, accounts.alice, 100);
    assert_eq!(vault.get_vault_collateral_value(accounts.alice, 0), Ok(300));
    assert_eq!(vault.get_max_borrow(accounts.alice, 0), Ok(200));
}

#[ink::test]
fn pagination_for_owner_and_global_vaults_works() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
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
        Err(Error::OutOfBoundPage)
    ));

    let all_page_0 = vault.get_all_vaults(0).expect("global page 0 should exist");
    assert_eq!(all_page_0.len(), 10);
    let all_page_1 = vault.get_all_vaults(1).expect("global page 1 should exist");
    assert_eq!(all_page_1.len(), 2);

    assert!(matches!(
        vault.get_all_vaults(2),
        Err(Error::OutOfBoundPage)
    ));
}

#[ink::test]
fn interest_accrues_after_full_days_only() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let vault_contract = TusdtVault::new_for_test(accounts.alice);
    let mut vault = Vault {
        id: 0,
        owner: accounts.alice,
        collateral_balance: 1_000,
        borrowed_token_balance: 1_000_000,
        created_at: 0,
        last_interest_accrued_at: 0,
    };

    set_time(MILLISECONDS_PER_DAY / 2);
    assert_eq!(vault_contract.accrue_interest(&mut vault), Ok(()));
    assert_eq!(vault.borrowed_token_balance, 1_000_000);
    assert_eq!(vault.last_interest_accrued_at, 0);

    set_time(MILLISECONDS_PER_DAY);
    assert_eq!(vault_contract.accrue_interest(&mut vault), Ok(()));
    assert!(vault.borrowed_token_balance > 1_000_000);
    assert_eq!(vault.last_interest_accrued_at, MILLISECONDS_PER_DAY);
}

#[ink::test]
fn liquidatable_check_uses_liquidation_ratio_limit() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let vault_contract = TusdtVault::new_for_test(accounts.alice);

    let safe_vault = Vault {
        id: 0,
        owner: accounts.alice,
        collateral_balance: 120,
        borrowed_token_balance: 100,
        created_at: 0,
        last_interest_accrued_at: 0,
    };
    let unsafe_vault = Vault {
        borrowed_token_balance: 101,
        ..safe_vault.clone()
    };

    assert_eq!(vault_contract.is_liquidatable(&safe_vault), Ok(false));
    assert_eq!(vault_contract.is_liquidatable(&unsafe_vault), Ok(true));
}
