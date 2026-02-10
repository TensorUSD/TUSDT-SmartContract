#![cfg_attr(not(feature = "std"), no_std, no_main)]

#[ink::contract]
mod vault {
    use core::cmp::min;
    use ink::prelude::vec::Vec;
    use ink::ToAccountId;

    use ink::storage::{Mapping, StorageVec};

    use tusdt_erc20::TusdtErc20Ref;

    const PAGE_SIZE: u32 = 10;

    #[derive(Debug, Clone)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct Vault {
        pub owner: AccountId,
        pub collateral_balance: Balance,
        pub borrowed_token_balance: Balance,
        pub created_at: u64,
    }

    #[ink(storage)]
    pub struct TusdtVault {
        // Token address of tusdt.
        token: TusdtErc20Ref,

        vaults: Mapping<(AccountId, u64), Vault>,
        vault_count: Mapping<AccountId, u64>,
        vault_keys: StorageVec<(AccountId, u64)>,
    }

    #[ink(event)]
    pub struct VaultCreated {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u64,
        amount: Balance,
    }

    #[ink(event)]
    pub struct CollateralAdded {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u64,
        amount: Balance,
    }

    #[ink(event)]
    pub struct CollateralReleased {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u64,
        amount: Balance,
    }

    #[ink(event)]
    pub struct TokensBorrowed {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u64,
        amount: Balance,
    }

    #[ink(event)]
    pub struct TokensRepaid {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u64,
        amount: Balance,
    }

    #[derive(Debug, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    pub enum Error {
        VaultNotFound,
        InsufficientCollateral,
        NotVaultOwner,
        TransferFailed,
        TokenBorrowedNotZero,
        OutOfBoundPageSize,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    impl TusdtVault {
        #[ink(constructor)]
        pub fn new(token_code_hash: Hash) -> Self {
            let token = TusdtErc20Ref::new()
                .code_hash(token_code_hash)
                .endowment(0)
                .salt_bytes([0; 32])
                .instantiate();

            Self {
                token,
                vaults: Mapping::default(),
                vault_count: Mapping::default(),
                vault_keys: StorageVec::default(),
            }
        }

        #[ink(message, payable)]
        pub fn create_vault(&mut self) -> Result<u64> {
            let caller = self.env().caller();
            let amount = self.env().transferred_value();
            let timestamp = self.env().block_timestamp();

            let vault_id = self.vault_count.get(caller).unwrap_or(0);
            let vault = Vault {
                owner: caller,
                collateral_balance: amount,
                borrowed_token_balance: 0,
                created_at: timestamp,
            };

            self.vaults.insert((caller, vault_id), &vault);
            self.vault_keys.push(&(caller, vault_id));

            let next_id = vault_id.checked_add(1).unwrap();
            self.vault_count.insert(caller, &next_id);

            self.env().emit_event(VaultCreated {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(vault_id)
        }

        #[ink(message, payable)]
        pub fn add_collateral(&mut self, vault_id: u64) -> Result<()> {
            let caller = self.env().caller();
            let amount = self.env().transferred_value();

            let mut vault = self
                .vaults
                .get((caller, vault_id))
                .ok_or(Error::VaultNotFound)?;

            if vault.owner != caller {
                return Err(Error::NotVaultOwner);
            }

            vault.collateral_balance = vault.collateral_balance.saturating_add(amount);
            self.vaults.insert((caller, vault_id), &vault);

            self.env().emit_event(CollateralAdded {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn borrow_token(&mut self, vault_id: u64, amount: Balance) -> Result<()> {
            let caller = self.env().caller();

            let mut vault = self
                .vaults
                .get((caller, vault_id))
                .ok_or(Error::VaultNotFound)?;

            if vault.owner != caller {
                return Err(Error::NotVaultOwner);
            }

            // TODO: Currently borrow ration is 1:1, need to add borrow ratio later.
            if vault.collateral_balance < amount {
                return Err(Error::InsufficientCollateral);
            }

            vault.borrowed_token_balance = vault.borrowed_token_balance.saturating_add(amount);
            self.vaults.insert((caller, vault_id), &vault);

            // Mint tusdt tokens to the caller.
            self.token
                .mint(caller, amount)
                .map_err(|_| Error::TransferFailed)?;

            self.env().emit_event(TokensBorrowed {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn repay_token(&mut self, vault_id: u64, amount: Balance) -> Result<()> {
            let caller = self.env().caller();

            let mut vault = self
                .vaults
                .get((caller, vault_id))
                .ok_or(Error::VaultNotFound)?;

            if vault.owner != caller {
                return Err(Error::NotVaultOwner);
            }

            vault.borrowed_token_balance = vault.borrowed_token_balance.saturating_sub(amount);
            self.vaults.insert((caller, vault_id), &vault);

            // Burn tusdt tokens from the caller.
            self.token
                .burn(caller, amount)
                .map_err(|_| Error::TransferFailed)?;

            self.env().emit_event(TokensRepaid {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn release_collateral(&mut self, vault_id: u64, amount: Balance) -> Result<()> {
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

            if vault.borrowed_token_balance > 0 {
                return Err(Error::TokenBorrowedNotZero);
            }

            vault.collateral_balance = vault.collateral_balance.saturating_sub(amount);
            self.vaults.insert((caller, vault_id), &vault);

            if self.env().transfer(caller, amount).is_err() {
                return Err(Error::TransferFailed);
            }

            self.env().emit_event(CollateralReleased {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn get_vault(&self, owner: AccountId, vault_id: u64) -> Option<Vault> {
            self.vaults.get((owner, vault_id))
        }

        #[ink(message)]
        pub fn get_token_address(&self) -> AccountId {
            self.token.to_account_id()
        }

        #[ink(message)]
        pub fn get_vault_collateral_balance(
            &self,
            owner: AccountId,
            vault_id: u64,
        ) -> Option<Balance> {
            self.vaults
                .get((owner, vault_id))
                .map(|v| v.collateral_balance)
        }

        #[ink(message)]
        pub fn get_all_vaults(&self, page: u32) -> Result<Vec<Vault>> {
            let total_vaults = self.vault_keys.len();
            let start = page.saturating_mul(PAGE_SIZE);
            if start >= total_vaults {
                return Err(Error::OutOfBoundPageSize);
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
