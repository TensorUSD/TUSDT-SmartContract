import type { ApiPromise } from "@polkadot/api";
import type { KeyringPair } from "@polkadot/keyring/types";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { getDevAccounts } from "../src/accounts.js";
import { createApi } from "../src/api.js";
import { type DecodedContractEvent } from "../src/contract.js";
import {
  commitRound,
  deployOracle,
  setValidator,
} from "../src/interactions/oracle.js";
import {
  borrowTusdt,
  coverDeficit,
  deployLendingPool,
  deployTusdtAndGetCodeHash,
  depositAlpha,
  erc20Approve,
  erc20Mint,
  expectOk,
  expectOption,
  liquidate,
  queryMarketDeficit,
  queryMarketState,
  queryPosition,
  queryUserDebt,
  setApprovedNetuid,
  supplyTusdt,
} from "../src/interactions/lending.js";
import {
  devAddStake,
  devAssociateHotkey,
  devForceSetBalance,
  devGetStake,
  devSudoSetSubtokenEnabled,
} from "../src/interactions/subtensor-dev.js";

const ROOT_NETUID = 0;
const DEBT_MARKET_TUSDT = 1;
const ALPHA_MARKET_ID = 2; // first approved netuid gets market id 2

const TAO = 10n ** 9n;
const COLLATERAL_ALPHA = 1_400n * TAO; // the user's reproduction: 1,400 alpha
const BORROW_TUSDT = 100_000n * TAO;

/**
 * End-to-end validation of the full-position-seizure liquidation against a
 * real node: `liquidate(borrower)` pays the borrower's ENTIRE debt on both
 * markets (with accrued interest) and seizes ALL alpha collateral across
 * every netuid, minus the platform's liquidation fee (per-netuid
 * `liquidation_fee`, default 5% of the collateral alpha, capped at the
 * surplus share so the liquidator never loses principal). When the collateral
 * cannot cover the debt, the liquidator pays the collateral value
 * (break-even), the platform gets nothing, and the residual is written off as
 * a frozen market deficit.
 *
 * The collateral subnet is ROOT (netuid 0): its alpha price is always 1.0
 * (swap-pallet root invariance), so no subnet registration is needed — the
 * only runtime prerequisites are hotkey Owner entries, root subtokens enabled,
 * and balances, all set up in `beforeAll`.
 *
 * Scenario B (borrower alice) exercises the UNDERWATER + DEFICIT path: at
 * oracle 60 her collateral is worth 1,400 × 60 = 84,000 TUSDT against a
 * ~100,000 TUSDT debt (health factor ~0.50). The liquidator pays the
 * collateral value (84,000 TUSDT, break-even), receives ALL 1,400 alpha (no
 * platform cut — there is no surplus), and the residual debt is written off
 * as a frozen market deficit.
 *
 * Scenario A (borrower charlie) exercises the HEALTHY-SIDE path: at oracle 100
 * her collateral is worth 140,000 TUSDT against a ~100,000 TUSDT debt
 * (health factor ~0.84). The liquidator pays the full debt, receives all
 * 1,400 alpha minus the platform's 5% cut (70 alpha → the platform role
 * account), and no deficit is created.
 */
describe.sequential("tusdt-lending-pool liquidation flow", () => {
  let api: ApiPromise;
  let alice: KeyringPair;
  let bob: KeyringPair;
  let charlie: KeyringPair;
  let aliceHotkey: KeyringPair;
  let charlieHotkey: KeyringPair;
  let poolHotkey: KeyringPair;
  let pool: Awaited<ReturnType<typeof deployLendingPool>>;
  let tusdt: Awaited<ReturnType<typeof deployTusdtAndGetCodeHash>>["tusdt"];
  let oracle: Awaited<ReturnType<typeof deployOracle>>;

  async function commitPrice(priceInteger: bigint): Promise<void> {
    await commitRound(api, oracle, alice, priceInteger);
  }

  function events(tx: { contractEvents: DecodedContractEvent[] }) {
    return tx.contractEvents;
  }

  function findEvent(
    tx: { contractEvents: DecodedContractEvent[] },
    identifier: string,
  ): DecodedContractEvent | undefined {
    return events(tx).find((e) => e.identifier === identifier);
  }

  beforeAll(async () => {
    api = await createApi();
    const accounts = await getDevAccounts();
    alice = accounts.alice;
    bob = accounts.bob;
    charlie = accounts.charlie;
    aliceHotkey = accounts.eve;
    charlieHotkey = accounts.dave;
    poolHotkey = accounts.ferdie;

    // ── Runtime prerequisites (no subnet registration needed — root only) ──
    await devForceSetBalance(api, alice, alice.address, 10_000n * TAO);
    await devForceSetBalance(api, alice, charlie.address, 5_000n * TAO);
    await devForceSetBalance(api, alice, bob.address, 5_000n * TAO);
    // Hotkey Owner entries: the borrowers' hotkeys and the pool's hotkey.
    await devAssociateHotkey(api, alice, aliceHotkey.address);
    await devAssociateHotkey(api, charlie, charlieHotkey.address);
    await devAssociateHotkey(api, alice, poolHotkey.address);
    // Root subtokens must be enabled for any root stake transfer.
    await devSudoSetSubtokenEnabled(api, alice, ROOT_NETUID, true);
    // Borrower root stakes: 1,400 alpha each (root alpha is 1:1 with TAO).
    await devAddStake(api, alice, aliceHotkey.address, ROOT_NETUID, COLLATERAL_ALPHA);
    await devAddStake(api, charlie, charlieHotkey.address, ROOT_NETUID, COLLATERAL_ALPHA);

    // ── Deploy the protocol ──
    const deployed = await deployTusdtAndGetCodeHash(api, alice);
    tusdt = deployed.tusdt;
    oracle = await deployOracle(api, alice, alice.address, alice.address, ROOT_NETUID);
    await setValidator(api, oracle, alice, alice.address);
    await commitPrice(230n * TAO); // TUSDT/TAO = 230 (1e18 ratio)
    pool = await deployLendingPool(
      api,
      alice,
      alice.address,
      tusdt.address.toString(),
      oracle.address.toString(),
      deployed.codeHash,
      poolHotkey.address,
    );
    console.log("Lending Pool Address: ", pool.address.toHuman());
    await setApprovedNetuid(api, pool, alice, ROOT_NETUID, true);

    // ── Fund TUSDT: mint + supply 250k to the pool ──
    await erc20Mint(api, tusdt, alice, alice.address, 350_000n * TAO);
    await erc20Mint(api, tusdt, alice, bob.address, 200_000n * TAO);
    await erc20Approve(api, tusdt, alice, pool.address.toString(), 300_000n * TAO);
    await supplyTusdt(api, pool, alice, 250_000n * TAO);
    // Bob approves the pool so his liquidation repayments can be pulled.
    await erc20Approve(api, tusdt, bob, pool.address.toString(), 200_000n * TAO);

    // ── Both borrowers deposit collateral and borrow 100k TUSDT at $230 ──
    // Capacity at $230: 50% × (1,400 α × 230) = 161,000 TUSDT.
    await depositAlpha(api, pool, alice, ROOT_NETUID, COLLATERAL_ALPHA);
    await depositAlpha(api, pool, charlie, ROOT_NETUID, COLLATERAL_ALPHA);
    await borrowTusdt(api, pool, alice, BORROW_TUSDT);
    await borrowTusdt(api, pool, charlie, BORROW_TUSDT);
  }, 600_000);

  afterAll(async () => {
    await api.disconnect();
  });

  it("borrows at $230 and is healthy", async () => {
    const debt = expectOk<bigint>(
      await queryUserDebt(pool, alice.address, DEBT_MARKET_TUSDT, alice.address),
      "get_user_debt(alice)",
    );
    // The debt exceeds the borrowed amount immediately (prepaid first hour).
    expect(debt).toBeGreaterThan(BORROW_TUSDT);
    expect(debt - BORROW_TUSDT).toBeLessThan(TAO); // a fraction of 1 TUSDT
  });

  describe("scenario B — underwater full seizure + deficit at oracle 60 (alice)", () => {
    let debtBefore: bigint;

    beforeAll(async () => {
      await commitPrice(60n * TAO);
      debtBefore = expectOk<bigint>(
        await queryUserDebt(pool, bob.address, DEBT_MARKET_TUSDT, alice.address),
        "get_user_debt(alice) @60",
      );
    }, 60_000);

    it("seizes the whole underwater position at the collateral value", async () => {
      const tx = await liquidate(api, pool, bob, alice.address);

      const liquidated = findEvent(tx, "Liquidated");
      expect(liquidated, "Liquidated event").toBeDefined();
      const [user, liquidator, netuids, seized, platformAlpha, coveredTao, coveredTusdt, deficit] =
        liquidated!.args as [
          string,
          string,
          number[],
          bigint,
          bigint,
          bigint,
          bigint,
          bigint,
        ];
      expect(user).toBe(alice.address);
      expect(liquidator).toBe(bob.address);
      expect(netuids).toContain(ROOT_NETUID);
      // Underwater: ALL alpha is seized, no platform cut (no surplus).
      expect(seized).toBe(COLLATERAL_ALPHA);
      expect(platformAlpha).toBe(0n);
      // Collateral value = 1,400 α × 60 TUSDT/α = 84,000 TUSDT, paid in full
      // (the borrower has no TAO debt). The residual is the deficit.
      expect(coveredTao).toBe(0n);
      expect(coveredTusdt).toBe(84_000n * TAO);
      expect(deficit).toBeGreaterThan(debtBefore - 84_000n * TAO - TAO);
      expect(deficit).toBeLessThan(debtBefore - 84_000n * TAO + TAO);
    });

    it("books the residual debt as a frozen market deficit", async () => {
      // The position is fully written off — the user debt reads zero and the
      // deficit holds the uncollectible face value (D − C ≈ 16,000 TUSDT).
      const debtAfter = expectOk<bigint>(
        await queryUserDebt(pool, bob.address, DEBT_MARKET_TUSDT, alice.address),
        "get_user_debt(alice) after",
      );
      expect(debtAfter).toBe(0n);

      const deficit = expectOption<bigint>(
        await queryMarketDeficit(pool, bob.address, DEBT_MARKET_TUSDT),
        "get_market_deficit(1)",
      );
      expect(deficit).toBeGreaterThan(debtBefore - 84_000n * TAO - TAO);
      expect(deficit).toBeLessThan(debtBefore - 84_000n * TAO + TAO);
    });

    it("empties the borrower's collateral position", async () => {
      const pos = expectOption<{
        alpha_principal: bigint;
        scaled_debt: bigint;
      }>(
        await queryPosition(pool, bob.address, ALPHA_MARKET_ID, alice.address),
        "get_position(alpha, alice)",
      );
      // Collateral exhausted → the liquidation wrote off the residual debt.
      expect(pos?.alpha_principal ?? 0n).toBe(0n);
    });

    it("lets the maintainer cover the deficit from the reserve", async () => {
      const deficitBefore = expectOption<bigint>(
        await queryMarketDeficit(pool, alice.address, DEBT_MARKET_TUSDT),
        "get_market_deficit(1)",
      );
      expect(deficitBefore).toBeGreaterThan(0n);

      // cover_deficit is maintainer-gated; alice is the maintainer.
      await coverDeficit(api, pool, alice, DEBT_MARKET_TUSDT);

      const deficitAfter = expectOption<bigint>(
        await queryMarketDeficit(pool, alice.address, DEBT_MARKET_TUSDT),
        "get_market_deficit(1) after cover",
      );
      // The reserve (20% of the prepaid-hour premium) funded part of it.
      expect(deficitAfter).toBeLessThan(deficitBefore!);
      // Remember the post-cover remainder for scenario A's no-new-deficit check.
      deficitFromScenarioB = deficitAfter ?? 0n;
    });

    it("pays the liquidator the seized alpha", async () => {
      // Bob's coldkey now holds the pool hotkey's alpha on root.
      const seized = await devGetStake(
        api,
        poolHotkey.address,
        bob.address,
        ROOT_NETUID,
      );
      expect(seized).toBe(COLLATERAL_ALPHA);
    });
  });

  describe("scenario A — healthy-side full seizure at oracle 100 (charlie)", () => {
    let debtBefore: bigint;

    beforeAll(async () => {
      await commitPrice(100n * TAO);
      debtBefore = expectOk<bigint>(
        await queryUserDebt(pool, bob.address, DEBT_MARKET_TUSDT, charlie.address),
        "get_user_debt(charlie) @100",
      );
    }, 60_000);

    it("closes the position completely in one liquidation", async () => {
      const tx = await liquidate(api, pool, bob, charlie.address);

      const liquidated = findEvent(tx, "Liquidated");
      expect(liquidated, "Liquidated event").toBeDefined();
      const [user, liquidator, netuids, seized, platformAlpha, coveredTao, coveredTusdt, deficit] =
        liquidated!.args as [
          string,
          string,
          number[],
          bigint,
          bigint,
          bigint,
          bigint,
          bigint,
        ];
      expect(user).toBe(charlie.address);
      expect(liquidator).toBe(bob.address);
      expect(netuids).toContain(ROOT_NETUID);
      // Healthy-side: the full debt is paid (TUSDT market only) and ALL the
      // collateral is seized; the platform takes 5% of it (70 of 1,400 α).
      expect(coveredTao).toBe(0n);
      expect(coveredTusdt).toBe(debtBefore);
      expect(deficit).toBe(0n);
      expect(seized).toBe(COLLATERAL_ALPHA);
      expect(platformAlpha).toBe((COLLATERAL_ALPHA * 5_000_000_000_000_000n) / 10n ** 18n);
    });

    it("leaves the borrower with zero debt and no new deficit", async () => {
      const debtAfter = expectOk<bigint>(
        await queryUserDebt(pool, bob.address, DEBT_MARKET_TUSDT, charlie.address),
        "get_user_debt(charlie) after",
      );
      expect(debtAfter).toBe(0n);

      const deficit = expectOption<bigint>(
        await queryMarketDeficit(pool, bob.address, DEBT_MARKET_TUSDT),
        "get_market_deficit(1)",
      );
      // Whatever the pre-cover remainder was, this liquidation adds nothing.
      expect(deficit).toBe(deficitFromScenarioB);
    });

    it("empties the borrower's collateral position", async () => {
      const pos = expectOption<{ alpha_principal: bigint }>(
        await queryPosition(pool, bob.address, ALPHA_MARKET_ID, charlie.address),
        "get_position(alpha, charlie)",
      );
      // Full seizure — nothing remains on the borrower.
      expect(pos?.alpha_principal ?? 0n).toBe(0n);
    });

    it("pays the liquidator the seized alpha and the platform its fee", async () => {
      // Bob now holds scenario B's 1,400 α plus scenario A's liquidator share
      // (1,400 − 70 platform = 1,330 α).
      const liquidatorSeized = await devGetStake(
        api,
        poolHotkey.address,
        bob.address,
        ROOT_NETUID,
      );
      expect(liquidatorSeized).toBe(
        COLLATERAL_ALPHA +
          COLLATERAL_ALPHA -
          (COLLATERAL_ALPHA * 5_000_000_000_000_000n) / 10n ** 18n,
      );

      // The platform role account (alice, the deployer) received its 5% cut.
      const platformSeized = await devGetStake(
        api,
        poolHotkey.address,
        alice.address,
        ROOT_NETUID,
      );
      expect(platformSeized).toBe((COLLATERAL_ALPHA * 5_000_000_000_000_000n) / 10n ** 18n);
    });
  });

  // The market's bookkeeping stays consistent after both liquidations.
  it("leaves the market ledger consistent", async () => {
    const state = expectOption<{ total_debt: bigint; total_scaled_debt: bigint }>(
      await queryMarketState(pool, bob.address, DEBT_MARKET_TUSDT),
      "get_market_state(1)",
    );
    // Both borrowers' positions are fully cleared: alice's residual was
    // written off into the deficit, charlie's was fully repaid.
    expect(state?.total_debt ?? 0n).toBe(0n);
    expect(state?.total_scaled_debt ?? 0n).toBe(0n);
  });
});

// Captured across describe blocks (module scope): the deficit amount after
// scenario B's cover_deficit — scenario A must not add to it.
let deficitFromScenarioB = 0n;
