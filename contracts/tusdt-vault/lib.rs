#![cfg_attr(not(feature = "std"), no_std, no_main)]

#[ink::contract(env = tusdt_env::CustomEnvironment)]
mod vault {
    use core::cmp::min;
    use ink::prelude::vec::Vec;
    use ink::storage::{Mapping, StorageVec};
    use ink::ToAccountId;

    use tusdt_auction::TusdtAuctionRef;
    use tusdt_erc20::TusdtErc20Ref;
    use tusdt_oracle::{PriceData, TusdtOracleRef};
    use tusdt_primitives::Ratio;

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
        pub total_interest_accrued: Balance,
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
        pub borrow_cap: Balance,
        pub auction_duration_ms: u64,
        pub max_oracle_age_ms: u64,
    }

    #[derive(Debug, Copy, Clone)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct VaultContractParamsConfig {
        pub collateral_ratio: u32,
        pub liquidation_ratio: u32,
        pub interest_rate: u32,
        pub liquidation_fee: u32,
        pub borrow_cap: Balance,
        pub auction_duration_ms: u64,
        pub max_oracle_age_ms: u64,
    }

    #[ink(storage)]
    pub struct TusdtVault {
        governance: AccountId,
        paused: bool,

        // Token address of tusdt.
        token: TusdtErc20Ref,
        // Auction contract address
        auction: TusdtAuctionRef,
        // External oracle providing raw TUSDT units per 1 raw collateral unit.
        oracle: TusdtOracleRef,
        total_collateral_balance: Balance,

        params: VaultContractParams,

        vaults: Mapping<(AccountId, u32), Vault>,
        owner_total_debt: Mapping<AccountId, Balance>,
        vault_count: Mapping<AccountId, u32>,
        vault_keys: StorageVec<(AccountId, u32)>,
        liquidation_auctions: Mapping<(AccountId, u32), u32>,
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
    pub struct InterestAccrued {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        previous_borrowed_balance: Balance,
        borrowed_balance: Balance,
        accrued_at: u64,
    }

    #[ink(event)]
    pub struct ContractParamsUpdated {
        params: VaultContractParamsConfig,
    }

    #[ink(event)]
    pub struct VaultGovernanceUpdated {
        #[ink(topic)]
        previous_governance: AccountId,
        #[ink(topic)]
        new_governance: AccountId,
    }

    #[ink(event)]
    pub struct Paused {}

    #[ink(event)]
    pub struct Unpaused {}

    #[ink(event)]
    pub struct LiquidationAuctionCreated {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        #[ink(topic)]
        auction_id: u32,
    }

    #[ink(event)]
    pub struct VaultLiquidated {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u32,
        #[ink(topic)]
        auction_id: u32,
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
        InvalidRatio,
        InvalidAuctionDuration,
        CollateralRatioExceeded,
        LiquidationRatioExceeded,
        BorrowCapExceeded,
        RepayAmountTooHigh,
        VaultInLiquidation,
        NotLiquidatable,
        LiquidationAuctionExists,
        AuctionContractCallFailed,
        AuctionNotFound,
        AuctionNotFinalized,
        ArithmeticError,
        NotGovernance,
        ContractPaused,
        OracleCallFailed,
        OraclePriceUnavailable,
        OraclePriceStale,
        InvalidOracleMaxAge,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    impl TusdtVault {
        /// Initializes the vault contract by instantiating the token and auction contracts with the provided code hashes.
        #[ink(constructor)]
        pub fn new(token_code_hash: Hash, auction_code_hash: Hash, oracle_code_hash: Hash) -> Self {
            let governance = Self::env().caller();

            let contract_account = Self::env().account_id();
            let token = TusdtErc20Ref::new(contract_account)
                .code_hash(token_code_hash)
                .endowment(0)
                .salt_bytes([0; 32])
                .instantiate();
            let auction = TusdtAuctionRef::new(contract_account, governance, token.to_account_id())
                .code_hash(auction_code_hash)
                .endowment(0)
                .salt_bytes([1; 32])
                .instantiate();
            let oracle = TusdtOracleRef::new(contract_account, governance)
                .code_hash(oracle_code_hash)
                .endowment(0)
                .salt_bytes([2; 32])
                .instantiate();

            let params = Self::default_contract_params();

            Self {
                governance,
                paused: false,
                token,
                auction,
                oracle,
                total_collateral_balance: 0,
                params,
                vaults: Mapping::default(),
                owner_total_debt: Mapping::default(),
                vault_count: Mapping::default(),
                vault_keys: StorageVec::default(),
                liquidation_auctions: Mapping::default(),
            }
        }

        /// Updates contract parameters (collateral ratio, liquidation ratio, interest rate, etc.) with validation; only callable by governance.
        #[ink(message)]
        pub fn set_contract_params(&mut self, params: VaultContractParamsConfig) -> Result<()> {
            self.ensure_governance()?;

            let validated = Self::contract_params_from_config(params)?;
            self.params = validated;

            self.env().emit_event(ContractParamsUpdated { params });

            Ok(())
        }

        #[ink(message)]
        pub fn update_governance(&mut self, new_governance: AccountId) -> Result<()> {
            self.ensure_governance()?;

            self.sync_child_governance(new_governance)?;

            let previous_governance = self.governance;
            self.governance = new_governance;

            self.env().emit_event(VaultGovernanceUpdated {
                previous_governance,
                new_governance,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn pause(&mut self) -> Result<()> {
            self.ensure_governance()?;

            self.paused = true;
            self.env().emit_event(Paused {});

            Ok(())
        }

        #[ink(message)]
        pub fn unpause(&mut self) -> Result<()> {
            self.ensure_governance()?;

            self.paused = false;
            self.env().emit_event(Unpaused {});

            Ok(())
        }

        /// Creates a new vault for the caller with the transferred collateral and returns the vault ID.
        #[ink(message, payable)]
        pub fn create_vault(&mut self) -> Result<u32> {
            self.ensure_not_paused()?;

            let caller = self.env().caller();
            let amount = self.env().transferred_value();
            let timestamp = self.env().block_timestamp();

            let vault_id = self.vault_count.get(caller).unwrap_or(0);
            let vault = Vault {
                id: vault_id,
                owner: caller,
                collateral_balance: amount,
                borrowed_token_balance: 0,
                total_interest_accrued: 0,
                created_at: timestamp,
                last_interest_accrued_at: timestamp,
            };

            self.save_vault(caller, vault_id, &vault)?;
            self.vault_keys.push(&(caller, vault_id));
            self.total_collateral_balance = self
                .total_collateral_balance
                .checked_add(amount)
                .ok_or(Error::ArithmeticError)?;

            let next_id = vault_id.checked_add(1).ok_or(Error::ArithmeticError)?;
            self.vault_count.insert(caller, &next_id);

            self.env().emit_event(VaultCreated {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(vault_id)
        }

        /// Adds the transferred collateral amount to an existing vault.
        #[ink(message, payable)]
        pub fn add_collateral(&mut self, vault_id: u32) -> Result<()> {
            self.ensure_not_paused()?;

            let (caller, mut vault) = self.load_caller_vault(vault_id)?;
            let amount = self.env().transferred_value();

            vault.collateral_balance = vault
                .collateral_balance
                .checked_add(amount)
                .ok_or(Error::ArithmeticError)?;
            self.total_collateral_balance = self
                .total_collateral_balance
                .checked_add(amount)
                .ok_or(Error::ArithmeticError)?;
            self.save_vault(caller, vault_id, &vault)?;

            self.env().emit_event(CollateralAdded {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        /// Borrows tokens against the vault's collateral, validating collateral ratio and accruing interest.
        #[ink(message)]
        pub fn borrow_token(&mut self, vault_id: u32, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;

            let (caller, mut vault) = self.load_caller_vault(vault_id)?;

            self.accrue_interest_for_vault(&mut vault)?;

            // If amount is 0, we still want to accrue interest, but no need to mint tokens.
            if amount.eq(&0) {
                self.save_vault(caller, vault_id, &vault)?;
                return Ok(());
            }

            let price = self.current_collateral_price()?;

            let max_borrow = self.max_borrow_allowed(price, vault.collateral_balance)?;
            let projected_borrowed = vault
                .borrowed_token_balance
                .checked_add(amount)
                .ok_or(Error::ArithmeticError)?;
            if projected_borrowed > max_borrow {
                return Err(Error::CollateralRatioExceeded);
            }
            let projected_total_supply = self
                .token
                .total_supply()
                .checked_add(amount)
                .ok_or(Error::ArithmeticError)?;
            if projected_total_supply > self.params.borrow_cap {
                return Err(Error::BorrowCapExceeded);
            }

            self.token
                .mint(caller, amount)
                .map_err(|_| Error::TransferFailed)?;

            self.adjust_last_interest_accrued_at_for_new_borrow(&mut vault, amount)?;
            vault.borrowed_token_balance = projected_borrowed;
            self.save_vault(caller, vault_id, &vault)?;

            self.env().emit_event(TokensBorrowed {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        /// Repays borrowed tokens from a vault, accruing interest and burning the repaid tokens.
        #[ink(message)]
        pub fn repay_token(&mut self, vault_id: u32, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;

            let (caller, mut vault) = self.load_caller_vault(vault_id)?;

            self.accrue_interest_for_vault(&mut vault)?;
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
            self.save_vault(caller, vault_id, &vault)?;

            self.env().emit_event(TokensRepaid {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        /// Accrues any elapsed interest for a vault and returns the updated debt balance.
        #[ink(message)]
        pub fn accrue_interest(&mut self, owner: AccountId, vault_id: u32) -> Result<Balance> {
            self.ensure_not_paused()?;

            self.ensure_not_in_liquidation(owner, vault_id)?;

            let mut vault = self.load_vault(owner, vault_id)?;
            let previous_borrowed_balance = vault.borrowed_token_balance;

            self.accrue_interest_for_vault(&mut vault)?;
            let borrowed_balance = vault.borrowed_token_balance;
            let accrued_at = vault.last_interest_accrued_at;

            self.save_vault(owner, vault_id, &vault)?;
            self.env().emit_event(InterestAccrued {
                owner,
                vault_id,
                previous_borrowed_balance,
                borrowed_balance,
                accrued_at,
            });

            Ok(borrowed_balance)
        }

        /// Releases collateral from a vault while ensuring the remaining collateral maintains the minimum collateral ratio.
        #[ink(message)]
        pub fn release_collateral(&mut self, vault_id: u32, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;

            let (caller, mut vault) = self.load_caller_vault(vault_id)?;

            if vault.collateral_balance < amount {
                return Err(Error::InsufficientCollateral);
            }

            self.accrue_interest_for_vault(&mut vault)?;
            let projected_collateral = vault
                .collateral_balance
                .checked_sub(amount)
                .ok_or(Error::ArithmeticError)?;
            if vault.borrowed_token_balance > 0 {
                let price = self.current_collateral_price()?;
                let max_borrow_after_release =
                    self.max_borrow_allowed(price, projected_collateral)?;
                if vault.borrowed_token_balance > max_borrow_after_release {
                    return Err(Error::CollateralRatioExceeded);
                }
            }

            if self.env().transfer(caller, amount).is_err() {
                return Err(Error::TransferFailed);
            }

            self.total_collateral_balance = self
                .total_collateral_balance
                .checked_sub(amount)
                .ok_or(Error::ArithmeticError)?;
            vault.collateral_balance = projected_collateral;
            self.save_vault(caller, vault_id, &vault)?;

            self.env().emit_event(CollateralReleased {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        /// Initiates a liquidation auction for an unsafe vault, returning the auction ID if successful.
        #[ink(message)]
        pub fn trigger_liquidation_auction(
            &mut self,
            owner: AccountId,
            vault_id: u32,
        ) -> Result<u32> {
            self.ensure_not_paused()?;

            if self.liquidation_auctions.get((owner, vault_id)).is_some() {
                return Err(Error::LiquidationAuctionExists);
            }

            let mut vault = self.load_vault(owner, vault_id)?;
            self.accrue_interest_for_vault(&mut vault)?;
            let price = self.current_collateral_price()?;

            if !self.is_liquidatable(price, &vault)? {
                return Err(Error::NotLiquidatable);
            }

            let collateral_debt =
                Self::collateral_needed_for_debt(price, vault.borrowed_token_balance)?;
            let liquidation_fee = self
                .params
                .liquidation_fee
                .checked_mul_value(u128::from(collateral_debt))
                .ok_or(Error::ArithmeticError)?;
            let liquidation_fee =
                Balance::try_from(liquidation_fee).map_err(|_| Error::ArithmeticError)?;
            let collateral_to_auction = collateral_debt
                .checked_add(liquidation_fee)
                .ok_or(Error::ArithmeticError)?;
            let auction_id = self
                .auction
                .create_auction(
                    owner,
                    vault_id,
                    min(collateral_to_auction, vault.collateral_balance),
                    vault.borrowed_token_balance,
                    Some(self.params.auction_duration_ms),
                )
                .map_err(|_| Error::AuctionContractCallFailed)?;

            self.save_vault(owner, vault_id, &vault)?;
            self.liquidation_auctions
                .insert((owner, vault_id), &auction_id);

            self.env().emit_event(LiquidationAuctionCreated {
                owner,
                vault_id,
                auction_id,
            });

            Ok(auction_id)
        }

        /// Settles a finalized liquidation auction, transferring collateral to the winner and clearing vault debt.
        #[ink(message)]
        pub fn settle_liquidation_auction(
            &mut self,
            owner: AccountId,
            vault_id: u32,
        ) -> Result<()> {
            self.ensure_not_paused()?;

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
                    .burn(self.get_auction_address(), debt_cleared)
                    .map_err(|_| Error::TransferFailed)?;

                vault.collateral_balance = vault
                    .collateral_balance
                    .checked_sub(collateral_sold)
                    .ok_or(Error::ArithmeticError)?;
                self.total_collateral_balance = self
                    .total_collateral_balance
                    .checked_sub(collateral_sold)
                    .ok_or(Error::ArithmeticError)?;
                vault.borrowed_token_balance = 0;
            }

            self.save_vault(owner, vault_id, &vault)?;
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

        /// Retrieves the vault details for a given owner and vault ID.
        #[ink(message)]
        pub fn get_vault(&self, owner: AccountId, vault_id: u32) -> Option<Vault> {
            self.vaults.get((owner, vault_id))
        }

        /// Returns the account ID of the deployed ERC-20 token contract.
        #[ink(message)]
        pub fn get_token_address(&self) -> AccountId {
            self.token.to_account_id()
        }

        /// Returns the account ID of the deployed auction contract.
        #[ink(message)]
        pub fn get_auction_address(&self) -> AccountId {
            self.auction.to_account_id()
        }

        /// Returns the account ID of the oracle contract.
        #[ink(message)]
        pub fn get_oracle_address(&self) -> AccountId {
            self.oracle.to_account_id()
        }

        /// Returns the current governance account.
        #[ink(message)]
        pub fn governance(&self) -> AccountId {
            self.governance
        }

        /// Returns whether the contract is paused by governance.
        #[ink(message)]
        pub fn paused(&self) -> bool {
            self.paused
        }

        /// Returns the current contract parameters (collateral ratio, liquidation ratio, interest rate, etc.) in the external config format.
        #[ink(message)]
        pub fn get_contract_params(&self) -> VaultContractParamsConfig {
            Self::contract_params_to_config(self.params)
        }

        /// Returns the collateral balance for a vault, or None if the vault does not exist.
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

        /// Returns the total collateral balance across all vaults.
        #[ink(message)]
        pub fn get_total_collateral_balance(&self) -> Balance {
            self.total_collateral_balance
        }

        /// Returns the total borrowed debt across all vaults owned by an account.
        #[ink(message)]
        pub fn get_total_debt(&self, owner: AccountId) -> Balance {
            self.owner_total_debt.get(owner).unwrap_or_default()
        }

        /// Calculates the token value of a vault's collateral based on the current price ratio.
        #[ink(message)]
        pub fn get_vault_collateral_value(
            &self,
            owner: AccountId,
            vault_id: u32,
        ) -> Result<Balance> {
            let vault = self
                .vaults
                .get((owner, vault_id))
                .ok_or(Error::VaultNotFound)?;
            let price = self.current_collateral_price()?;
            Self::collateral_value(price, vault.collateral_balance)
        }

        /// Returns the maximum token amount that can be borrowed against a vault's collateral.
        #[ink(message)]
        pub fn get_max_borrow(&self, owner: AccountId, vault_id: u32) -> Result<Balance> {
            let vault = self
                .vaults
                .get((owner, vault_id))
                .ok_or(Error::VaultNotFound)?;
            let price = self.current_collateral_price()?;
            let max = self.max_borrow_allowed(price, vault.collateral_balance)?;

            Ok(max)
        }

        /// Returns the auction ID for an active liquidation of a vault, or None if there is no active liquidation.
        #[ink(message)]
        pub fn get_liquidation_auction_id(&self, owner: AccountId, vault_id: u32) -> Option<u32> {
            self.liquidation_auctions.get((owner, vault_id))
        }

        /// Returns the total number of vaults created across all owners.
        #[ink(message)]
        pub fn get_total_vaults_count(&self) -> u32 {
            self.vault_keys.len()
        }

        /// Returns the number of vaults owned by a specific account.
        #[ink(message)]
        pub fn get_vaults_count(&self, owner: AccountId) -> u32 {
            self.vault_count.get(owner).unwrap_or_default()
        }

        /// Returns a paginated list of vaults owned by a specific account.
        #[ink(message)]
        pub fn get_vaults(&self, owner: AccountId, page: u32) -> Result<Vec<Vault>> {
            let total_owner_vaults = self.vault_count.get(owner).unwrap_or_default();
            let start = page.saturating_mul(PAGE_SIZE);
            if start >= total_owner_vaults {
                return Ok(Vec::new());
            }
            let end = min(start.saturating_add(PAGE_SIZE), total_owner_vaults);

            let mut vaults = Vec::new();
            for index in start..end {
                let vault = self.vaults.get((owner, index));
                vaults.push(vault.expect("should be present"));
            }

            Ok(vaults)
        }

        /// Returns a paginated list of all vaults across all owners.
        #[ink(message)]
        pub fn get_all_vaults(&self, page: u32) -> Result<Vec<Vault>> {
            let total_vaults = self.vault_keys.len();
            let start = page.saturating_mul(PAGE_SIZE);
            if start >= total_vaults {
                return Ok(Vec::new());
            }
            let end = min(start.saturating_add(PAGE_SIZE), total_vaults);

            let mut vaults = Vec::new();
            for index in start..end {
                let key = self.vault_keys.get(index).expect("should be present");
                let vault = self.vaults.get(key);
                vaults.push(vault.expect("should be present"));
            }

            Ok(vaults)
        }

        pub(crate) fn validate_price_data(
            price_data: Option<PriceData>,
            now: u64,
            max_oracle_age_ms: u64,
        ) -> Result<PriceData> {
            let price_data = price_data.ok_or(Error::OraclePriceUnavailable)?;
            let age = now
                .checked_sub(price_data.committed_at)
                .ok_or(Error::OraclePriceStale)?;
            if age > max_oracle_age_ms {
                return Err(Error::OraclePriceStale);
            }
            if price_data.price.is_zero() {
                return Err(Error::OraclePriceUnavailable);
            }
            Ok(price_data)
        }

        pub(crate) fn current_collateral_price(&self) -> Result<Ratio> {
            let price_data = Self::validate_price_data(
                self.oracle.get_latest_price(),
                self.env().block_timestamp(),
                self.params.max_oracle_age_ms,
            )?;
            Ok(price_data.price)
        }

        pub(crate) fn sync_owner_total_debt(
            &mut self,
            owner: AccountId,
            previous_vault_debt: Balance,
            next_vault_debt: Balance,
        ) -> Result<()> {
            let owner_total_debt = self.owner_total_debt.get(owner).unwrap_or_default();
            let owner_total_debt = owner_total_debt
                .checked_sub(previous_vault_debt)
                .ok_or(Error::ArithmeticError)?
                .checked_add(next_vault_debt)
                .ok_or(Error::ArithmeticError)?;
            self.owner_total_debt.insert(owner, &owner_total_debt);
            Ok(())
        }

        #[inline]
        fn ensure_governance(&self) -> Result<()> {
            if self.env().caller() != self.governance {
                return Err(Error::NotGovernance);
            }
            Ok(())
        }

        #[inline]
        fn ensure_not_paused(&self) -> Result<()> {
            if self.paused {
                return Err(Error::ContractPaused);
            }
            Ok(())
        }

        #[cfg(not(test))]
        fn sync_child_governance(&mut self, new_governance: AccountId) -> Result<()> {
            self.auction
                .update_governance(new_governance)
                .map_err(|_| Error::AuctionContractCallFailed)?;
            self.oracle
                .update_governance(new_governance)
                .map_err(|_| Error::OracleCallFailed)?;
            Ok(())
        }

        #[cfg(test)]
        fn sync_child_governance(&mut self, _new_governance: AccountId) -> Result<()> {
            Ok(())
        }
    }

    #[cfg(test)]
    impl TusdtVault {
        pub(crate) fn new_for_test(governance: AccountId) -> Self {
            use ink::env::call::FromAccountId;

            let accounts = ink::env::test::default_accounts::<tusdt_env::CustomEnvironment>();

            Self {
                governance,
                paused: false,
                token: TusdtErc20Ref::from_account_id(accounts.charlie),
                auction: TusdtAuctionRef::from_account_id(accounts.django),
                oracle: TusdtOracleRef::from_account_id(accounts.eve),
                total_collateral_balance: 0,
                params: Self::default_contract_params(),
                vaults: Mapping::default(),
                owner_total_debt: Mapping::default(),
                vault_count: Mapping::default(),
                vault_keys: StorageVec::default(),
                liquidation_auctions: Mapping::default(),
            }
        }

        pub(crate) fn set_liquidation_auction_for_test(
            &mut self,
            owner: AccountId,
            vault_id: u32,
            auction_id: u32,
        ) {
            self.liquidation_auctions
                .insert((owner, vault_id), &auction_id);
        }
    }
}

#[cfg(test)]
mod tests;
