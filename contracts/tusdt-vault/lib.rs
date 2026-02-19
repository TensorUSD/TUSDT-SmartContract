#![cfg_attr(not(feature = "std"), no_std, no_main)]

#[ink::contract]
mod vault {
    use core::cmp::min;
    use ink::prelude::vec::Vec;
    use ink::storage::{Mapping, StorageVec};
    use ink::ToAccountId;
    use tusdt_primitives::Ratio;

    use tusdt_auction::TusdtAuctionRef;
    use tusdt_erc20::TusdtErc20Ref;

    const PAGE_SIZE: u32 = 10;

    mod params {
        include!("params.rs");
    }
    mod interest {
        include!("interest.rs");
    }
    mod risk {
        include!("risk.rs");
    }
    mod vault_access {
        include!("vault_access.rs");
    }

    #[derive(Debug, Clone)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct Vault {
        pub id: u32,
        pub owner: AccountId,
        pub collateral_balance: Balance,
        pub borrowed_token_balance: Balance,
        pub created_at: u64,
        pub last_interest_accrued_at: u64,
    }

    #[derive(Debug, Copy, Clone)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct VaultContractParams {
        pub collateral_ratio: Ratio,
        pub liquidation_ratio: Ratio,
        pub interest_rate: Ratio,
        pub liquidation_fee: Ratio,
    }

    #[derive(Debug, Copy, Clone)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct VaultContractParamsPercentage {
        pub collateral_ratio: u32,
        pub liquidation_ratio: u32,
        pub interest_rate: u32,
        pub liquidation_fee: u32,
    }

    #[ink(storage)]
    pub struct TusdtVault {
        owner: AccountId,

        // Token address of tusdt.
        token: TusdtErc20Ref,
        // Auction contract address
        auction: TusdtAuctionRef,

        params: VaultContractParams,

        vaults: Mapping<(AccountId, u32), Vault>,
        vault_count: Mapping<AccountId, u32>,
        vault_keys: StorageVec<(AccountId, u32)>,
        liquidation_auctions: Mapping<(AccountId, u32), u64>,
    }

    #[ink(event)]
    pub struct VaultCreated {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        amount: Balance,
    }

    #[ink(event)]
    pub struct CollateralAdded {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        amount: Balance,
    }

    #[ink(event)]
    pub struct CollateralReleased {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        amount: Balance,
    }

    #[ink(event)]
    pub struct TokensBorrowed {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        amount: Balance,
    }

    #[ink(event)]
    pub struct TokensRepaid {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        amount: Balance,
    }

    #[ink(event)]
    pub struct ContractParamsUpdated {
        params: VaultContractParamsPercentage,
    }

    #[ink(event)]
    pub struct LiquidationAuctionCreated {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        #[ink(topic)]
        auction_id: u64,
    }

    #[ink(event)]
    pub struct VaultLiquidated {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        #[ink(topic)]
        auction_id: u64,
        winner: Option<AccountId>,
        winning_bid: Balance,
        collateral_sold: Balance,
        debt_cleared: Balance,
    }

    #[derive(Debug, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    pub enum Error {
        VaultNotFound,
        InsufficientCollateral,
        NotVaultOwner,
        TransferFailed,
        TokenBorrowedNotZero,
        OutOfBoundPage,
        InvalidRatio,
        MaxBorrowExceeded,
        LiquidationRatioExceeded,
        RepayAmountTooHigh,
        VaultInLiquidation,
        NotLiquidatable,
        LiquidationAuctionExists,
        AuctionContractCallFailed,
        AuctionNotFound,
        AuctionNotFinalized,
        ArithmeticError,
        NotContractOwner,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    impl TusdtVault {
        #[ink(constructor)]
        pub fn new(token_code_hash: Hash, auction_code_hash: Hash) -> Self {
            let owner = Self::env().caller();

            let contract_account = Self::env().account_id();
            let token = TusdtErc20Ref::new(contract_account)
                .code_hash(token_code_hash)
                .endowment(0)
                .salt_bytes([0; 32])
                .instantiate();
            let auction = TusdtAuctionRef::new(contract_account, token.to_account_id())
                .code_hash(auction_code_hash)
                .endowment(0)
                .salt_bytes([1; 32])
                .instantiate();

            let params = Self::default_contract_params();

            Self {
                owner,
                token,
                auction,
                params,
                vaults: Mapping::default(),
                vault_count: Mapping::default(),
                vault_keys: StorageVec::default(),
                liquidation_auctions: Mapping::default(),
            }
        }

        #[ink(message)]
        pub fn set_contract_params(&mut self, params: VaultContractParamsPercentage) -> Result<()> {
            if self.env().caller() != self.owner {
                return Err(Error::NotContractOwner);
            }

            let validated = Self::contract_params_from_percentages(params)?;
            self.params = validated;

            self.env().emit_event(ContractParamsUpdated { params });

            Ok(())
        }

        #[ink(message, payable)]
        pub fn create_vault(&mut self) -> Result<u32> {
            let caller = self.env().caller();
            let amount = self.env().transferred_value();
            let timestamp = self.env().block_timestamp();

            let vault_id = self.vault_count.get(caller).unwrap_or(0);
            let vault = Vault {
                id: vault_id,
                owner: caller,
                collateral_balance: amount,
                borrowed_token_balance: 0,
                created_at: timestamp,
                last_interest_accrued_at: timestamp,
            };

            self.vaults.insert((caller, vault_id), &vault);
            self.vault_keys.push(&(caller, vault_id));

            let next_id = vault_id.checked_add(1).ok_or(Error::ArithmeticError)?;
            self.vault_count.insert(caller, &next_id);

            self.env().emit_event(VaultCreated {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(vault_id)
        }

        #[ink(message, payable)]
        pub fn add_collateral(&mut self, vault_id: u32) -> Result<()> {
            let (caller, mut vault) = self.load_caller_vault(vault_id)?;
            let amount = self.env().transferred_value();

            vault.collateral_balance = vault
                .collateral_balance
                .checked_add(amount)
                .ok_or(Error::ArithmeticError)?;
            self.save_vault(caller, vault_id, &vault);

            self.env().emit_event(CollateralAdded {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn borrow_token(&mut self, vault_id: u32, amount: Balance) -> Result<()> {
            let (caller, mut vault) = self.load_caller_vault(vault_id)?;

            self.accrue_interest(&mut vault)?;

            let max_borrow = self.max_borrow_allowed(vault.collateral_balance)?;
            let projected_borrowed = vault
                .borrowed_token_balance
                .checked_add(amount)
                .ok_or(Error::ArithmeticError)?;
            if projected_borrowed > max_borrow {
                return Err(Error::MaxBorrowExceeded);
            }

            self.token
                .mint(caller, amount)
                .map_err(|_| Error::TransferFailed)?;

            vault.borrowed_token_balance = projected_borrowed;
            self.save_vault(caller, vault_id, &vault);

            self.env().emit_event(TokensBorrowed {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn repay_token(&mut self, vault_id: u32, amount: Balance) -> Result<()> {
            let (caller, mut vault) = self.load_caller_vault(vault_id)?;

            self.accrue_interest(&mut vault)?;
            if amount > vault.borrowed_token_balance {
                return Err(Error::RepayAmountTooHigh);
            }

            self.token
                .burn(caller, amount)
                .map_err(|_| Error::TransferFailed)?;

            vault.borrowed_token_balance = vault
                .borrowed_token_balance
                .checked_sub(amount)
                .ok_or(Error::ArithmeticError)?;
            self.save_vault(caller, vault_id, &vault);

            self.env().emit_event(TokensRepaid {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn release_collateral(&mut self, vault_id: u32, amount: Balance) -> Result<()> {
            let (caller, mut vault) = self.load_caller_vault(vault_id)?;

            if vault.collateral_balance < amount {
                return Err(Error::InsufficientCollateral);
            }

            self.accrue_interest(&mut vault)?;
            if vault.borrowed_token_balance > 0 {
                return Err(Error::TokenBorrowedNotZero);
            }

            if self.env().transfer(caller, amount).is_err() {
                return Err(Error::TransferFailed);
            }

            vault.collateral_balance = vault
                .collateral_balance
                .checked_sub(amount)
                .ok_or(Error::ArithmeticError)?;
            self.save_vault(caller, vault_id, &vault);

            self.env().emit_event(CollateralReleased {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn trigger_liquidation_auction(
            &mut self,
            owner: AccountId,
            vault_id: u32,
            duration_secs: Option<u64>,
        ) -> Result<u64> {
            if self.liquidation_auctions.get((owner, vault_id)).is_some() {
                return Err(Error::LiquidationAuctionExists);
            }

            let mut vault = self.load_vault(owner, vault_id)?;
            self.accrue_interest(&mut vault)?;

            if !self.is_liquidatable(&vault)? {
                return Err(Error::NotLiquidatable);
            }

            let collateral_debt = vault.borrowed_token_balance;
            let collateral_to_auction = collateral_debt
                .checked_add(
                    self.params
                        .liquidation_fee
                        .checked_mul_value(collateral_debt)
                        .ok_or(Error::ArithmeticError)?,
                )
                .ok_or(Error::ArithmeticError)?;
            let auction_id = self
                .auction
                .create_auction(
                    owner,
                    vault_id,
                    collateral_to_auction,
                    vault.borrowed_token_balance,
                    duration_secs,
                )
                .map_err(|_| Error::AuctionContractCallFailed)?;

            self.save_vault(owner, vault_id, &vault);
            self.liquidation_auctions
                .insert((owner, vault_id), &auction_id);

            self.env().emit_event(LiquidationAuctionCreated {
                owner,
                vault_id,
                auction_id,
            });

            Ok(auction_id)
        }

        #[ink(message)]
        pub fn settle_liquidation_auction(
            &mut self,
            owner: AccountId,
            vault_id: u32,
        ) -> Result<()> {
            let auction_id = self
                .liquidation_auctions
                .get((owner, vault_id))
                .ok_or(Error::AuctionNotFound)?;

            let auction = self
                .auction
                .get_auction(auction_id)
                .ok_or(Error::AuctionNotFound)?;

            if !auction.is_finalized {
                return Err(Error::AuctionNotFinalized);
            }

            let winner = auction.highest_bidder;
            let winning_bid = auction.highest_bid;

            let mut vault = self.load_vault(owner, vault_id)?;
            let mut collateral_sold = 0;
            let mut debt_cleared = 0;

            if let Some(winner) = winner {
                collateral_sold = auction.collateral_balance;
                debt_cleared = auction.debt_balance;

                if collateral_sold > 0 && self.env().transfer(winner, collateral_sold).is_err() {
                    return Err(Error::TransferFailed);
                }
                self.token
                    .burn(self.get_auction_address(), auction.highest_bid)
                    .map_err(|_| Error::TransferFailed)?;

                vault.collateral_balance = vault.collateral_balance.saturating_sub(collateral_sold);
                vault.borrowed_token_balance = 0;
            }

            self.save_vault(owner, vault_id, &vault);
            self.liquidation_auctions.remove((owner, vault_id));

            self.env().emit_event(VaultLiquidated {
                owner,
                vault_id,
                auction_id,
                winner,
                winning_bid,
                collateral_sold,
                debt_cleared,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn get_vault(&self, owner: AccountId, vault_id: u32) -> Option<Vault> {
            self.vaults.get((owner, vault_id))
        }

        #[ink(message)]
        pub fn get_token_address(&self) -> AccountId {
            self.token.to_account_id()
        }

        #[ink(message)]
        pub fn get_auction_address(&self) -> AccountId {
            self.auction.to_account_id()
        }

        #[ink(message)]
        pub fn get_contract_params(&self) -> VaultContractParamsPercentage {
            Self::contract_params_to_percentages(self.params)
        }

        #[ink(message)]
        pub fn get_vault_collateral_balance(
            &self,
            owner: AccountId,
            vault_id: u32,
        ) -> Option<Balance> {
            self.vaults
                .get((owner, vault_id))
                .map(|v| v.collateral_balance)
        }

        #[ink(message)]
        pub fn get_liquidation_auction_id(&self, owner: AccountId, vault_id: u32) -> Option<u64> {
            self.liquidation_auctions.get((owner, vault_id))
        }

        #[ink(message)]
        pub fn get_total_vaults_count(&self) -> u32 {
            self.vault_keys.len()
        }

        #[ink(message)]
        pub fn get_vaults_count(&self, owner: AccountId) -> u32 {
            self.vault_count.get(owner).unwrap_or_default()
        }

        #[ink(message)]
        pub fn get_vaults(&self, owner: AccountId, page: u32) -> Result<Vec<Vault>> {
            let total_owner_vaults = self.vault_count.get(owner).unwrap_or_default();
            let start = page.saturating_mul(PAGE_SIZE);
            if start >= total_owner_vaults {
                return Err(Error::OutOfBoundPage);
            }
            let end = min(start.saturating_add(PAGE_SIZE), total_owner_vaults);

            let mut vaults = Vec::new();
            let _ = (start..end).map(|index| {
                let vault = self.vaults.get((owner, index));
                vaults.push(vault.expect("should be present"));
            });

            Ok(vaults)
        }

        #[ink(message)]
        pub fn get_all_vaults(&self, page: u32) -> Result<Vec<Vault>> {
            let total_vaults = self.vault_keys.len();
            let start = page.saturating_mul(PAGE_SIZE);
            if start >= total_vaults {
                return Err(Error::OutOfBoundPage);
            }
            let end = min(start.saturating_add(PAGE_SIZE), total_vaults);

            let mut vaults = Vec::new();
            (start..end)
                .map(|index| self.vault_keys.get(index))
                .for_each(|key| {
                    if let Some(key) = key {
                        let vault = self.vaults.get(key);
                        vaults.push(vault.expect("should be present"));
                    }
                });

            Ok(vaults)
        }
    }
}
