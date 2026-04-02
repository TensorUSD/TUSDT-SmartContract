use super::*;
use ink::codegen::Env as _;

impl TusdtVault {
    pub(crate) fn ensure_not_in_liquidation(&self, owner: AccountId, vault_id: u32) -> Result<()> {
        if self.liquidation_auctions.get((owner, vault_id)).is_some() {
            return Err(Error::VaultInLiquidation);
        }
        Ok(())
    }

    pub(crate) fn load_caller_vault(&self, vault_id: u32) -> Result<(AccountId, Vault)> {
        let caller = self.env().caller();
        let vault = self
            .vaults
            .get((caller, vault_id))
            .ok_or(Error::VaultNotFound)?;
        self.ensure_not_in_liquidation(caller, vault_id)?;
        Ok((caller, vault))
    }

    pub(crate) fn load_vault(&self, owner: AccountId, vault_id: u32) -> Result<Vault> {
        self.vaults
            .get((owner, vault_id))
            .ok_or(Error::VaultNotFound)
    }

    pub(crate) fn save_vault(
        &mut self,
        owner: AccountId,
        vault_id: u32,
        vault: &Vault,
    ) -> Result<()> {
        let previous_vault_debt = self
            .vaults
            .get((owner, vault_id))
            .map(|stored_vault: Vault| stored_vault.borrowed_token_balance)
            .unwrap_or_default();
        self.sync_owner_total_debt(owner, previous_vault_debt, vault.borrowed_token_balance)?;
        self.vaults.insert((owner, vault_id), vault);
        Ok(())
    }
}
