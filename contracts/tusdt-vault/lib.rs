#![cfg_attr(not(feature = "std"), no_std, no_main)]

#[ink::contract]
mod vault {
    use core::cmp::min;
    use ink::prelude::vec::Vec;
    use ink::storage::{Mapping, StorageVec};
    use ink::ToAccountId;

    use tusdt_erc20::TusdtErc20Ref;

    const PAGE_SIZE: u32 = 10;
    const PARTS_PER_BILLION: u128 = 1_000_000_000;

    const DEFAULT_COLLATERAL_RATIO_PARTS: u32 = 1_500_000_000; // 150%
    const DEFAULT_LIQUIDATION_RATIO_PARTS: u32 = 1_200_000_000; // 120%
    const DEFAULT_INTEREST_RATE_PARTS: u32 = 50_000_000; // 5% APR

    const SECONDS_PER_YEAR: u128 = 31_536_000;

    #[derive(Debug, Clone)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct Vault {
        pub owner: AccountId,
        pub collateral_balance: Balance,
        pub borrowed_token_balance: Balance,
        pub created_at: u64,
        pub last_interest_accrued_at: u64,
    }

    #[ink(storage)]
    pub struct TusdtVault {
        owner: AccountId,

        // Token address of tusdt.
        token: TusdtErc20Ref,

        collateral_ratio_parts: u32,
        liquidation_ratio_parts: u32,
        interest_rate_parts: u32,

        vaults: Mapping<(AccountId, u32), Vault>,
        vault_count: Mapping<AccountId, u32>,
        vault_keys: StorageVec<(AccountId, u32)>,
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
    pub struct RatiosUpdated {
        collateral_ratio_parts: u32,
        liquidation_ratio_parts: u32,
        interest_rate_parts: u32,
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
        ArithmeticError,
        NotContractOwner,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    impl TusdtVault {
        #[ink(constructor)]
        pub fn new(token_code_hash: Hash) -> Self {
            let owner = Self::env().caller();
            let token = TusdtErc20Ref::new()
                .code_hash(token_code_hash)
                .endowment(0)
                .salt_bytes([0; 32])
                .instantiate();

            Self {
                owner,
                token,
                collateral_ratio_parts: DEFAULT_COLLATERAL_RATIO_PARTS,
                liquidation_ratio_parts: DEFAULT_LIQUIDATION_RATIO_PARTS,
                interest_rate_parts: DEFAULT_INTEREST_RATE_PARTS,
                vaults: Mapping::default(),
                vault_count: Mapping::default(),
                vault_keys: StorageVec::default(),
            }
        }

        #[ink(message)]
        pub fn set_ratios(
            &mut self,
            collateral_ratio_parts: u32,
            liquidation_ratio_parts: u32,
            interest_rate_parts: u32,
        ) -> Result<()> {
            if self.env().caller() != self.owner {
                return Err(Error::NotContractOwner);
            }

            Self::validate_ratios(
                collateral_ratio_parts,
                liquidation_ratio_parts,
                interest_rate_parts,
            )?;

            self.collateral_ratio_parts = collateral_ratio_parts;
            self.liquidation_ratio_parts = liquidation_ratio_parts;
            self.interest_rate_parts = interest_rate_parts;

            self.env().emit_event(RatiosUpdated {
                collateral_ratio_parts,
                liquidation_ratio_parts,
                interest_rate_parts,
            });

            Ok(())
        }

        #[ink(message, payable)]
        pub fn create_vault(&mut self) -> Result<u32> {
            let caller = self.env().caller();
            let amount = self.env().transferred_value();
            let timestamp = self.env().block_timestamp();

            let vault_id = self.vault_count.get(caller).unwrap_or(0);
            let vault = Vault {
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
            let caller = self.env().caller();
            let amount = self.env().transferred_value();

            let mut vault = self
                .vaults
                .get((caller, vault_id))
                .ok_or(Error::VaultNotFound)?;

            if vault.owner != caller {
                return Err(Error::NotVaultOwner);
            }

            vault.collateral_balance = vault
                .collateral_balance
                .checked_add(amount)
                .ok_or(Error::ArithmeticError)?;
            self.vaults.insert((caller, vault_id), &vault);

            self.env().emit_event(CollateralAdded {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn borrow_token(&mut self, vault_id: u32, amount: Balance) -> Result<()> {
            let caller = self.env().caller();

            let mut vault = self
                .vaults
                .get((caller, vault_id))
                .ok_or(Error::VaultNotFound)?;

            if vault.owner != caller {
                return Err(Error::NotVaultOwner);
            }

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
            self.vaults.insert((caller, vault_id), &vault);

            self.env().emit_event(TokensBorrowed {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn repay_token(&mut self, vault_id: u32, amount: Balance) -> Result<()> {
            let caller = self.env().caller();

            let mut vault = self
                .vaults
                .get((caller, vault_id))
                .ok_or(Error::VaultNotFound)?;

            if vault.owner != caller {
                return Err(Error::NotVaultOwner);
            }

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
            self.vaults.insert((caller, vault_id), &vault);

            self.env().emit_event(TokensRepaid {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn release_collateral(&mut self, vault_id: u32, amount: Balance) -> Result<()> {
            let caller = self.env().caller();

            let mut vault = self
                .vaults
                .get((caller, vault_id))
                .ok_or(Error::VaultNotFound)?;

            if vault.owner != caller {
                return Err(Error::NotVaultOwner);
            }

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
            self.vaults.insert((caller, vault_id), &vault);

            self.env().emit_event(CollateralReleased {
                owner: caller,
                vault_id,
                amount,
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
        pub fn get_ratios(&self) -> (u32, u32, u32) {
            (
                self.collateral_ratio_parts,
                self.liquidation_ratio_parts,
                self.interest_rate_parts,
            )
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

        fn validate_ratios(
            collateral_ratio_parts: u32,
            liquidation_ratio_parts: u32,
            interest_rate_parts: u32,
        ) -> Result<()> {
            // Collateral ration should be greater that 100%
            if (collateral_ratio_parts as u128) < PARTS_PER_BILLION {
                return Err(Error::InvalidRatio);
            }
            // Liquidation ration should be greater than 100%
            if (liquidation_ratio_parts as u128) < PARTS_PER_BILLION {
                return Err(Error::InvalidRatio);
            }
            // Intrest rate should be less than 100%
            if (interest_rate_parts as u128) > PARTS_PER_BILLION {
                return Err(Error::InvalidRatio);
            }
            Ok(())
        }

        fn mul_ratio(value: u128, ratio_parts: u32) -> Result<u128> {
            value
                .checked_mul(ratio_parts as u128)
                .ok_or(Error::ArithmeticError)?
                .checked_div(PARTS_PER_BILLION)
                .ok_or(Error::ArithmeticError)
        }

        fn div_ratio(value: u128, ratio_parts: u32) -> Result<u128> {
            if ratio_parts == 0 {
                return Err(Error::InvalidRatio);
            }
            value
                .checked_mul(PARTS_PER_BILLION)
                .ok_or(Error::ArithmeticError)?
                .checked_div(ratio_parts as u128)
                .ok_or(Error::ArithmeticError)
        }

        fn max_borrow_allowed(&self, collateral_balance: Balance) -> Result<Balance> {
            Self::div_ratio(collateral_balance, self.collateral_ratio_parts)
        }

        fn accrue_interest(&self, vault: &mut Vault) -> Result<()> {
            let now = self.env().block_timestamp();
            if now <= vault.last_interest_accrued_at {
                return Ok(());
            }
            if vault.borrowed_token_balance == 0 || self.interest_rate_parts == 0 {
                vault.last_interest_accrued_at = now;
                return Ok(());
            }

            // We checked that noe > vault.last_interest_accrued_at
            #[allow(clippy::arithmetic_side_effects)]
            let elapsed = (now - vault.last_interest_accrued_at) as u128;
            let yearly_interest =
                Self::mul_ratio(vault.borrowed_token_balance, self.interest_rate_parts)?;
            let interest = yearly_interest
                .checked_mul(elapsed)
                .ok_or(Error::ArithmeticError)?
                .checked_div(SECONDS_PER_YEAR)
                .ok_or(Error::ArithmeticError)?;

            vault.borrowed_token_balance = vault
                .borrowed_token_balance
                .checked_add(interest)
                .ok_or(Error::ArithmeticError)?;
            vault.last_interest_accrued_at = now;

            Ok(())
        }
    }
}
