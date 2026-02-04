#![cfg_attr(not(feature = "std"), no_std, no_main)]

#[ink::contract]
mod vault {
    use ink::storage::Mapping;

    #[derive(Debug, Clone)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct Vault {
        pub owner: AccountId,
        pub balance: Balance,
        pub created_at: u64,
    }

    #[ink(storage)]
    pub struct TusdVault {
        vaults: Mapping<(AccountId, u64), Vault>,
        vault_count: Mapping<AccountId, u64>,
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
    pub struct TokensLocked {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        vault_id: u64,
        amount: Balance,
    }

    #[ink(event)]
    pub struct TokensWithdrawn {
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
        InsufficientBalance,
        NotVaultOwner,
        TransferFailed,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    impl TusdVault {
        #[ink(constructor)]
        pub fn new() -> Self {
            Self {
                vaults: Mapping::default(),
                vault_count: Mapping::default(),
            }
        }

        #[ink(constructor)]
        pub fn default() -> Self {
            Self::new()
        }

        #[ink(message, payable)]
        pub fn create_vault(&mut self) -> Result<u64> {
            let caller = self.env().caller();
            let amount = self.env().transferred_value();
            let timestamp = self.env().block_timestamp();

            // Get current vault count for this user
            let vault_id = self.vault_count.get(caller).unwrap_or(0);

            let vault = Vault {
                owner: caller,
                balance: amount,
                created_at: timestamp,
            };

            self.vaults.insert((caller, vault_id), &vault);

            // Increment vault count
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
        pub fn lock_tokens(&mut self, vault_id: u64) -> Result<()> {
            let caller = self.env().caller();
            let amount = self.env().transferred_value();

            let mut vault = self
                .vaults
                .get((caller, vault_id))
                .ok_or(Error::VaultNotFound)?;

            if vault.owner != caller {
                return Err(Error::NotVaultOwner);
            }

            vault.balance = vault.balance.checked_add(amount).unwrap();
            self.vaults.insert((caller, vault_id), &vault);

            self.env().emit_event(TokensLocked {
                owner: caller,
                vault_id,
                amount,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn withdraw(&mut self, vault_id: u64, amount: Balance) -> Result<()> {
            let caller = self.env().caller();

            let mut vault = self
                .vaults
                .get((caller, vault_id))
                .ok_or(Error::VaultNotFound)?;

            if vault.owner != caller {
                return Err(Error::NotVaultOwner);
            }

            if vault.balance < amount {
                return Err(Error::InsufficientBalance);
            }

            vault.balance = vault.balance.checked_sub(amount).unwrap();
            self.vaults.insert((caller, vault_id), &vault);

            if self.env().transfer(caller, amount).is_err() {
                return Err(Error::TransferFailed);
            }

            self.env().emit_event(TokensWithdrawn {
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
        pub fn get_vault_balance(&self, owner: AccountId, vault_id: u64) -> Option<Balance> {
            self.vaults.get((owner, vault_id)).map(|v| v.balance)
        }
    }
}
