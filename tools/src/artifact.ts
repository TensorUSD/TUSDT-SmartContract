import { Abi } from "@polkadot/api-contract";
import { readFileSync } from "node:fs";

import { CONTRACT_CONFIGS, type ContractName } from "./constants.js";

export interface ContractArtifact {
  name: ContractName;
  abi: Abi;
  wasm: string;
  sourceHash: string;
  artifactPath: string;
  manifestPath: string;
  raw: Record<string, unknown>;
}

export function loadContractArtifact(name: ContractName): ContractArtifact {
  const config = CONTRACT_CONFIGS[name];
  const raw = JSON.parse(readFileSync(config.artifactPath, "utf8")) as Record<
    string,
    unknown
  >;
  const source = raw.source as { wasm?: string; hash?: string } | undefined;
  const wasm = source?.wasm;
  const sourceHash = source?.hash;

  if (!wasm) {
    throw new Error(
      `Artifact at ${config.artifactPath} does not include embedded wasm`,
    );
  }

  if (!sourceHash) {
    throw new Error(
      `Artifact at ${config.artifactPath} does not include a source hash`,
    );
  }

  return {
    name,
    abi: new Abi(raw),
    wasm,
    sourceHash,
    artifactPath: config.artifactPath,
    manifestPath: config.manifestPath,
    raw,
  };
}

export function loadAllContractArtifacts(): ContractArtifact[] {
  return Object.keys(CONTRACT_CONFIGS).map((name) =>
    loadContractArtifact(name as ContractName),
  );
}
