#![cfg_attr(not(feature = "std"), no_std, no_main)]

pub use self::oracle::{PriceData, RoundSummary, TusdtOracle, TusdtOracleRef};

#[ink::contract(env = tusdt_env::CustomEnvironment)]
mod oracle {
    use ink::{prelude::vec::Vec, storage::Mapping};
    use tusdt_primitives::Ratio;

    const MIN_REPORTERS: u32 = 3;

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct PriceData {
        pub round_id: u32,
        pub price: Ratio,
        pub median_price: Ratio,
        pub reporter_count: u32,
        pub committed_at: u64,
        pub was_overridden: bool,
    }

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct RoundSummary {
        pub round_id: u32,
        pub reporter_count: u32,
        pub median_price: Option<Ratio>,
    }

    #[ink(storage)]
    pub struct TusdtOracle {
        owner: AccountId,
        reporters: Mapping<AccountId, bool>,
        current_round_id: u32,
        round_submissions: Mapping<(u32, AccountId), Ratio>,
        round_reporter_count: Mapping<u32, u32>,
        round_reporters: Mapping<(u32, u32), AccountId>,
        latest_price: Option<PriceData>,
    }

    #[ink(event)]
    pub struct ReporterUpdated {
        #[ink(topic)]
        reporter: AccountId,
        enabled: bool,
    }

    #[ink(event)]
    pub struct PriceSubmitted {
        #[ink(topic)]
        round_id: u32,
        #[ink(topic)]
        reporter: AccountId,
        price: Ratio,
        replaced_existing: bool,
    }

    #[ink(event)]
    pub struct RoundCommitted {
        #[ink(topic)]
        round_id: u32,
        committed_price: Ratio,
        median_price: Ratio,
        reporter_count: u32,
        was_overridden: bool,
    }

    #[derive(Debug, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    pub enum Error {
        NotOwner,
        NotReporter,
        InvalidPrice,
        NotEnoughSubmissions,
        ArithmeticError,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    impl TusdtOracle {
        #[ink(constructor)]
        pub fn new(owner: AccountId) -> Self {
            Self {
                owner,
                reporters: Mapping::default(),
                current_round_id: 0,
                round_submissions: Mapping::default(),
                round_reporter_count: Mapping::default(),
                round_reporters: Mapping::default(),
                latest_price: None,
            }
        }

        #[ink(message)]
        pub fn submit_price(&mut self, price: Ratio) -> Result<()> {
            let reporter = self.env().caller();
            if !self.is_reporter(reporter) {
                return Err(Error::NotReporter);
            }
            if price.is_zero() {
                return Err(Error::InvalidPrice);
            }

            let round_id = self.current_round_id;
            let replaced_existing = self.round_submissions.get((round_id, reporter)).is_some();
            if !replaced_existing {
                let reporter_count = self.round_reporter_count.get(round_id).unwrap_or(0);
                self.round_reporters.insert((round_id, reporter_count), &reporter);
                self.round_reporter_count.insert(
                    round_id,
                    &reporter_count
                        .checked_add(1)
                        .ok_or(Error::ArithmeticError)?,
                );
            }

            self.round_submissions.insert((round_id, reporter), &price);

            self.env().emit_event(PriceSubmitted {
                round_id,
                reporter,
                price,
                replaced_existing,
            });

            Ok(())
        }

        #[ink(message)]
        pub fn commit_round(&mut self, override_price: Option<Ratio>) -> Result<PriceData> {
            self.ensure_owner()?;

            let round_id = self.current_round_id;
            let reporter_count = self.round_reporter_count.get(round_id).unwrap_or(0);
            if reporter_count < MIN_REPORTERS {
                return Err(Error::NotEnoughSubmissions);
            }

            let median_price = self
                .compute_round_median(round_id)?
                .ok_or(Error::NotEnoughSubmissions)?;
            let committed_price = match override_price {
                Some(price) if price.is_zero() => return Err(Error::InvalidPrice),
                Some(price) => price,
                None => median_price,
            };
            let was_overridden = override_price.is_some();
            let price_data = PriceData {
                round_id,
                price: committed_price,
                median_price,
                reporter_count,
                committed_at: self.env().block_timestamp(),
                was_overridden,
            };

            self.latest_price = Some(price_data);
            self.current_round_id = self
                .current_round_id
                .checked_add(1)
                .ok_or(Error::ArithmeticError)?;

            self.env().emit_event(RoundCommitted {
                round_id,
                committed_price,
                median_price,
                reporter_count,
                was_overridden,
            });

            Ok(price_data)
        }

        #[ink(message)]
        pub fn set_reporter(&mut self, reporter: AccountId, enabled: bool) -> Result<()> {
            self.ensure_owner()?;
            self.reporters.insert(reporter, &enabled);
            self.env().emit_event(ReporterUpdated { reporter, enabled });
            Ok(())
        }

        #[ink(message)]
        pub fn get_latest_price(&self) -> Option<PriceData> {
            self.latest_price
        }

        #[ink(message)]
        pub fn get_current_round_summary(&self) -> RoundSummary {
            let round_id = self.current_round_id;
            RoundSummary {
                round_id,
                reporter_count: self.round_reporter_count.get(round_id).unwrap_or(0),
                median_price: self.compute_round_median(round_id).unwrap_or(None),
            }
        }

        #[ink(message)]
        pub fn is_reporter(&self, account: AccountId) -> bool {
            self.reporters.get(account).unwrap_or(false)
        }

        #[ink(message)]
        pub fn owner(&self) -> AccountId {
            self.owner
        }

        #[ink(message)]
        pub fn current_round_id(&self) -> u32 {
            self.current_round_id
        }

        fn ensure_owner(&self) -> Result<()> {
            if self.env().caller() != self.owner {
                return Err(Error::NotOwner);
            }
            Ok(())
        }

        fn compute_round_median(&self, round_id: u32) -> Result<Option<Ratio>> {
            let reporter_count = self.round_reporter_count.get(round_id).unwrap_or(0);
            if reporter_count == 0 {
                return Ok(None);
            }

            let mut prices = Vec::with_capacity(reporter_count as usize);
            for index in 0..reporter_count {
                let reporter = self
                    .round_reporters
                    .get((round_id, index))
                    .expect("reporter should exist for round");
                let price = self
                    .round_submissions
                    .get((round_id, reporter))
                    .expect("submission should exist for reporter");
                prices.push(price);
            }

            prices.sort_unstable();
            let middle_index = (prices.len() - 1) / 2;
            Ok(prices.get(middle_index).copied())
        }
    }
}

#[cfg(test)]
mod tests;
