import { ApiPromise, WsProvider } from "@polkadot/api";

import { getToolsConfig } from "./config.js";

export async function createApi(): Promise<ApiPromise> {
  const provider = new WsProvider(getToolsConfig().wsUrl);
  return ApiPromise.create({ provider });
}
