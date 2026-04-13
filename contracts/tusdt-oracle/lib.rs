#![cfg_attr(not(feature = "std"), no_std, no_main)]

pub use self::oracle::{
    PriceData, PriceSubmission, PriceSubmissionMetadata, RoundSummary, TusdtOracle, TusdtOracleRef,
};

#[ink::contract(env = tusdt_env::CustomEnvironment)]
mod oracle {
    use core::cmp::min;
    use ink::{prelude::vec::Vec, storage::Mapping};
    use tusdt_primitives::Ratio;

    const MIN_REPORTERS: u32 = 3;
    const PAGE_SIZE: u32 = 10;
    const MAX_ROUND_SUBMISSIONS: u32 = 256;

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
    pub struct PriceSubmissionMetadata {
        pub hot_key: AccountId,
    }

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    #[ink::scale_derive(Decode, Encode, TypeInfo)]
    #[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
    pub struct PriceSubmission {
        pub reporter: AccountId,
        pub price: Ratio,
        pub metadata: Option<PriceSubmissionMetadata>,
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
        controller: AccountId,
        governance: AccountId,

        validator: Option<AccountId>,
        reporters: Mapping<AccountId, bool>,

        current_round_id: u32,
        round_submissions: Mapping<(u32, AccountId), PriceSubmission>,
        round_reporter_count: Mapping<u32, u32>,
        round_reporters: Mapping<(u32, u32), AccountId>,
        committed_round_prices: Mapping<u32, PriceData>,
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
        metadata: Option<PriceSubmissionMetadata>,
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

    #[ink(event)]
    pub struct OracleGovernanceUpdated {
        #[ink(topic)]
        previous_governance: AccountId,
        #[ink(topic)]
        new_governance: AccountId,
    }

    #[ink(event)]
    pub struct ValidatorUpdated {
        #[ink(topic)]
        validator: Option<AccountId>,
    }

    #[derive(Debug, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    pub enum Error {
        NotController,
        NotGovernance,
        NotValidator,
        NotReporter,
        InvalidPrice,
        NotEnoughSubmissions,
        MedianUnavailable,
        MaxSubmissionsReached,
        ArithmeticError,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    impl TusdtOracle {
        /// Initializes the oracle contract with controller and governance accounts.
        #[ink(constructor)]
        pub fn new(controller: AccountId, governance: AccountId) -> Self {
            Self {
                controller,
                governance,
                validator: None,
                reporters: Mapping::default(),
                current_round_id: 0,
                round_submissions: Mapping::default(),
                round_reporter_count: Mapping::default(),
                round_reporters: Mapping::default(),
                committed_round_prices: Mapping::default(),
                latest_price: None,
            }
        }

        /// Submits or replaces the caller's price for the current round, optionally attaching metadata.
        #[ink(message)]
        pub fn submit_price(
            &mut self,
            price: Ratio,
            metadata: Option<PriceSubmissionMetadata>,
        ) -> Result<()> {
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
                if reporter_count >= MAX_ROUND_SUBMISSIONS {
                    return Err(Error::MaxSubmissionsReached);
                }
                self.round_reporters
                    .insert((round_id, reporter_count), &reporter);
                self.round_reporter_count.insert(
                    round_id,
                    &reporter_count
                        .checked_add(1)
                        .ok_or(Error::ArithmeticError)?,
                );
            }

            self.round_submissions.insert(
                (round_id, reporter),
                &PriceSubmission {
                    reporter,
                    price,
                    metadata,
                },
            );

            self.env().emit_event(PriceSubmitted {
                round_id,
                reporter,
                price,
                metadata,
                replaced_existing,
            });

            Ok(())
        }

        /// Commits the current round using the median submission price or an optional validator override.
        #[ink(message)]
        pub fn commit_round(&mut self, override_price: Option<Ratio>) -> Result<PriceData> {
            self.ensure_validator()?;

            let round_id = self.current_round_id;
            let reporter_count = self.round_reporter_count.get(round_id).unwrap_or(0);
            let round_median = self.compute_round_median(round_id)?;
            let (committed_price, median_price, was_overridden) = match override_price {
                Some(price) if price.is_zero() => return Err(Error::InvalidPrice),
                Some(price) => (price, round_median.unwrap_or(price), true),
                None => {
                    if reporter_count < MIN_REPORTERS {
                        return Err(Error::NotEnoughSubmissions);
                    }
                    let median_price = round_median.ok_or(Error::MedianUnavailable)?;
                    (median_price, median_price, false)
                }
            };
            let price_data = PriceData {
                round_id,
                price: committed_price,
                median_price,
                reporter_count,
                committed_at: self.env().block_timestamp(),
                was_overridden,
            };

            self.committed_round_prices.insert(round_id, &price_data);
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

        /// Enables or disables a reporter account.
        #[ink(message)]
        pub fn set_reporter(&mut self, reporter: AccountId, enabled: bool) -> Result<()> {
            self.ensure_governance()?;
            self.reporters.insert(reporter, &enabled);
            self.env().emit_event(ReporterUpdated { reporter, enabled });
            Ok(())
        }

        /// Sets or clears the validator account allowed to commit rounds.
        #[ink(message)]
        pub fn set_validator(&mut self, validator: Option<AccountId>) -> Result<()> {
            self.ensure_governance()?;
            self.validator = validator;
            self.env().emit_event(ValidatorUpdated { validator });
            Ok(())
        }

        /// Transfers oracle governance control to a new account.
        #[ink(message)]
        pub fn update_governance(&mut self, new_governance: AccountId) -> Result<()> {
            self.ensure_controller()?;
            let previous_governance = self.governance;
            self.governance = new_governance;
            self.env().emit_event(OracleGovernanceUpdated {
                previous_governance,
                new_governance,
            });
            Ok(())
        }

        /// Returns the most recently committed oracle price, if any.
        #[ink(message)]
        pub fn get_latest_price(&self) -> Option<PriceData> {
            self.latest_price
        }

        /// Returns the committed price data for a specific round, if it exists.
        #[ink(message)]
        pub fn get_round_price(&self, round_id: u32) -> Option<PriceData> {
            self.committed_round_prices.get(round_id)
        }

        /// Returns the number of committed rounds available in price history.
        #[ink(message)]
        pub fn get_price_history_count(&self) -> u32 {
            self.current_round_id
        }

        /// Returns a paginated history of committed round prices, newest first.
        #[ink(message)]
        pub fn get_price_history(&self, page: u32) -> Vec<PriceData> {
            let Some(latest_round_id) = self.latest_committed_round_id() else {
                return Vec::new();
            };

            let total_prices = self.current_round_id;
            let start = page.saturating_mul(PAGE_SIZE);
            if start >= total_prices {
                return Vec::new();
            }
            let end = min(start.saturating_add(PAGE_SIZE), total_prices);

            let mut history = Vec::new();
            for offset in start..end {
                let round_id = latest_round_id
                    .checked_sub(offset)
                    .expect("round id should exist within computed history page");
                let price_data = self
                    .committed_round_prices
                    .get(round_id)
                    .expect("committed round price should exist");
                history.push(price_data);
            }

            history
        }

        /// Returns all stored submissions for the given round in submission order.
        #[ink(message)]
        pub fn get_round_submissions(&self, round_id: u32) -> Vec<PriceSubmission> {
            let reporter_count = self.round_reporter_count.get(round_id).unwrap_or(0);
            let mut submissions = Vec::with_capacity(reporter_count as usize);

            for index in 0..reporter_count {
                let reporter = self
                    .round_reporters
                    .get((round_id, index))
                    .expect("reporter should exist for round");
                let submission = self
                    .round_submissions
                    .get((round_id, reporter))
                    .expect("submission should exist for reporter");
                submissions.push(submission);
            }

            submissions
        }

        /// Returns the current round summary, including reporter count and median when available.
        #[ink(message)]
        pub fn get_current_round_summary(&self) -> RoundSummary {
            let round_id = self.current_round_id;
            RoundSummary {
                round_id,
                reporter_count: self.round_reporter_count.get(round_id).unwrap_or(0),
                median_price: self.compute_round_median(round_id).unwrap_or(None),
            }
        }

        /// Returns whether the provided account is currently authorized as a reporter.
        #[ink(message)]
        pub fn is_reporter(&self, account: AccountId) -> bool {
            self.reporters.get(account).unwrap_or(false)
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

        /// Returns the validator account ID, if one is configured.
        #[ink(message)]
        pub fn validator(&self) -> Option<AccountId> {
            self.validator
        }

        /// Returns the current open round ID.
        #[ink(message)]
        pub fn current_round_id(&self) -> u32 {
            self.current_round_id
        }

        /// Returns the maximum number of unique submissions allowed in a round.
        #[ink(message)]
        pub fn max_round_submissions(&self) -> u32 {
            MAX_ROUND_SUBMISSIONS
        }

        fn ensure_controller(&self) -> Result<()> {
            if self.env().caller() != self.controller {
                return Err(Error::NotController);
            }
            Ok(())
        }

        fn ensure_governance(&self) -> Result<()> {
            if self.env().caller() != self.governance {
                return Err(Error::NotGovernance);
            }
            Ok(())
        }

        fn ensure_validator(&self) -> Result<()> {
            if self.validator != Some(self.env().caller()) {
                return Err(Error::NotValidator);
            }
            Ok(())
        }

        fn latest_committed_round_id(&self) -> Option<u32> {
            self.current_round_id.checked_sub(1)
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
                let submission = self
                    .round_submissions
                    .get((round_id, reporter))
                    .expect("submission should exist for reporter");
                prices.push(submission.price);
            }

            prices.sort_unstable();
            let middle_index = prices.len() / 2;
            if prices.len() % 2 == 1 {
                return Ok(prices.get(middle_index).copied());
            }

            let lower = prices
                .get(middle_index.saturating_sub(1))
                .copied()
                .ok_or(Error::MedianUnavailable)?;
            let upper = prices
                .get(middle_index)
                .copied()
                .ok_or(Error::MedianUnavailable)?;
            let average_inner = lower
                .into_inner()
                .checked_add(upper.into_inner())
                .ok_or(Error::ArithmeticError)?
                .checked_div(2)
                .ok_or(Error::ArithmeticError)?;
            Ok(Some(Ratio::from_inner(average_inner)))
        }
    }
}

#[cfg(test)]
mod tests;
