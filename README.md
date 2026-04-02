# TUSDT Contracts Workspace

Ink! contracts for a collateralized TUSDT system with vault borrowing, interest accrual, and liquidation auctions.

## Prerequisites

- Rust stable toolchain
- `wasm32-unknown-unknown` target
- `cargo-contract`
- Node.js + Yarn (for the isolated contract tooling under `tools/`)
- A Contracts-enabled Substrate node (local or remote)

```bash
rustup target add wasm32-unknown-unknown
cargo install --locked cargo-contract
```

## Build

Build all crates:

```bash
cargo check
```

Build contract artifacts (`.contract`, `.wasm`, metadata):

```bash
cargo contract build --manifest-path contracts/tusdt-erc20/Cargo.toml
cargo contract build --manifest-path contracts/tusdt-auction/Cargo.toml
cargo contract build --manifest-path contracts/tusdt-oracle/Cargo.toml
cargo contract build --manifest-path contracts/tusdt-vault/Cargo.toml
```

Artifacts are produced in `target/ink/`.

## Contract Tooling (`tools/`)

Shared deployment scripts and on-chain tests live in an isolated TypeScript subproject under `tools/`.
The current iteration exposes upload support for `erc20`, `auction`, `oracle`, and `vault`, plus a single `vault` deployment entrypoint that instantiates the whole runtime flow. 

Setup:

```bash
cd tools
yarn install
cp .env.example .env
```

Default `.env` values target a local dev node:

- `WS_URL=ws://127.0.0.1:9944`

Scripts and tests use the standard local dev accounts (`//Alice`, `//Bob`, `//Charlie`, `//Dave`, `//Eve`, `//Ferdie`) from the shared dev-account helper.
When needed, you can override the selected account via SURI environment variables such as `CONTRACT_UPLOADER=//Alice`, `CONTRACT_DEPLOYER=//Alice`.

Useful commands:

```bash
cd tools
yarn build:erc20-artifacts
yarn build:auction-artifacts
yarn build:oracle-artifacts
yarn build:vault-artifacts
yarn erc20:upload
yarn auction:upload
yarn oracle:upload
yarn vault:upload
yarn vault:deploy --token-code-hash <TOKEN_CODE_HASH> --auction-code-hash <AUCTION_CODE_HASH> --oracle-code-hash <ORACLE_CODE_HASH>
yarn test:oracle
```

## Deployment (Recommended Order)

`tusdt-vault` constructor requires the **code hash** of the token, auction, and oracle contracts, then instantiates all three internally.

1. Upload ERC20 code (`tusdt-erc20`) and capture code hash.
2. Upload Auction code (`tusdt-auction`) and capture code hash.
3. Upload Oracle code (`tusdt-oracle`) and capture code hash.
4. Instantiate Vault (`tusdt-vault::new`) with:
   - `token_code_hash`
   - `auction_code_hash`
   - `oracle_code_hash`

The deployed vault owner becomes the oracle owner during instantiation.

Example CLI (adjust URL/account):

```bash
cargo contract upload \
  --manifest-path contracts/tusdt-erc20/Cargo.toml \
  --suri //Alice --url ws://127.0.0.1:9944

cargo contract upload \
  --manifest-path contracts/tusdt-auction/Cargo.toml \
  --suri //Alice --url ws://127.0.0.1:9944

cargo contract upload \
  --manifest-path contracts/tusdt-oracle/Cargo.toml \
  --suri //Alice --url ws://127.0.0.1:9944

cargo contract instantiate \
  --manifest-path contracts/tusdt-vault/Cargo.toml \
  --constructor new \
  --args <ERC20_CODE_HASH> <AUCTION_CODE_HASH> <ORACLE_CODE_HASH> \
  --suri //Alice --url ws://127.0.0.1:9944
```

Prefer the `tools/` workflow above instead of using `cargo contract` for upload/deploy operations where a TS script already exists.
The current e2e test suite is intentionally oracle-only, and the only deployment script entrypoint is `vault:deploy`.

## Working Flow

### 1) Vault lifecycle

1. User creates vault with native collateral: `create_vault` (payable).
2. User adds collateral: `add_collateral` (payable).
3. User borrows token: `borrow_token`.
4. User repays token: `repay_token`.
5. Anyone can trigger vault debt accrual: `accrue_interest(owner, vault_id)`.
6. User releases collateral (only when debt is zero): `release_collateral`.

### 2) Interest model

- Accrual is hour-based.
- Growth model uses discrete hourly compounding from the configured annual interest rate.
- Implementation compounds by elapsed full hours and advances `last_interest_accrued_at` to the last fully accrued hour.

### 3) Liquidation flow

1. Anyone can call `trigger_liquidation_auction(owner, vault_id)` when vault exceeds liquidation threshold.
2. Auction contract creates an auction tied to that vault.
3. Bidders approve token allowance to auction contract, then call `place_bid`.
4. After end time, call `finalize_auction` on auction contract.
5. Vault settlement: `settle_liquidation_auction(owner, vault_id)`.

### 4) Admin flow

- Contract owner updates risk params (in percentages) with `set_contract_params`:
  - `collateral_ratio`
  - `liquidation_ratio`
  - `interest_rate`
  - `liquidation_fee`
  - `auction_duration_ms`
  - `max_oracle_age_ms`
- Oracle owner manages reporter access with `set_reporter`
- Oracle owner commits the active round with `commit_round`, optionally using a manual override price

Default params:

- Collateral ratio: `150%`
- Liquidation ratio: `120%`
- Interest rate: `5%`
- Liquidation fee: `1%`
- Auction duration: `3_600_000` milliseconds
- Max oracle age: `3_600_000` milliseconds

## Useful Read Methods

- Vault: `get_vault`, `get_total_debt`, `get_contract_params`, `get_oracle_address`, `get_vaults`, `get_all_vaults`
- Oracle: `get_latest_price`, `get_current_round_summary`, `is_reporter`
- Auction: `get_auction`, `get_active_vault_auction`, `get_bid`, `get_all_auctions`, `get_active_auctions`
- Token: `balance_of`, `allowance`, `total_supply`

## Notes

- `tusdt-vault` owns the token and auction instances it creates.
- `tusdt-vault` reads collateral pricing from the external oracle contract.
- Borrowing mints TUSDT to borrower.
- Repayment and settlement burn TUSDT.
