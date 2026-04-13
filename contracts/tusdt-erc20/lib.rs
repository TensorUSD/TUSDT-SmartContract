#![cfg_attr(not(feature = "std"), no_std, no_main)]

pub use self::tusdt::{TusdtErc20, TusdtErc20Ref};

#[ink::contract(env = tusdt_env::CustomEnvironment)]
mod tusdt {
    use ink::storage::Mapping;

    #[ink(storage)]
    pub struct TusdtErc20 {
        controller: AccountId,
        total_supply: Balance,
        balances: Mapping<AccountId, Balance>,
        allowances: Mapping<(AccountId, AccountId), Balance>,
    }

    #[ink(event)]
    pub struct Transfer {
        #[ink(topic)]
        from: Option<AccountId>,
        #[ink(topic)]
        to: Option<AccountId>,
        value: Balance,
    }

    #[ink(event)]
    pub struct Approval {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        spender: AccountId,
        value: Balance,
    }

    #[derive(Debug, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    pub enum Error {
        InsufficientBalance,
        InsufficientAllowance,
        NotController,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    impl TusdtErc20 {
        /// Initializes the token contract with the specified controller account.
        #[ink(constructor)]
        pub fn new(controller: AccountId) -> Self {
            Self {
                controller,
                total_supply: 0,
                balances: Mapping::default(),
                allowances: Default::default(),
            }
        }

        /// Returns the controller account ID.
        #[ink(message)]
        pub fn controller(&self) -> AccountId {
            self.controller
        }

        /// Returns the total supply of tokens in circulation.
        #[ink(message)]
        pub fn total_supply(&self) -> Balance {
            self.total_supply
        }

        /// Returns the token balance of an account.
        #[ink(message)]
        pub fn balance_of(&self, owner: AccountId) -> Balance {
            self.balance_of_impl(&owner)
        }

        #[inline]
        fn balance_of_impl(&self, owner: &AccountId) -> Balance {
            self.balances.get(owner).unwrap_or_default()
        }

        /// Returns the amount of tokens that a spender is allowed to transfer from an owner's account.
        #[ink(message)]
        pub fn allowance(&self, owner: AccountId, spender: AccountId) -> Balance {
            self.allowance_impl(&owner, &spender)
        }

        #[inline]
        fn allowance_impl(&self, owner: &AccountId, spender: &AccountId) -> Balance {
            self.allowances.get((owner, spender)).unwrap_or_default()
        }

        #[inline]
        fn set_allowance(&mut self, owner: AccountId, spender: AccountId, value: Balance) {
            self.allowances.insert((&owner, &spender), &value);
            self.env().emit_event(Approval {
                owner,
                spender,
                value,
            });
        }

        #[inline]
        fn ensure_controller(&self) -> Result<()> {
            if self.env().caller() != self.controller {
                return Err(Error::NotController);
            }
            Ok(())
        }

        /// Transfers tokens from the caller to a recipient account.
        #[ink(message)]
        pub fn transfer(&mut self, to: AccountId, value: Balance) -> Result<()> {
            let from = self.env().caller();
            self.transfer_from_to(&from, &to, value)
        }

        /// Mints new tokens and adds them to an account's balance; only callable by controller.
        #[ink(message)]
        pub fn mint(&mut self, to: AccountId, value: Balance) -> Result<()> {
            self.ensure_controller()?;
            let to_balance = self.balance_of_impl(&to);

            self.total_supply = self
                .total_supply
                .checked_add(value)
                .expect("mint overflow on total_supply");
            self.balances.insert(
                to,
                &to_balance
                    .checked_add(value)
                    .expect("mint overflow on balance"),
            );

            self.env().emit_event(Transfer {
                from: None,
                to: Some(to),
                value,
            });
            Ok(())
        }

        /// Burns tokens from an account, reducing the total supply; only callable by controller.
        #[ink(message)]
        pub fn burn(&mut self, from: AccountId, value: Balance) -> Result<()> {
            self.ensure_controller()?;
            let from_balance = self.balance_of_impl(&from);
            if from_balance < value {
                return Err(Error::InsufficientBalance);
            }
            // We checked that from_balance >= value
            #[allow(clippy::arithmetic_side_effects)]
            self.balances.insert(from, &(from_balance - value));

            self.total_supply = self
                .total_supply
                .checked_sub(value)
                .expect("burn underflow on total_supply");

            self.env().emit_event(Transfer {
                from: Some(from),
                to: None,
                value,
            });
            Ok(())
        }

        /// Approves a spender to transfer up to a specified amount of tokens on behalf of the caller.
        #[ink(message)]
        pub fn approve(&mut self, spender: AccountId, value: Balance) -> Result<()> {
            let owner = self.env().caller();
            self.set_allowance(owner, spender, value);
            Ok(())
        }

        /// Increases a spender's allowance by a specified amount.
        #[ink(message)]
        pub fn increase_allowance(&mut self, spender: AccountId, delta_value: Balance) -> Result<()> {
            let owner = self.env().caller();
            let allowance = self.allowance_impl(&owner, &spender);
            let updated_allowance = allowance.saturating_add(delta_value);
            self.set_allowance(owner, spender, updated_allowance);
            Ok(())
        }

        /// Decreases a spender's allowance by a specified amount.
        #[ink(message)]
        pub fn decrease_allowance(&mut self, spender: AccountId, delta_value: Balance) -> Result<()> {
            let owner = self.env().caller();
            let allowance = self.allowance_impl(&owner, &spender);
            let updated_allowance = allowance
                .checked_sub(delta_value)
                .ok_or(Error::InsufficientAllowance)?;
            self.set_allowance(owner, spender, updated_allowance);
            Ok(())
        }

        /// Transfers tokens on behalf of an owner account to a recipient, using the caller's allowance.
        #[ink(message)]
        pub fn transfer_from(
            &mut self,
            from: AccountId,
            to: AccountId,
            value: Balance,
        ) -> Result<()> {
            let caller = self.env().caller();
            let allowance = self.allowance_impl(&from, &caller);
            if allowance < value {
                return Err(Error::InsufficientAllowance);
            }
            self.transfer_from_to(&from, &to, value)?;
            // We checked that allowance >= value
            #[allow(clippy::arithmetic_side_effects)]
            self.allowances
                .insert((&from, &caller), &(allowance - value));
            Ok(())
        }

        fn transfer_from_to(
            &mut self,
            from: &AccountId,
            to: &AccountId,
            value: Balance,
        ) -> Result<()> {
            let from_balance = self.balance_of_impl(from);
            if from_balance < value {
                return Err(Error::InsufficientBalance);
            }
            // We checked that from_balance >= value
            #[allow(clippy::arithmetic_side_effects)]
            self.balances.insert(from, &(from_balance - value));
            let to_balance = self.balance_of_impl(to);
            self.balances
                .insert(to, &(to_balance.checked_add(value).unwrap()));
            self.env().emit_event(Transfer {
                from: Some(*from),
                to: Some(*to),
                value,
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
