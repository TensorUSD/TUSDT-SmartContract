import { RATIO_SCALE } from "./constants.js";

export interface DecodedInkResult {
  ok: boolean;
  value?: unknown;
  error?: unknown;
}

export function toPrimitive(value: unknown): unknown {
  if (
    value !== null &&
    typeof value === "object" &&
    "toPrimitive" in value &&
    typeof (value as { toPrimitive?: unknown }).toPrimitive === "function"
  ) {
    return (value as { toPrimitive: () => unknown }).toPrimitive();
  }

  return value;
}

export function decodeInkResult(value: unknown): DecodedInkResult {
  const primitive = toPrimitive(value);

  if (primitive && typeof primitive === "object" && !Array.isArray(primitive)) {
    const record = primitive as Record<string, unknown>;

    if ("Ok" in record) {
      return decodeInkResult(record.Ok);
    }

    if ("ok" in record) {
      return decodeInkResult(record.ok);
    }

    if ("Err" in record) {
      return { ok: false, error: record.Err };
    }

    if ("err" in record) {
      return { ok: false, error: record.err };
    }
  }

  return { ok: true, value: primitive };
}

export function stringifyJson(value: unknown): string {
  return JSON.stringify(
    value,
    (_key, innerValue) =>
      typeof innerValue === "bigint" ? innerValue.toString() : innerValue,
    2,
  );
}

export function formatInkError(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }

  if (error instanceof Error) {
    return error.message;
  }

  return stringifyJson(error);
}

export function parseIntegerArg(value: string, label: string): bigint {
  try {
    return BigInt(value);
  } catch (error) {
    throw new Error(`Invalid ${label}: ${value}`, { cause: error });
  }
}

export function ratioFromInteger(value: bigint | number | string): [bigint] {
  return [BigInt(value) * RATIO_SCALE];
}

export function extractRatioInner(value: unknown): bigint | null {
  const primitive = toPrimitive(value);

  if (primitive === null || primitive === undefined) {
    return null;
  }

  if (typeof primitive === "bigint") {
    return primitive;
  }

  if (typeof primitive === "number") {
    return BigInt(primitive);
  }

  if (typeof primitive === "string") {
    return BigInt(primitive);
  }

  if (Array.isArray(primitive) && primitive.length > 0) {
    return extractRatioInner(primitive[0]);
  }

  if (typeof primitive === "object") {
    const record = primitive as Record<string, unknown>;

    if ("0" in record) {
      return extractRatioInner(record["0"]);
    }
  }

  return null;
}

export function ratioInnerToDisplay(value: unknown): string | null {
  const inner = extractRatioInner(value);
  if (inner === null) {
    return null;
  }

  if (inner % RATIO_SCALE === 0n) {
    return (inner / RATIO_SCALE).toString();
  }

  return `${inner.toString()} / ${RATIO_SCALE.toString()}`;
}

export function pickProperty<T = unknown>(
  value: unknown,
  ...keys: string[]
): T | undefined {
  const primitive = toPrimitive(value);
  if (!primitive || typeof primitive !== "object" || Array.isArray(primitive)) {
    return undefined;
  }

  const record = primitive as Record<string, unknown>;
  for (const key of keys) {
    if (key in record) {
      return record[key] as T;
    }
  }

  return undefined;
}

export function toNumber(value: unknown): number {
  const primitive = toPrimitive(value);
  if (typeof primitive === "number") {
    return primitive;
  }
  if (typeof primitive === "string") {
    return Number(primitive);
  }
  if (typeof primitive === "bigint") {
    return Number(primitive);
  }

  throw new Error(`Unable to coerce value to number: ${stringifyJson(primitive)}`);
}
