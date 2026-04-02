use super::oracle::*;
use tusdt_primitives::Ratio;

fn set_caller(caller: ink::primitives::AccountId) {
    let callee = ink::env::account_id::<tusdt_env::CustomEnvironment>();
    ink::env::test::set_callee::<tusdt_env::CustomEnvironment>(callee);
    ink::env::test::set_caller::<tusdt_env::CustomEnvironment>(caller);
}

fn set_time(timestamp: u64) {
    ink::env::test::set_block_timestamp::<tusdt_env::CustomEnvironment>(timestamp);
}

fn submit_price(
    oracle: &mut TusdtOracle,
    reporter: ink::primitives::AccountId,
    price: u128,
) {
    set_caller(reporter);
    assert_eq!(oracle.submit_price(Ratio::from_integer(price)), Ok(()));
}

#[ink::test]
fn reporter_whitelist_is_enforced() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);

    set_caller(accounts.bob);
    assert_eq!(
        oracle.submit_price(Ratio::from_integer(10)),
        Err(Error::NotReporter)
    );
}

#[ink::test]
fn zero_price_is_rejected() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    assert_eq!(oracle.set_reporter(accounts.bob, true), Ok(()));

    set_caller(accounts.bob);
    assert_eq!(
        oracle.submit_price(Ratio::from_integer(0)),
        Err(Error::InvalidPrice)
    );
}

#[ink::test]
fn reporter_resubmission_replaces_previous_value() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    assert_eq!(oracle.set_reporter(accounts.bob, true), Ok(()));
    assert_eq!(oracle.set_reporter(accounts.charlie, true), Ok(()));
    assert_eq!(oracle.set_reporter(accounts.django, true), Ok(()));

    submit_price(&mut oracle, accounts.bob, 10);
    submit_price(&mut oracle, accounts.bob, 12);
    submit_price(&mut oracle, accounts.charlie, 13);
    submit_price(&mut oracle, accounts.django, 14);

    assert_eq!(
        oracle.get_current_round_summary(),
        RoundSummary {
            round_id: 0,
            reporter_count: 3,
            median_price: Some(Ratio::from_integer(13)),
        }
    );
}

#[ink::test]
fn commit_is_blocked_below_quorum() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    assert_eq!(oracle.set_reporter(accounts.bob, true), Ok(()));
    assert_eq!(oracle.set_reporter(accounts.charlie, true), Ok(()));
    assert_eq!(oracle.set_validator(Some(accounts.django)), Ok(()));

    submit_price(&mut oracle, accounts.bob, 10);
    submit_price(&mut oracle, accounts.charlie, 20);

    set_caller(accounts.django);
    assert_eq!(oracle.commit_round(None), Err(Error::NotEnoughSubmissions));
}

#[ink::test]
fn override_allows_commit_without_submissions() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    assert_eq!(oracle.set_validator(Some(accounts.bob)), Ok(()));

    set_time(55);
    set_caller(accounts.bob);
    let committed = oracle
        .commit_round(Some(Ratio::from_integer(42)))
        .expect("override commit should succeed");

    assert_eq!(
        committed,
        PriceData {
            round_id: 0,
            price: Ratio::from_integer(42),
            median_price: Ratio::from_integer(42),
            reporter_count: 0,
            committed_at: 55,
            was_overridden: true,
        }
    );
    assert_eq!(oracle.get_latest_price(), Some(committed));
    assert_eq!(oracle.current_round_id(), 1);
}

#[ink::test]
fn override_bypasses_quorum_and_keeps_available_median() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    assert_eq!(oracle.set_reporter(accounts.bob, true), Ok(()));
    assert_eq!(oracle.set_validator(Some(accounts.charlie)), Ok(()));

    submit_price(&mut oracle, accounts.bob, 10);

    set_time(88);
    set_caller(accounts.charlie);
    let committed = oracle
        .commit_round(Some(Ratio::from_integer(25)))
        .expect("override commit should succeed");

    assert_eq!(
        committed,
        PriceData {
            round_id: 0,
            price: Ratio::from_integer(25),
            median_price: Ratio::from_integer(10),
            reporter_count: 1,
            committed_at: 88,
            was_overridden: true,
        }
    );
}

#[ink::test]
fn median_is_used_for_three_submissions() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    assert_eq!(oracle.set_reporter(accounts.bob, true), Ok(()));
    assert_eq!(oracle.set_reporter(accounts.charlie, true), Ok(()));
    assert_eq!(oracle.set_reporter(accounts.django, true), Ok(()));
    assert_eq!(oracle.set_validator(Some(accounts.eve)), Ok(()));

    submit_price(&mut oracle, accounts.bob, 30);
    submit_price(&mut oracle, accounts.charlie, 10);
    submit_price(&mut oracle, accounts.django, 20);

    set_time(77);
    set_caller(accounts.eve);
    let committed = oracle.commit_round(None).expect("commit should succeed");
    assert_eq!(
        committed,
        PriceData {
            round_id: 0,
            price: Ratio::from_integer(20),
            median_price: Ratio::from_integer(20),
            reporter_count: 3,
            committed_at: 77,
            was_overridden: false,
        }
    );
}

#[ink::test]
fn median_is_used_for_five_submissions() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    for reporter in [
        accounts.bob,
        accounts.charlie,
        accounts.django,
        accounts.eve,
        accounts.frank,
    ] {
        assert_eq!(oracle.set_reporter(reporter, true), Ok(()));
    }

    submit_price(&mut oracle, accounts.bob, 50);
    submit_price(&mut oracle, accounts.charlie, 10);
    submit_price(&mut oracle, accounts.django, 30);
    submit_price(&mut oracle, accounts.eve, 20);
    submit_price(&mut oracle, accounts.frank, 40);

    assert_eq!(
        oracle.get_current_round_summary(),
        RoundSummary {
            round_id: 0,
            reporter_count: 5,
            median_price: Some(Ratio::from_integer(30)),
        }
    );
}

#[ink::test]
fn manual_override_is_stored_while_preserving_median_metadata() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    assert_eq!(oracle.set_reporter(accounts.bob, true), Ok(()));
    assert_eq!(oracle.set_reporter(accounts.charlie, true), Ok(()));
    assert_eq!(oracle.set_reporter(accounts.django, true), Ok(()));
    assert_eq!(oracle.set_validator(Some(accounts.eve)), Ok(()));

    submit_price(&mut oracle, accounts.bob, 10);
    submit_price(&mut oracle, accounts.charlie, 20);
    submit_price(&mut oracle, accounts.django, 30);

    set_time(99);
    set_caller(accounts.eve);
    let committed = oracle
        .commit_round(Some(Ratio::from_integer(99)))
        .expect("override commit should succeed");

    assert_eq!(
        committed,
        PriceData {
            round_id: 0,
            price: Ratio::from_integer(99),
            median_price: Ratio::from_integer(20),
            reporter_count: 3,
            committed_at: 99,
            was_overridden: true,
        }
    );
    assert_eq!(oracle.get_latest_price(), Some(committed));
}

#[ink::test]
fn commit_advances_the_round() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    assert_eq!(oracle.set_reporter(accounts.bob, true), Ok(()));
    assert_eq!(oracle.set_reporter(accounts.charlie, true), Ok(()));
    assert_eq!(oracle.set_reporter(accounts.django, true), Ok(()));
    assert_eq!(oracle.set_validator(Some(accounts.eve)), Ok(()));

    submit_price(&mut oracle, accounts.bob, 10);
    submit_price(&mut oracle, accounts.charlie, 20);
    submit_price(&mut oracle, accounts.django, 30);

    set_caller(accounts.eve);
    oracle.commit_round(None).expect("commit should succeed");

    assert_eq!(oracle.current_round_id(), 1);
    assert_eq!(
        oracle.get_current_round_summary(),
        RoundSummary {
            round_id: 1,
            reporter_count: 0,
            median_price: None,
        }
    );
}

#[ink::test]
fn governance_sets_validator_and_reporters() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.bob);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.bob);

    assert_eq!(oracle.set_validator(Some(accounts.charlie)), Ok(()));
    assert_eq!(oracle.validator(), Some(accounts.charlie));
    assert_eq!(oracle.set_reporter(accounts.django, true), Ok(()));
    assert!(oracle.is_reporter(accounts.django));

    set_caller(accounts.eve);
    assert_eq!(
        oracle.set_validator(Some(accounts.eve)),
        Err(Error::NotGovernance)
    );
}

#[ink::test]
fn controller_updates_oracle_governance() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.bob);

    set_caller(accounts.bob);
    assert_eq!(
        oracle.update_governance(accounts.charlie),
        Err(Error::NotController)
    );

    set_caller(accounts.alice);
    assert_eq!(oracle.update_governance(accounts.charlie), Ok(()));
    assert_eq!(oracle.governance(), accounts.charlie);

    set_caller(accounts.bob);
    assert_eq!(
        oracle.set_reporter(accounts.django, true),
        Err(Error::NotGovernance)
    );

    set_caller(accounts.charlie);
    assert_eq!(oracle.set_reporter(accounts.django, true), Ok(()));
}

#[ink::test]
fn committed_round_history_is_queryable() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    assert_eq!(oracle.set_validator(Some(accounts.bob)), Ok(()));

    set_caller(accounts.bob);
    set_time(10);
    let round_0 = oracle
        .commit_round(Some(Ratio::from_integer(11)))
        .expect("first override commit should succeed");

    set_time(20);
    let round_1 = oracle
        .commit_round(Some(Ratio::from_integer(22)))
        .expect("second override commit should succeed");

    assert_eq!(oracle.get_round_price(0), Some(round_0));
    assert_eq!(oracle.get_round_price(1), Some(round_1));
    assert_eq!(oracle.get_price_history_count(), 2);
    assert_eq!(oracle.get_price_history(0), vec![round_1, round_0]);
    assert_eq!(oracle.get_price_history(1), Vec::<PriceData>::new());
}

#[ink::test]
fn committed_round_history_supports_pagination() {
    let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();
    set_caller(accounts.alice);
    let mut oracle = TusdtOracle::new(accounts.alice, accounts.alice);
    assert_eq!(oracle.set_validator(Some(accounts.bob)), Ok(()));

    set_caller(accounts.bob);
    for round_id in 0..12_u32 {
        set_time(round_id as u64);
        oracle
            .commit_round(Some(Ratio::from_integer(round_id as u128 + 1)))
            .expect("override commit should succeed");
    }

    assert_eq!(oracle.get_price_history_count(), 12);

    let page_0 = oracle.get_price_history(0);
    assert_eq!(page_0.len(), 10);
    assert_eq!(page_0[0].round_id, 11);
    assert_eq!(page_0[9].round_id, 2);

    let page_1 = oracle.get_price_history(1);
    assert_eq!(page_1.len(), 2);
    assert_eq!(page_1[0].round_id, 1);
    assert_eq!(page_1[1].round_id, 0);

    assert!(oracle.get_price_history(2).is_empty());
}
