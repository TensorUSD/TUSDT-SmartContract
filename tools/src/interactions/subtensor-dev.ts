import type { ApiPromise } from "@polkadot/api";
import type { KeyringPair } from "@polkadot/keyring/types";

import { submitExtrinsic } from "../contract.js";

/**
 * Dev-node setup helpers for the lending-pool liquidation e2e.
 *
 * The test collateralizes the ROOT subnet (netuid 0): its alpha price is
 * always 1.0 (`swap` pallet root invariance), so no subnet registration is
 * needed — the only runtime prerequisites are hotkey Owner entries, root
 * subtokens enabled, and balances. All of these are set up here via the same
 * extrinsics subtensor's own ts-tests use on dev chains.
 */

export async function devForceSetBalance(
  api: ApiPromise,
  sudoer: KeyringPair,
  address: string,
  amount: bigint,
): Promise<void> {
  await submitExtrinsic(
    api,
    sudoer,
    api.tx.sudo.sudo(api.tx.balances.forceSetBalance(address, amount)),
  );
}

/** Creates the hotkey's Owner entry (`create_account_if_non_existent`). */
export async function devAssociateHotkey(
  api: ApiPromise,
  coldkey: KeyringPair,
  hotkey: string,
): Promise<void> {
  await submitExtrinsic(
    api,
    coldkey,
    api.tx.subtensorModule.tryAssociateHotkey(hotkey),
  );
}

/** Enables subtokens on a subnet (required for any stake transfer on it). */
export async function devSudoSetSubtokenEnabled(
  api: ApiPromise,
  sudoer: KeyringPair,
  netuid: number,
  enabled = true,
): Promise<void> {
  await submitExtrinsic(
    api,
    sudoer,
    api.tx.sudo.sudo(
      api.tx.adminUtils.sudoSetSubtokenEnabled(netuid, enabled),
    ),
  );
}

/** Stakes `amount` of the coldkey's TAO on a subnet under `hotkey`. */
export async function devAddStake(
  api: ApiPromise,
  coldkey: KeyringPair,
  hotkey: string,
  netuid: number,
  amount: bigint,
): Promise<void> {
  await submitExtrinsic(
    api,
    coldkey,
    api.tx.subtensorModule.addStake(hotkey, netuid, amount),
  );
}

/** Reads the stake a coldkey holds on (hotkey, netuid). */
export async function devGetStake(
  api: ApiPromise,
  hotkey: string,
  coldkey: string,
  netuid: number,
): Promise<bigint> {
  const value = (await api.query.subtensorModule.alphaV2(
    hotkey,
    coldkey,
    netuid,
  )) as unknown as { mantissa: bigint; exponent: bigint };
  const mantissa = value.mantissa;
  const exponent = value.exponent;
  if (exponent >= 0n) {
    return BigInt(mantissa) * 10n ** exponent;
  }
  return BigInt(mantissa) / 10n ** -exponent;
}
