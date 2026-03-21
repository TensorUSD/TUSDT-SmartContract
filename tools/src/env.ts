import dotenv from "dotenv";
import { resolve } from "node:path";

import { TOOLS_ROOT } from "./constants.js";

let loaded = false;

export function loadToolsEnv(): void {
  if (loaded) {
    return;
  }

  dotenv.config({ path: resolve(TOOLS_ROOT, ".env") });
  loaded = true;
}

export function getEnv(name: string, fallback?: string): string {
  loadToolsEnv();

  const value = process.env[name] ?? fallback;
  if (value === undefined || value === "") {
    throw new Error(`Missing required environment variable: ${name}`);
  }

  return value;
}

export function getOptionalEnv(name: string): string | undefined {
  loadToolsEnv();

  const value = process.env[name];
  return value === "" ? undefined : value;
}
