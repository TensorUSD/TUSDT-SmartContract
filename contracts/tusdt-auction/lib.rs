#![cfg_attr(not(feature = "std"), no_std, no_main)]

pub use self::auction::{Auction, Bid, BidMetadata, TusdtAuction, TusdtAuctionRef};

#[ink::contract(env = tusdt_env::CustomEnvironment)]
mod auction {
    use core::cmp::min;
    use ink::{env::call::FromAccountId, prelude::vec::Vec, storage::Mapping};

    use tusdt_erc20::TusdtErc20Ref;

    const PAGE_SIZE: u32 = 10;
    const DEFAULT_AUCTION_DURATION_MS: u64 = 3_600_000;

    #[derive(Debug, Clone)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct Auction {
        pub id: u32,
        pub vault_owner: AccountId,
        pub vault_id: u32,

        pub collateral_balance: Balance,
        pub debt_balance: Balance,

        pub starts_at: u64,
        pub ends_at: u64,

        pub highest_bidder: Option<AccountId>,
        pub highest_bid: Balance,
        pub highest_bid_id: Option<u32>,
        pub bid_count: u32,

        pub is_finalized: bool,
    }

    #[derive(Debug, Clone)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct Bid {
        pub id: u32,
        pub auction_id: u32,
        pub bidder: AccountId,
        pub amount: Balance,
        pub metadata: Option<BidMetadata>,
        pub is_withdrawn: bool,
    }

    #[derive(Debug, Clone)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct BidMetadata {
        pub hot_key: AccountId,
    }

    #[ink(storage)]
    pub struct TusdtAuction {
        controller: AccountId,
        governance: AccountId,
        admin: Option<AccountId>,
        token: TusdtErc20Ref,

        auction_count: u32,
        auctions: Mapping<u32, Auction>,
        auction_bids: Mapping<(u32, u32), Bid>,
        auction_bidder_bids: Mapping<(u32, AccountId), u32>,

        active_vault_auction: Mapping<(AccountId, u32), u32>,
        active_auction_count: u32,
        active_auctions: Mapping<u32, u32>,
        active_auction_indices: Mapping<u32, u32>,
    }

    #[ink(event)]
    pub struct AuctionCreated {
        #[ink(topic)]
        auction_id: u32,
        #[ink(topic)]
        vault_owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        starts_at: u64,
        ends_at: u64,
    }

    #[ink(event)]
    pub struct BidPlaced {
        #[ink(topic)]
        auction_id: u32,
        #[ink(topic)]
        bid_id: u32,
        #[ink(topic)]
        bidder: AccountId,
        amount: Balance,
    }

    #[ink(event)]
    pub struct AuctionFinalized {
        #[ink(topic)]
        auction_id: u32,
        #[ink(topic)]
        winner: AccountId,
        highest_bid: Balance,
        debt_balance: Balance,
        highest_bid_metadata: Option<BidMetadata>,
    }

    #[ink(event)]
    pub struct RefundWithdrawn {
        #[ink(topic)]
        bidder: AccountId,
        amount: Balance,
    }

    #[ink(event)]
    pub struct WinningBidTransferred {
        #[ink(topic)]
        auction_id: u32,
        #[ink(topic)]
        recipient: AccountId,
        amount: Balance,
    }

    #[ink(event)]
    pub struct AuctionGovernanceUpdated {
        #[ink(topic)]
        previous_governance: AccountId,
        #[ink(topic)]
        new_governance: AccountId,
    }

    #[ink(event)]
    pub struct AdminUpdated {
        #[ink(topic)]
        admin: Option<AccountId>,
    }

    #[derive(Debug, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    pub enum Error {
        NotController,
        NotGovernance,
        NotAdmin,
        AuctionNotFound,
        BidNotFound,
        NotBidder,
        AuctionAlreadyExistsForVault,
        BidBelowDebtBalance,
        AuctionEnded,
        AuctionNotEnded,
        AuctionFinalized,
        AuctionHasNoBids,
        WinningBidLocked,
        WinningBidAlreadyTransferred,
        InvalidDuration,
        TransferFailed,
        NoRefundAvailable,
        BidAmountNotIncreased,
        ArithmeticError,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    struct PreparedBid {
        auction: Auction,
        bid: Bid,
        transfer_amount: Balance,
        is_new_bid: bool,
    }

    impl TusdtAuction {
        /// Initializes the auction contract with controller, governance, and token contract reference.
        #[ink(constructor)]
        pub fn new(controller: AccountId, governance: AccountId, token_address: AccountId) -> Self {
            let token = TusdtErc20Ref::from_account_id(token_address);
            Self {
                controller,
                governance,
                admin: None,
                token,

                auction_count: 0,
                auctions: Mapping::default(),
                auction_bids: Mapping::default(),
                auction_bidder_bids: Mapping::default(),

                active_vault_auction: Mapping::default(),
                active_auction_count: 0,
                active_auctions: Mapping::default(),
                active_auction_indices: Mapping::default(),
            }
        }

        /// Creates a new liquidation auction for a vault with specified collateral and debt, returning the auction ID.
        #[ink(message)]
        pub fn create_auction(
            &mut self,
            vault_owner: AccountId,
            vault_id: u32,
            collateral_balance: Balance,
            debt_balance: Balance,
            duration_ms: Option<u64>,
        ) -> Result<u32> {
            self.ensure_controller()?;

            if self
                .active_vault_auction
                .get((vault_owner, vault_id))
                .is_some()
            {
                return Err(Error::AuctionAlreadyExistsForVault);
            }

            let duration = duration_ms.unwrap_or(DEFAULT_AUCTION_DURATION_MS);
            if duration == 0 {
                return Err(Error::InvalidDuration);
            }

            let now = self.env().block_timestamp();
            let ends_at = now.checked_add(duration).ok_or(Error::ArithmeticError)?;

            let auction_id = self.auction_count;
            self.auction_count = self
                .auction_count
                .checked_add(1)
                .ok_or(Error::ArithmeticError)?;

            let auction = Auction {
                id: auction_id,
                vault_owner,
                vault_id,
                collateral_balance,
                debt_balance,
                starts_at: now,
                ends_at,
                highest_bidder: None,
                highest_bid: 0,
                highest_bid_id: None,
                bid_count: 0,
                is_finalized: false,
            };

            self.auctions.insert(auction_id, &auction);
            let active_index = self.active_auction_count;
            self.active_auctions.insert(active_index, &auction_id);
            self.active_auction_indices
                .insert(auction_id, &active_index);
            self.active_auction_count = self
                .active_auction_count
                .checked_add(1)
                .ok_or(Error::ArithmeticError)?;
            self.active_vault_auction
                .insert((vault_owner, vault_id), &auction_id);

            self.env().emit_event(AuctionCreated {
                auction_id,
                vault_owner,
                vault_id,
                starts_at: now,
                ends_at,
            });

            Ok(auction_id)
        }

        #[ink(message)]
        pub fn set_admin(&mut self, admin: Option<AccountId>) -> Result<()> {
            self.ensure_governance()?;
            self.admin = admin;
            self.env().emit_event(AdminUpdated { admin });
            Ok(())
        }

        #[ink(message)]
        pub fn update_governance(&mut self, new_governance: AccountId) -> Result<()> {
            self.ensure_controller()?;
            let previous_governance = self.governance;
            self.governance = new_governance;
            self.env().emit_event(AuctionGovernanceUpdated {
                previous_governance,
                new_governance,
            });
            Ok(())
        }

        /// Places a bid on an auction, transferring the bid amount and updating the highest bid if applicable.
        #[ink(message)]
        pub fn place_bid(
            &mut self,
            auction_id: u32,
            bid_amount: Balance,
            metadata: Option<BidMetadata>,
        ) -> Result<u32> {
            let bidder = self.env().caller();

            let auction = self
                .auctions
                .get(auction_id)
                .ok_or(Error::AuctionNotFound)?;

            self.ensure_bid_allowed(&auction, bidder)?;
            let prepared = self.prepare_bid(auction, auction_id, bidder, bid_amount, metadata)?;

            self.token
                .transfer_from(bidder, self.env().account_id(), prepared.transfer_amount)
                .map_err(|_| Error::TransferFailed)?;

            let bid_id = prepared.bid.id;
            self.auction_bids
                .insert((auction_id, bid_id), &prepared.bid);
            if prepared.is_new_bid {
                self.auction_bidder_bids
                    .insert((auction_id, bidder), &bid_id);
            }
            self.auctions.insert(auction_id, &prepared.auction);

            self.env().emit_event(BidPlaced {
                auction_id,
                bid_id,
                bidder,
                amount: bid_amount,
            });

            Ok(bid_id)
        }

        fn prepare_bid(
            &self,
            mut auction: Auction,
            auction_id: u32,
            bidder: AccountId,
            bid_amount: Balance,
            metadata: Option<BidMetadata>,
        ) -> Result<PreparedBid> {
            if bid_amount < auction.debt_balance {
                return Err(Error::BidBelowDebtBalance);
            }

            let existing_bid_id = self.auction_bidder_bids.get((auction_id, bidder));
            let (bid, transfer_amount, is_new_bid) = if let Some(bid_id) = existing_bid_id {
                let mut bid = self
                    .auction_bids
                    .get((auction_id, bid_id))
                    .ok_or(Error::BidNotFound)?;
                if bid_amount <= bid.amount {
                    return Err(Error::BidAmountNotIncreased);
                }

                let transfer_amount = bid_amount
                    .checked_sub(bid.amount)
                    .ok_or(Error::ArithmeticError)?;
                bid.amount = bid_amount;
                bid.metadata = metadata;
                (bid, transfer_amount, false)
            } else {
                let bid_id = auction.bid_count;
                auction.bid_count = auction
                    .bid_count
                    .checked_add(1)
                    .ok_or(Error::ArithmeticError)?;

                (
                    Bid {
                        id: bid_id,
                        auction_id,
                        bidder,
                        amount: bid_amount,
                        metadata,
                        is_withdrawn: false,
                    },
                    bid_amount,
                    true,
                )
            };

            if bid_amount > auction.highest_bid {
                auction.highest_bidder = Some(bidder);
                auction.highest_bid = bid_amount;
                auction.highest_bid_id = Some(bid.id);
            }

            Ok(PreparedBid {
                auction,
                bid,
                transfer_amount,
                is_new_bid,
            })
        }

        /// Finalizes an auction after it has ended, marking the highest bidder as winner.
        #[ink(message)]
        pub fn finalize_auction(&mut self, auction_id: u32) -> Result<()> {
            let mut auction = self
                .auctions
                .get(auction_id)
                .ok_or(Error::AuctionNotFound)?;

            if auction.is_finalized {
                return Err(Error::AuctionFinalized);
            }
            if self.env().block_timestamp() < auction.ends_at {
                return Err(Error::AuctionNotEnded);
            }
            if auction.bid_count == 0 || auction.highest_bidder.is_none() {
                return Err(Error::AuctionHasNoBids);
            }

            auction.is_finalized = true;
            self.active_vault_auction
                .remove((auction.vault_owner, auction.vault_id));
            self.remove_active_auction(auction_id)?;

            let winner = auction.highest_bidder.expect("should have winner");
            let highest_bid = auction.highest_bid;
            let highest_bid_metadata = auction
                .highest_bid_id
                .and_then(|bid_id| self.auction_bids.get((auction_id, bid_id)))
                .ok_or(Error::BidNotFound)?
                .metadata;
            let debt_balance = auction.debt_balance;
            self.auctions.insert(auction_id, &auction);

            self.env().emit_event(AuctionFinalized {
                auction_id,
                winner,
                highest_bid,
                debt_balance,
                highest_bid_metadata,
            });

            Ok(())
        }

        /// Transfers the winning bid out of the auction contract after finalization; only callable by the controller.
        #[ink(message)]
        pub fn transfer_winning_bid(
            &mut self,
            auction_id: u32,
            recipient: AccountId,
        ) -> Result<Balance> {
            self.ensure_controller()?;

            let auction = self
                .auctions
                .get(auction_id)
                .ok_or(Error::AuctionNotFound)?;
            if !auction.is_finalized {
                return Err(Error::AuctionNotEnded);
            }

            let winning_bid_id = auction.highest_bid_id.ok_or(Error::AuctionHasNoBids)?;
            let mut winning_bid = self
                .auction_bids
                .get((auction_id, winning_bid_id))
                .ok_or(Error::BidNotFound)?;
            if winning_bid.is_withdrawn {
                return Err(Error::WinningBidAlreadyTransferred);
            }

            self.token
                .transfer(recipient, winning_bid.amount)
                .map_err(|_| Error::TransferFailed)?;

            winning_bid.is_withdrawn = true;
            self.auction_bids
                .insert((auction_id, winning_bid_id), &winning_bid);

            self.env().emit_event(WinningBidTransferred {
                auction_id,
                recipient,
                amount: winning_bid.amount,
            });

            Ok(winning_bid.amount)
        }

        /// Withdraws refund for a losing bid after the auction is finalized.
        #[ink(message)]
        pub fn withdraw_refund(&mut self, auction_id: u32, bid_id: u32) -> Result<()> {
            let caller = self.env().caller();
            let mut bid = self
                .auction_bids
                .get((auction_id, bid_id))
                .ok_or(Error::BidNotFound)?;
            if bid.bidder != caller {
                return Err(Error::NotBidder);
            }
            if bid.is_withdrawn {
                return Err(Error::NoRefundAvailable);
            }

            let auction = self
                .auctions
                .get(auction_id)
                .ok_or(Error::AuctionNotFound)?;
            if !auction.is_finalized {
                return Err(Error::AuctionNotEnded);
            }
            if auction.highest_bid_id == Some(bid_id) {
                return Err(Error::WinningBidLocked);
            }

            self.token
                .transfer(caller, bid.amount)
                .map_err(|_| Error::TransferFailed)?;

            bid.is_withdrawn = true;
            self.auction_bids.insert((auction_id, bid_id), &bid);

            self.env().emit_event(RefundWithdrawn {
                bidder: caller,
                amount: bid.amount,
            });

            Ok(())
        }

        /// Retrieves the details of an auction by its ID.
        #[ink(message)]
        pub fn get_auction(&self, auction_id: u32) -> Option<Auction> {
            self.auctions.get(auction_id)
        }

        /// Returns the active auction ID for a vault, or None if no active auction exists.
        #[ink(message)]
        pub fn get_active_vault_auction(
            &self,
            vault_owner: AccountId,
            vault_id: u32,
        ) -> Option<u32> {
            self.active_vault_auction.get((vault_owner, vault_id))
        }

        /// Retrieves a specific bid from an auction by auction ID and bid ID.
        #[ink(message)]
        pub fn get_bid(&self, auction_id: u32, bid_id: u32) -> Option<Bid> {
            self.auction_bids.get((auction_id, bid_id))
        }

        /// Retrieves a specific bid from an auction by auction ID and Bidder.
        #[ink(message)]
        pub fn get_auction_bid(&self, auction_id: u32, bidder: AccountId) -> Option<Bid> {
            let bid_id = self.auction_bidder_bids.get((auction_id, bidder))?;
            self.auction_bids.get((auction_id, bid_id))
        }

        /// Returns a paginated list of all bids placed on an auction.
        #[ink(message)]
        pub fn get_bids(&self, auction_id: u32, page: u32) -> Result<Vec<Bid>> {
            let auction = self
                .auctions
                .get(auction_id)
                .ok_or(Error::AuctionNotFound)?;

            let total_bids = auction.bid_count;
            let start = page.saturating_mul(PAGE_SIZE);
            if start >= total_bids {
                return Ok(Vec::new());
            }
            let end = min(start.saturating_add(PAGE_SIZE), total_bids);

            let mut bids = Vec::new();
            for bid_id in start..end {
                let bid = self.auction_bids.get((auction_id, bid_id));
                bids.push(bid.expect("should be present"));
            }

            Ok(bids)
        }

        /// Returns the total number of auctions created.
        #[ink(message)]
        pub fn get_total_auctions_count(&self) -> u32 {
            self.auction_count
        }

        /// Returns the total number of active auctions.
        #[ink(message)]
        pub fn get_active_auctions_count(&self) -> u32 {
            self.active_auction_count
        }

        /// Returns a paginated list of all auctions.
        #[ink(message)]
        pub fn get_all_auctions(&self, page: u32) -> Result<Vec<Auction>> {
            let total_auctions = self.auction_count;
            let start = page.saturating_mul(PAGE_SIZE);
            if start >= total_auctions {
                return Ok(Vec::new());
            }
            let end = min(start.saturating_add(PAGE_SIZE), total_auctions);

            let mut auctions = Vec::new();
            for auction_id in start..end {
                let auction = self.auctions.get(auction_id);
                auctions.push(auction.expect("should be present"));
            }

            Ok(auctions)
        }

        /// Returns a paginated list of active auctions.
        #[ink(message)]
        pub fn get_active_auctions(&self, page: u32) -> Result<Vec<Auction>> {
            let total_active_auctions = self.active_auction_count;
            let start = page.saturating_mul(PAGE_SIZE);
            if start >= total_active_auctions {
                return Ok(Vec::new());
            }
            let end = min(start.saturating_add(PAGE_SIZE), total_active_auctions);

            let mut auctions = Vec::new();
            for index in start..end {
                let auction_id = self.active_auctions.get(index).expect("should be present");
                let auction = self.auctions.get(auction_id).expect("should be present");
                auctions.push(auction);
            }

            Ok(auctions)
        }

        fn remove_active_auction(&mut self, auction_id: u32) -> Result<()> {
            let active_index = self
                .active_auction_indices
                .get(auction_id)
                .ok_or(Error::AuctionNotFound)?;
            let last_index = self
                .active_auction_count
                .checked_sub(1)
                .ok_or(Error::ArithmeticError)?;

            if active_index != last_index {
                let last_auction_id = self
                    .active_auctions
                    .get(last_index)
                    .ok_or(Error::AuctionNotFound)?;
                self.active_auctions.insert(active_index, &last_auction_id);
                self.active_auction_indices
                    .insert(last_auction_id, &active_index);
            }

            self.active_auctions.remove(last_index);
            self.active_auction_indices.remove(auction_id);
            self.active_auction_count = last_index;

            Ok(())
        }

        #[inline]
        fn ensure_controller(&self) -> Result<()> {
            if self.env().caller() != self.controller {
                return Err(Error::NotController);
            }
            Ok(())
        }

        #[inline]
        fn ensure_governance(&self) -> Result<()> {
            if self.env().caller() != self.governance {
                return Err(Error::NotGovernance);
            }
            Ok(())
        }

        pub(crate) fn ensure_bid_allowed(
            &self,
            auction: &Auction,
            bidder: AccountId,
        ) -> Result<()> {
            if auction.is_finalized {
                return Err(Error::AuctionFinalized);
            }

            let now = self.env().block_timestamp();
            if now < auction.ends_at {
                return Ok(());
            }

            // If the auction ended and has bid nobody can place bid.
            if auction.bid_count > 0 {
                return Err(Error::AuctionEnded);
            }
            // If the auction ends and no bid, only admin can place the bid.
            if self.admin != Some(bidder) {
                return Err(Error::NotAdmin);
            }

            Ok(())
        }

        /// Returns the controller account ID.
        #[ink(message)]
        pub fn controller(&self) -> AccountId {
            self.controller
        }

        /// Returns the governance account ID.
        #[ink(message)]
        pub fn governance(&self) -> AccountId {
            self.governance
        }

        /// Returns the admin account ID.
        #[ink(message)]
        pub fn admin(&self) -> Option<AccountId> {
            self.admin
        }
    }

    #[cfg(test)]
    impl TusdtAuction {
        pub(crate) fn seed_bid_for_test(
            &mut self,
            auction_id: u32,
            bidder: AccountId,
            amount: Balance,
        ) -> Result<()> {
            let mut auction = self
                .auctions
                .get(auction_id)
                .ok_or(Error::AuctionNotFound)?;

            auction.bid_count = 1;
            auction.highest_bidder = Some(bidder);
            auction.highest_bid = amount;
            auction.highest_bid_id = Some(0);
            self.auctions.insert(auction_id, &auction);
            self.auction_bids.insert(
                (auction_id, 0),
                &Bid {
                    id: 0,
                    auction_id,
                    bidder,
                    amount,
                    metadata: None,
                    is_withdrawn: false,
                },
            );
            self.auction_bidder_bids.insert((auction_id, bidder), &0);

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
