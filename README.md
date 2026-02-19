# TUSD Contracts Workspace

Ink! contracts for a collateralized TUSD system with vault borrowing, interest accrual, and liquidation auctions.

## Prerequisites
- Rust stable toolchain
- `wasm32-unknown-unknown` target
- `cargo-contract`
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
cargo contract build --manifest-path contracts/tusdt-vault/Cargo.toml
```

Artifacts are produced in `target/ink/`.

## Deployment (Recommended Order)
`tusdt-vault` constructor requires **code hash** of both token and auction contracts, and then instantiates them internally.

1. Upload ERC20 code (`tusdt-erc20`) and capture code hash.
2. Upload Auction code (`tusdt-auction`) and capture code hash.
3. Instantiate Vault (`tusdt-vault::new`) with:
   - `token_code_hash`
   - `auction_code_hash`

Example CLI (adjust URL/account):

```bash
cargo contract upload \
  --manifest-path contracts/tusdt-erc20/Cargo.toml \
  --suri //Alice --url ws://127.0.0.1:9944

cargo contract upload \
  --manifest-path contracts/tusdt-auction/Cargo.toml \
  --suri //Alice --url ws://127.0.0.1:9944

cargo contract instantiate \
  --manifest-path contracts/tusdt-vault/Cargo.toml \
  --constructor new \
  --args <ERC20_CODE_HASH> <AUCTION_CODE_HASH> \
  --suri //Alice --url ws://127.0.0.1:9944
```

## Working Flow
### 1) Vault lifecycle
1. User creates vault with native collateral: `create_vault` (payable).
2. User adds collateral: `add_collateral` (payable).
3. User borrows token: `borrow_token`.
4. User repays token: `repay_token`.
5. User releases collateral (only when debt is zero): `release_collateral`.

### 2) Interest model
- Accrual is day-based.
- Growth model is compound interest equivalent to:
  `borrowed * e^(interest_rate * borrowed_days / 365)`.
- Implementation computes daily growth and compounds by elapsed full days.

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
  - `auction_duration_secs`

Default params:
- Collateral ratio: `150%`
- Liquidation ratio: `120%`
- Interest rate: `5%`
- Liquidation fee: `1%`
- Auction duration: `3600` seconds

## Useful Read Methods
- Vault: `get_vault`, `get_contract_params`, `get_vaults`, `get_all_vaults`
- Auction: `get_auction`, `get_active_vault_auction`, `get_bid`
- Token: `balance_of`, `allowance`, `total_supply`

## Notes
- `tusdt-vault` owns the token and auction instances it creates.
- Borrowing mints TUSDT to borrower.
- Repayment and settlement burn TUSDT.
