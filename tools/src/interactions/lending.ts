import { ContractPromise } from "@polkadot/api-contract";
import type { ApiPromise } from "@polkadot/api";
import type { KeyringPair } from "@polkadot/keyring/types";

import { loadContractArtifact } from "../artifact.js";
import { decodeInkResult, toPrimitive } from "../codec.js";
import {
  type ContractQueryResult,
  type ExtrinsicWaitFor,
  instantiateContract,
  queryMessage,
  txMessage,
} from "../contract.js";
import { deployErc20, uploadContractCode } from "../deployment.js";

export const RATIO_SCALE = 10n ** 18n;

// ---------------------------------------------------------------------------
// Deployment
// ---------------------------------------------------------------------------

/**
 * Uploads the erc20 code and instantiates a TUSDT instance, returning both the
 * deployed contract and its code hash (needed by the pool constructor to spawn
 * its lTAO / lTUSDT children).
 */
export async function deployTusdtAndGetCodeHash(
  api: ApiPromise,
  signer: KeyringPair,
): Promise<{ tusdt: ContractPromise; codeHash: string }> {
  const upload = await uploadContractCode(api, signer, "erc20");
  const tusdt = await deployErc20(api, signer, signer.address);
  return { tusdt, codeHash: upload.codeHash };
}

/**
 * Instantiates the lending pool. The deployer becomes governance/maintainer/
 * platform. `ltoken_code_hash` must be the uploaded tusdt-erc20 code hash (the
 * constructor spawns lTAO and lTUSDT children from it).
 */
export async function deployLendingPool(
  api: ApiPromise,
  signer: KeyringPair,
  treasuryAddress: string,
  tusdtAddress: string,
  oracleAddress: string,
  ltokenCodeHash: string,
  poolHotkey: string,
  waitFor: ExtrinsicWaitFor = "inBlock",
): Promise<ContractPromise> {
  const artifact = loadContractArtifact("lending_pool");
  const deployed = await instantiateContract(
    api,
    signer,
    artifact.abi,
    artifact.wasm,
    "new",
    [treasuryAddress, tusdtAddress, oracleAddress, ltokenCodeHash, poolHotkey],
    waitFor,
  );
  return new ContractPromise(api, artifact.abi, deployed.address);
}

export function getLendingPool(api: ApiPromise, address: string): ContractPromise {
  const artifact = loadContractArtifact("lending_pool");
  return new ContractPromise(api, artifact.abi, address);
}

// ---------------------------------------------------------------------------
// TUSDT (tusdt-erc20) helpers — PSP22-ish: mint / approve / balance_of
// ---------------------------------------------------------------------------

export async function erc20Mint(
  api: ApiPromise,
  erc20: ContractPromise,
  signer: KeyringPair,
  to: string,
  amount: bigint,
) {
  return txMessage(api, erc20, "mint", signer, [to, amount]);
}

export async function erc20Approve(
  api: ApiPromise,
  erc20: ContractPromise,
  signer: KeyringPair,
  spender: string,
  amount: bigint,
) {
  return txMessage(api, erc20, "approve", signer, [spender, amount]);
}

export async function erc20BalanceOf(
  erc20: ContractPromise,
  callerAddress: string,
  owner: string,
): Promise<ContractQueryResult> {
  return queryMessage(erc20, "balance_of", callerAddress, [owner]);
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

export async function setApprovedNetuid(
  api: ApiPromise,
  pool: ContractPromise,
  signer: KeyringPair,
  netuid: number,
  approved: boolean,
) {
  return txMessage(api, pool, "set_approved_netuid", signer, [netuid, approved]);
}

export async function setAlphaParams(
  api: ApiPromise,
  pool: ContractPromise,
  signer: KeyringPair,
  netuid: number,
  config: {
    collateralFactor: number;
    liquidationThreshold: number;
    liquidationFee: number;
    supplyCap: number;
  },
) {
  return txMessage(api, pool, "set_alpha_params", signer, [
    netuid,
    {
      collateral_factor: config.collateralFactor,
      liquidation_threshold: config.liquidationThreshold,
      liquidation_fee: config.liquidationFee,
      supply_cap: config.supplyCap,
    },
  ]);
}

export async function supplyTao(
  api: ApiPromise,
  pool: ContractPromise,
  signer: KeyringPair,
  amount: bigint,
) {
  return txMessage(api, pool, "supply_tao", signer, [amount], amount);
}

export async function supplyTusdt(
  api: ApiPromise,
  pool: ContractPromise,
  signer: KeyringPair,
  amount: bigint,
) {
  return txMessage(api, pool, "supply_tusdt", signer, [amount]);
}

export async function depositAlpha(
  api: ApiPromise,
  pool: ContractPromise,
  signer: KeyringPair,
  netuid: number,
  amount: bigint,
) {
  return txMessage(api, pool, "deposit_alpha", signer, [netuid, amount]);
}

export async function borrowTao(
  api: ApiPromise,
  pool: ContractPromise,
  signer: KeyringPair,
  amount: bigint,
) {
  return txMessage(api, pool, "borrow_tao", signer, [amount]);
}

export async function borrowTusdt(
  api: ApiPromise,
  pool: ContractPromise,
  signer: KeyringPair,
  amount: bigint,
) {
  return txMessage(api, pool, "borrow_tusdt", signer, [amount]);
}

export async function liquidate(
  api: ApiPromise,
  pool: ContractPromise,
  signer: KeyringPair,
  borrower: string,
  value: bigint = 0n,
) {
  return txMessage(api, pool, "liquidate", signer, [borrower], value);
}

export async function coverDeficit(
  api: ApiPromise,
  pool: ContractPromise,
  signer: KeyringPair,
  marketId: number,
) {
  return txMessage(api, pool, "cover_deficit", signer, [marketId]);
}

export async function accrueMarketInterest(
  api: ApiPromise,
  pool: ContractPromise,
  signer: KeyringPair,
) {
  return txMessage(api, pool, "accrue_market_interest", signer, []);
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

export async function queryMarketState(
  pool: ContractPromise,
  callerAddress: string,
  marketId: number,
): Promise<ContractQueryResult> {
  return queryMessage(pool, "get_market_state", callerAddress, [marketId]);
}

export async function queryUserDebtDetails(
  pool: ContractPromise,
  callerAddress: string,
  marketId: number,
  user: string,
): Promise<ContractQueryResult> {
  return queryMessage(pool, "get_user_debt_details", callerAddress, [
    marketId,
    user,
  ]);
}

export async function queryUserDebt(
  pool: ContractPromise,
  callerAddress: string,
  marketId: number,
  user: string,
): Promise<ContractQueryResult> {
  return queryMessage(pool, "get_user_debt", callerAddress, [marketId, user]);
}

export async function queryPosition(
  pool: ContractPromise,
  callerAddress: string,
  marketId: number,
  user: string,
): Promise<ContractQueryResult> {
  return queryMessage(pool, "get_position", callerAddress, [marketId, user]);
}

export async function queryMarketDeficit(
  pool: ContractPromise,
  callerAddress: string,
  marketId: number,
): Promise<ContractQueryResult> {
  return queryMessage(pool, "get_market_deficit", callerAddress, [marketId]);
}

export async function queryGlobalParams(
  pool: ContractPromise,
  callerAddress: string,
): Promise<ContractQueryResult> {
  return queryMessage(pool, "get_global_params", callerAddress, []);
}

/** Decodes a `Result<T, Error>`-wrapped query's `Ok` value, or throws with the contract error. */
export function expectOk<T>(result: ContractQueryResult, label: string): T {
  const decoded = decodeInkResult(result.output);
  if (!decoded.ok) {
    throw new Error(
      `${label} failed: ${JSON.stringify(decoded.error)} (debug: ${result.debugMessage})`,
    );
  }
  return toPrimitive(decoded.value) as T;
}

/** Decodes an `Option<T>`-wrapped query: `Some(value)` or `null`. */
export function expectOption<T>(result: ContractQueryResult, label: string): T | null {
  const decoded = decodeInkResult(result.output);
  if (!decoded.ok) {
    throw new Error(`${label} failed: ${JSON.stringify(decoded.error)}`);
  }
  const option = toPrimitive(decoded.value) as
    | { isSome: boolean; value: T }
    | T
    | null;
  if (option === null || option === undefined) return null;
  if (typeof option === "object" && "isSome" in option) {
    return option.isSome ? option.value : null;
  }
  return option as T;
}
