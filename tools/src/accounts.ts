import { Keyring } from "@polkadot/keyring";
import type { KeyringPair } from "@polkadot/keyring/types";
import { cryptoWaitReady } from "@polkadot/util-crypto";

export const DEV_ACCOUNT_SURIS = {
  alice: "//Alice",
  bob: "//Bob",
  charlie: "//Charlie",
  dave: "//Dave",
  eve: "//Eve",
  ferdie: "//Ferdie",
} as const;

export type DevAccountSuri =
  (typeof DEV_ACCOUNT_SURIS)[keyof typeof DEV_ACCOUNT_SURIS];

export interface DevAccounts {
  alice: KeyringPair;
  bob: KeyringPair;
  charlie: KeyringPair;
  dave: KeyringPair;
  eve: KeyringPair;
  ferdie: KeyringPair;
}

export async function getDevAccounts(): Promise<DevAccounts> {
  await cryptoWaitReady();

  const keyring = new Keyring({ type: "sr25519" });

  return {
    alice: keyring.addFromUri(DEV_ACCOUNT_SURIS.alice),
    bob: keyring.addFromUri(DEV_ACCOUNT_SURIS.bob),
    charlie: keyring.addFromUri(DEV_ACCOUNT_SURIS.charlie),
    dave: keyring.addFromUri(DEV_ACCOUNT_SURIS.dave),
    eve: keyring.addFromUri(DEV_ACCOUNT_SURIS.eve),
    ferdie: keyring.addFromUri(DEV_ACCOUNT_SURIS.ferdie),
  };
}

export async function getAccountFromSuri(suri: string): Promise<KeyringPair> {
  await cryptoWaitReady();

  const keyring = new Keyring({ type: "sr25519" });
  return keyring.addFromUri(suri);
}
