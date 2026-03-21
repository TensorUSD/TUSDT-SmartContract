import type { ApiPromise } from "@polkadot/api";
import type { KeyringPair } from "@polkadot/keyring/types";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  DEV_ACCOUNT_SURIS,
  type DevAccountSuri,
  getAccountFromSuri,
} from "../src/accounts.js";
import { createApi } from "../src/api.js";
import { formatInkError } from "../src/codec.js";
import { assertContractExists } from "../src/contract.js";
import { getOptionalEnv } from "../src/env.js";
import {
  deployOracle,
  dryRunCommitRound,
  dryRunSubmitPrice,
  getOracleContract,
  queryIsReporter,
  queryOwner,
  setReporter,
  submitPrice,
} from "../src/interactions/oracle.js";

const DEFAULT_ORACLE_CONTRACT_OWNER: DevAccountSuri = DEV_ACCOUNT_SURIS.alice;
const DEFAULT_ORACLE_REPORTER_1: DevAccountSuri = DEV_ACCOUNT_SURIS.bob;
const DEFAULT_ORACLE_REPORTER_2: DevAccountSuri = DEV_ACCOUNT_SURIS.charlie;
const DEFAULT_ORACLE_REPORTER_3: DevAccountSuri = DEV_ACCOUNT_SURIS.dave;

const ORACLE_CONTRACT_OWNER_SURI =
  getOptionalEnv("ORACLE_CONTRACT_OWNER") ?? DEFAULT_ORACLE_CONTRACT_OWNER;
const ORACLE_REPORTER_1_SURI =
  getOptionalEnv("ORACLE_REPORTER_1") ?? DEFAULT_ORACLE_REPORTER_1;
const ORACLE_REPORTER_2_SURI =
  getOptionalEnv("ORACLE_REPORTER_2") ?? DEFAULT_ORACLE_REPORTER_2;
const ORACLE_REPORTER_3_SURI =
  getOptionalEnv("ORACLE_REPORTER_3") ?? DEFAULT_ORACLE_REPORTER_3;
const ORACLE_CONTRACT_ADDRESS = getOptionalEnv("ORACLE_CONTRACT_ADDRESS");

describe.sequential("tusdt-oracle on-chain flow", () => {
  let api: ApiPromise;
  let owner: KeyringPair;
  let reporters: [KeyringPair, KeyringPair, KeyringPair];
  let oracle: Awaited<ReturnType<typeof deployOracle>>;

  beforeAll(async () => {
    api = await createApi();
    owner = await getAccountFromSuri(ORACLE_CONTRACT_OWNER_SURI);
    reporters = [
      await getAccountFromSuri(ORACLE_REPORTER_1_SURI),
      await getAccountFromSuri(ORACLE_REPORTER_2_SURI),
      await getAccountFromSuri(ORACLE_REPORTER_3_SURI),
    ];
    if (ORACLE_CONTRACT_ADDRESS) {
      await assertContractExists(
        api,
        ORACLE_CONTRACT_ADDRESS,
        "Oracle contract",
      );
      oracle = getOracleContract(api, ORACLE_CONTRACT_ADDRESS);
    } else {
      oracle = await deployOracle(api, owner, owner.address);
    }
    console.log("Oracle Address: ", oracle.address.toHuman());
  });

  afterAll(async () => {
    await api.disconnect();
  });

  it("deploys with the configured owner", async () => {
    await expect(queryOwner(oracle, owner.address)).resolves.toBe(
      owner.address,
    );
  });

  it("lets the owner manage reporter status", async () => {
    await setReporter(api, oracle, owner, reporters[0].address, true);

    await expect(
      queryIsReporter(oracle, owner.address, reporters[0].address),
    ).resolves.toBe(true);
  });

  it("rejects non-reporters during submit_price dry runs", async () => {
    const result = await dryRunSubmitPrice(oracle, owner.address, 10);

    expect(result.decoded.ok).toBe(false);
    expect(formatInkError(result.decoded.error)).toContain("NotReporter");
  });

  it("rejects zero-price submissions", async () => {
    const result = await dryRunSubmitPrice(oracle, reporters[0].address, 0);

    expect(result.decoded.ok).toBe(false);
    expect(formatInkError(result.decoded.error)).toContain("InvalidPrice");
  });

  it("blocks round commit below quorum", async () => {
    await setReporter(api, oracle, owner, reporters[1].address, true);

    await submitPrice(api, oracle, reporters[0], 10);
    await submitPrice(api, oracle, reporters[1], 20);

    const result = await dryRunCommitRound(oracle, owner.address);

    expect(result.decoded.ok).toBe(false);
    expect(formatInkError(result.decoded.error)).toContain(
      "NotEnoughSubmissions",
    );
  });
});
