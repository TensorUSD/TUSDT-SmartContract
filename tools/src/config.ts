import { DEFAULT_WS_URL } from "./constants.js";
import { getEnv } from "./env.js";

export interface ToolsConfig {
  wsUrl: string;
}

export function getToolsConfig(): ToolsConfig {
  return {
    wsUrl: getEnv("WS_URL", DEFAULT_WS_URL),
  };
}
