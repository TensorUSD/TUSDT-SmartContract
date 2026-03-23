import type { ApiPromise, SubmittableResult } from "@polkadot/api";
import type { SignerOptions } from "@polkadot/api/submittable/types";
import { CodePromise, ContractPromise } from "@polkadot/api-contract";
import type { KeyringPair } from "@polkadot/keyring/types";

import { decodeInkResult, stringifyJson } from "./codec.js";

export interface DecodedContractEvent {
  identifier: string;
  args: unknown[];
}

export interface ContractQueryResult {
  gasRequired: unknown;
  output: unknown;
  debugMessage: string;
  decoded: ReturnType<typeof decodeInkResult>;
}

export interface SubmittedExtrinsic {
  blockHash: string;
  events: SubmittableResult["events"];
}

export type ExtrinsicWaitFor = "inBlock" | "finalized";

export interface ContractTxResult extends SubmittedExtrinsic {
  dryRun: ContractQueryResult;
  contractEvents: DecodedContractEvent[];
}

interface ContractInfoOption {
  isSome?: boolean;
}

export class ContractLogicError extends Error {
  readonly contractError: unknown;

  constructor(contractError: unknown) {
    super(`Contract rejected the call: ${stringifyJson(contractError)}`);
    this.contractError = contractError;
  }
}

const GAS_LIMIT_NUMERATOR = 1n;
const GAS_LIMIT_DENOMINATOR = 10n;

function scaleWeightPart(value: string | undefined, fallback: string): string {
  const base = BigInt(value ?? fallback);
  const scaled = (base * GAS_LIMIT_NUMERATOR) / GAS_LIMIT_DENOMINATOR;

  return scaled > 0n ? scaled.toString() : "1";
}

type ApiWithContractsMetadata = Pick<ApiPromise, "registry" | "consts">;

function createGasLimit(api: ApiWithContractsMetadata) {
  const fallback = {
    refTime: "1000000000000",
    proofSize: "262144",
  };

  const maxBlock = (
    api.consts.system as unknown as {
      blockWeights?: {
        maxBlock?: {
          refTime?: { toString(): string };
          proofSize?: { toString(): string };
        };
      };
    }
  ).blockWeights?.maxBlock;

  return api.registry.createType("WeightV2", {
    // Keep headroom below max block weight so submission itself does not trip block-limit checks.
    refTime: scaleWeightPart(maxBlock?.refTime?.toString(), fallback.refTime),
    proofSize: scaleWeightPart(
      maxBlock?.proofSize?.toString(),
      fallback.proofSize,
    ),
  });
}

function normalizeGasLimit(
  api: ApiWithContractsMetadata,
  gasLimit: unknown,
) {
  if (!gasLimit || typeof gasLimit !== "object") {
    return createGasLimit(api);
  }

  const limit = gasLimit as {
    refTime?: { toString(): string };
    proofSize?: { toString(): string };
  };

  return api.registry.createType("WeightV2", {
    refTime: limit.refTime?.toString() ?? "1000000000000",
    proofSize: limit.proofSize?.toString() ?? "262144",
  });
}

function decodeDispatchError(
  api: ApiWithContractsMetadata,
  error: unknown,
): string {
  const dispatchError = error as
    | {
        isModule?: boolean;
        asModule?: unknown;
        toString(): string;
      }
    | undefined;

  if (dispatchError?.isModule && dispatchError.asModule) {
    const metaError = api.registry.findMetaError(
      dispatchError.asModule as never,
    );
    return `${metaError.section}.${metaError.name}: ${metaError.docs.join(" ")}`;
  }

  return dispatchError?.toString() ?? "Unknown dispatch error";
}

function collectContractEvents(
  api: ApiPromise,
  contract: ContractPromise,
  address: string,
  events: SubmittableResult["events"],
): DecodedContractEvent[] {
  return events.flatMap((record) => {
    if (!api.events.contracts.ContractEmitted.is(record.event)) {
      return [];
    }

    const [emittedAddress, payload] = record.event.data;
    if (emittedAddress.toString() !== address) {
      return [];
    }

    const decoded = contract.abi.decodeEvent(record as never);
    return [
      {
        identifier: decoded.event.identifier,
        args: decoded.args.map((arg) => arg.toPrimitive()),
      },
    ];
  });
}

function resolveContractMessageName(
  contract: ContractPromise,
  message: string,
): string {
  try {
    return contract.abi.findMessage(message).method;
  } catch {
    return message;
  }
}

async function signAndWatch(
  api: ApiPromise,
  signer: KeyringPair,
  tx: {
    signAndSend: (
      signer: KeyringPair,
      options: Partial<SignerOptions>,
      cb: (result: SubmittableResult) => void,
    ) => Promise<() => void>;
  },
  waitFor: ExtrinsicWaitFor,
): Promise<SubmittedExtrinsic> {
  return new Promise((resolve, reject) => {
    let unsubscribe: (() => void) | undefined;

    const finish = (callback: () => void) => {
      if (unsubscribe) {
        unsubscribe();
        unsubscribe = undefined;
      }
      callback();
    };

    void api.rpc.system
      .accountNextIndex(signer.address)
      .then((nonce) =>
        tx.signAndSend(signer, { nonce }, (result) => {
          if (result.dispatchError) {
            finish(() =>
              reject(new Error(decodeDispatchError(api, result.dispatchError))),
            );
            return;
          }

          const failedEvent = result.events.find(({ event }) =>
            api.events.system.ExtrinsicFailed.is(event),
          );
          if (failedEvent) {
            finish(() =>
              reject(
                new Error(
                  decodeDispatchError(
                    api,
                    failedEvent.event.data[0] as unknown,
                  ),
                ),
              ),
            );
            return;
          }

          if (waitFor === "inBlock" && result.status.isInBlock) {
            const blockHash = result.status.asInBlock.toString();
            finish(() => resolve({ blockHash, events: result.events }));
            return;
          }

          if (waitFor === "finalized" && result.status.isFinalized) {
            const blockHash = result.status.asFinalized.toString();
            finish(() => resolve({ blockHash, events: result.events }));
          }
        }),
      )
      .then((unsub) => {
        unsubscribe = unsub;
      })
      .catch((error) => reject(error));
  });
}

export async function submitExtrinsic(
  api: ApiPromise,
  signer: KeyringPair,
  tx: {
    signAndSend: (
      signer: KeyringPair,
      options: Partial<SignerOptions>,
      cb: (result: SubmittableResult) => void,
    ) => Promise<() => void>;
  },
  waitFor: ExtrinsicWaitFor = "inBlock",
): Promise<SubmittedExtrinsic> {
  return signAndWatch(api, signer, tx, waitFor);
}

export async function contractExists(
  api: ApiPromise,
  address: string,
): Promise<boolean> {
  const contractsInfoOf = (
    api.query as unknown as {
      contracts?: {
        contractInfoOf?: (address: string) => Promise<ContractInfoOption>;
      };
    }
  ).contracts?.contractInfoOf;

  if (contractsInfoOf) {
    const contractInfo = await contractsInfoOf(address);
    return Boolean(contractInfo.isSome);
  }

  throw new Error(
    "The connected runtime does not expose contracts.contractInfoOf",
  );
}

export async function assertContractExists(
  api: ApiPromise,
  address: string,
  label = "Contract",
): Promise<void> {
  const exists = await contractExists(api, address);

  if (!exists) {
    throw new Error(`${label} was not found at address ${address}.`);
  }
}

export async function queryMessage(
  contract: ContractPromise,
  message: string,
  callerAddress: string,
  args: unknown[] = [],
  value: bigint | number = 0,
): Promise<ContractQueryResult> {
  const resolvedMessage = resolveContractMessageName(contract, message);
  const queryFn = (
    contract.query as unknown as Record<
      string,
      (
        address: string,
        options: {
          gasLimit: unknown;
          storageDepositLimit: null;
          value: bigint | number;
        },
        ...args: unknown[]
      ) => Promise<{
        gasRequired: unknown;
        result: { isErr: boolean; asErr?: { toString(): string } };
        output: unknown;
        debugMessage?: { toString(): string };
      }>
    >
  )[resolvedMessage];

  if (!queryFn) {
    throw new Error(`Unknown contract query message: ${message}`);
  }

  const result = await queryFn(
    callerAddress,
    {
      gasLimit: createGasLimit(contract.api),
      storageDepositLimit: null,
      value,
    },
    ...args,
  );

  if (result.result.isErr) {
    throw new Error(
      `Dry-run failed for ${message}: ${
        result.result.asErr
          ? decodeDispatchError(contract.api, result.result.asErr)
          : "unknown error"
      }`,
    );
  }

  return {
    gasRequired: result.gasRequired,
    output: result.output,
    debugMessage: result.debugMessage?.toString() ?? "",
    decoded: decodeInkResult(result.output),
  };
}

export async function txMessage(
  api: ApiPromise,
  contract: ContractPromise,
  message: string,
  signer: KeyringPair,
  args: unknown[] = [],
  value: bigint | number = 0,
): Promise<ContractTxResult> {
  const dryRun = await queryMessage(
    contract,
    message,
    signer.address,
    args,
    value,
  );
  if (!dryRun.decoded.ok) {
    throw new ContractLogicError(dryRun.decoded.error);
  }

  const resolvedMessage = resolveContractMessageName(contract, message);
  const txFn = (
    contract.tx as Record<
      string,
      (
        options: {
          gasLimit: unknown;
          storageDepositLimit: null;
          value: bigint | number;
        },
        ...args: unknown[]
      ) => {
        signAndSend: SubmittedExtrinsic["events"] extends never ? never : any;
      }
    >
  )[resolvedMessage];

  if (!txFn) {
    throw new Error(`Unknown contract tx message: ${message}`);
  }

  const submitted = await submitExtrinsic(
    api,
    signer,
    txFn(
      {
        gasLimit: normalizeGasLimit(api, dryRun.gasRequired),
        storageDepositLimit: null,
        value,
      },
      ...args,
    ),
  );

  return {
    ...submitted,
    dryRun,
    contractEvents: collectContractEvents(
      api,
      contract,
      contract.address.toString(),
      submitted.events,
    ),
  };
}

export async function instantiateContract(
  api: ApiPromise,
  signer: KeyringPair,
  abi: CodePromise["abi"],
  wasm: string,
  constructorName: string,
  args: unknown[] = [],
  waitFor: ExtrinsicWaitFor = "inBlock",
): Promise<{
  address: string;
  blockHash: string;
  events: SubmittableResult["events"];
}> {
  const code = new CodePromise(api, abi, wasm);
  const constructorTx = (
    code.tx as Record<
      string,
      (
        options: {
          gasLimit: unknown;
          storageDepositLimit: null;
          value: number;
        },
        ...args: unknown[]
      ) => {
        signAndSend: SubmittedExtrinsic["events"] extends never ? never : any;
      }
    >
  )[constructorName];

  if (!constructorTx) {
    throw new Error(`Unknown constructor: ${constructorName}`);
  }

  const submitted = await submitExtrinsic(
    api,
    signer,
    constructorTx(
      {
        gasLimit: createGasLimit(api),
        storageDepositLimit: null,
        value: 0,
      },
      ...args,
    ),
    waitFor,
  );

  const address = findInstantiatedContractAddress(api, submitted.events);

  return {
    address,
    blockHash: submitted.blockHash,
    events: submitted.events,
  };
}

export function findInstantiatedContractAddress(
  api: Pick<ApiPromise, "events">,
  events: SubmittableResult["events"],
): string {
  const instantiatedEvents = events.filter(({ event }) =>
    api.events.contracts.Instantiated.is(event),
  );
  const instantiated = instantiatedEvents.at(-1);

  if (!instantiated) {
    throw new Error(
      "Constructor succeeded but no Instantiated event was found",
    );
  }

  return instantiated.event.data[1].toString();
}
