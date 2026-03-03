use super::auction::*;
use super::*;

fn set_caller(caller: ink::primitives::AccountId) {
    let callee = ink::env::account_id::<ink::env::DefaultEnvironment>();
    ink::env::test::set_callee::<ink::env::DefaultEnvironment>(callee);
    ink::env::test::set_caller::<ink::env::DefaultEnvironment>(caller);
}

fn set_time(timestamp: u64) {
    ink::env::test::set_block_timestamp::<ink::env::DefaultEnvironment>(timestamp);
}

fn create_default_auction(
    contract: &mut TusdtAuction,
    vault_owner: ink::primitives::AccountId,
    vault_id: u32,
) -> u32 {
    contract
        .create_auction(vault_owner, vault_id, 1_000, 500, Some(1_000))
        .expect("create_auction should succeed")
}

#[ink::test]
fn new_works() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    assert_eq!(auction.owner(), accounts.alice);
    assert_eq!(auction.get_total_auctions_count(), 0);
    assert_eq!(auction.get_active_auctions_count(), 0);
}

#[ink::test]
fn create_auction_works() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    set_time(10);
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    let auction_id = create_default_auction(&mut auction, accounts.bob, 7);
    assert_eq!(auction_id, 0);
    assert_eq!(auction.get_total_auctions_count(), 1);
    assert_eq!(auction.get_active_auctions_count(), 1);
    assert_eq!(
        auction.get_active_vault_auction(accounts.bob, 7),
        Some(auction_id)
    );

    let created = auction
        .get_auction(auction_id)
        .expect("auction should exist");
    assert_eq!(created.vault_owner, accounts.bob);
    assert_eq!(created.vault_id, 7);
    assert_eq!(created.collateral_balance, 1_000);
    assert_eq!(created.debt_balance, 500);
    assert_eq!(created.starts_at, 10);
    assert_eq!(created.ends_at, 1_010);
    assert_eq!(created.highest_bid, 0);
    assert_eq!(created.highest_bidder, None);
    assert_eq!(created.is_finalized, false);
}

#[ink::test]
fn create_auction_fails_for_non_owner() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    set_caller(accounts.bob);
    assert_eq!(
        auction.create_auction(accounts.bob, 1, 1_000, 400, Some(1_000)),
        Err(Error::NotOwner)
    );
    assert_eq!(auction.get_total_auctions_count(), 0);
    assert_eq!(auction.get_active_auctions_count(), 0);
}

#[ink::test]
fn create_auction_fails_if_active_auction_exists_for_vault() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    create_default_auction(&mut auction, accounts.bob, 1);
    assert_eq!(
        auction.create_auction(accounts.bob, 1, 2_000, 800, Some(1_000)),
        Err(Error::AuctionAlreadyExistsForVault)
    );
}

#[ink::test]
fn create_auction_fails_on_invalid_duration() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    assert_eq!(
        auction.create_auction(accounts.bob, 1, 1_000, 400, Some(0)),
        Err(Error::InvalidDuration)
    );
}

#[ink::test]
fn place_bid_fails_when_auction_not_found() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    assert_eq!(auction.place_bid(42, 600), Err(Error::AuctionNotFound));
}

#[ink::test]
fn place_bid_fails_when_bid_is_below_debt() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    let auction_id = create_default_auction(&mut auction, accounts.bob, 2);
    assert_eq!(
        auction.place_bid(auction_id, 499),
        Err(Error::BidBelowDebtBalance)
    );
}

#[ink::test]
fn place_bid_fails_when_auction_has_ended() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    set_time(100);
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    let auction_id = create_default_auction(&mut auction, accounts.bob, 2);
    set_time(1_100);
    assert_eq!(auction.place_bid(auction_id, 600), Err(Error::AuctionEnded));
}

#[ink::test]
fn finalize_auction_works_after_end() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    set_time(200);
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    let auction_id = create_default_auction(&mut auction, accounts.bob, 3);
    set_time(1_200);
    assert_eq!(auction.finalize_auction(auction_id), Ok(()));

    let finalized = auction
        .get_auction(auction_id)
        .expect("auction should exist");
    assert_eq!(finalized.is_finalized, true);
    assert_eq!(finalized.highest_bidder, None);
    assert_eq!(finalized.highest_bid, 0);
    assert_eq!(auction.get_active_auctions_count(), 0);
    assert_eq!(auction.get_active_vault_auction(accounts.bob, 3), None);
}

#[ink::test]
fn finalize_auction_fails_before_end() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    set_time(50);
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    let auction_id = create_default_auction(&mut auction, accounts.bob, 4);
    set_time(999);
    assert_eq!(
        auction.finalize_auction(auction_id),
        Err(Error::AuctionNotEnded)
    );
}

#[ink::test]
fn finalize_auction_fails_if_already_finalized() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    set_time(100);
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    let auction_id = create_default_auction(&mut auction, accounts.bob, 5);
    set_time(1_100);
    assert_eq!(auction.finalize_auction(auction_id), Ok(()));
    assert_eq!(
        auction.finalize_auction(auction_id),
        Err(Error::AuctionFinalized)
    );
}

#[ink::test]
fn get_all_auctions_supports_pagination() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    for vault_id in 0..12 {
        assert_eq!(
            auction.create_auction(accounts.bob, vault_id, 1_000, 500, Some(2_000)),
            Ok(vault_id)
        );
    }

    let page0 = auction.get_all_auctions(0).expect("page 0 should exist");
    assert_eq!(page0.len(), 10);
    assert_eq!(page0[0].id, 0);
    assert_eq!(page0[9].id, 9);

    let page1 = auction.get_all_auctions(1).expect("page 1 should exist");
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].id, 10);
    assert_eq!(page1[1].id, 11);

    assert!(matches!(
        auction.get_all_auctions(2),
        Err(Error::OutOfBoundPage)
    ));
}

#[ink::test]
fn get_active_auctions_updates_after_finalize() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    set_time(1);
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    let first = create_default_auction(&mut auction, accounts.bob, 11);
    let second = create_default_auction(&mut auction, accounts.bob, 12);
    assert_eq!(first, 0);
    assert_eq!(second, 1);

    set_time(2_000);
    assert_eq!(auction.finalize_auction(first), Ok(()));

    let active = auction
        .get_active_auctions(0)
        .expect("active auctions should have one entry");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, second);
    assert_eq!(auction.get_active_auctions_count(), 1);
}

#[ink::test]
fn get_bids_fails_for_out_of_bounds_page_when_no_bids() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    let auction_id = create_default_auction(&mut auction, accounts.bob, 13);
    assert!(matches!(
        auction.get_bids(auction_id, 0),
        Err(Error::OutOfBoundPage)
    ));
}

#[ink::test]
fn withdraw_refund_fails_when_bid_not_found() {
    let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
    let mut auction = TusdtAuction::new(accounts.alice, accounts.charlie);

    let auction_id = create_default_auction(&mut auction, accounts.bob, 14);
    assert_eq!(
        auction.withdraw_refund(auction_id, 0),
        Err(Error::BidNotFound)
    );
}
