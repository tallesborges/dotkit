//! Choosing and holding the account that signs Bulletin store transactions.
//!
//! An account is usable only when no other local dotkit process holds it, no
//! earlier run left signed transactions that can still be included, and (for
//! pool accounts) its Bulletin authorization outlives one transaction era.
//! There is no fallback: when nothing qualifies, the command stops and says why.

use crate::bulletin::inflight::{self, AccountLock, InflightRecord, Ledger};
use crate::bulletin::storage::{self, ERA_PERIOD, RPC_TIMEOUT};
use crate::chain::config::BulletinConfig;
use crate::chain::signer::account_id;
use crate::env::Env;
use crate::pool::{self, PoolKind, PoolSource};
use crate::ui;
use anyhow::{bail, Context, Result};
use std::path::Path;
use subxt::utils::AccountId32;
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

/// The account a store upload signs with. Holds the account's local lock for
/// as long as it lives.
pub struct UploadSigner {
    keypair: Keypair,
    account: AccountId32,
    label: String,
    ledger: Ledger,
    _lock: AccountLock,
}

impl UploadSigner {
    pub fn keypair(&self) -> &Keypair {
        &self.keypair
    }

    pub fn account(&self) -> &AccountId32 {
        &self.account
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn ledger_path(&self) -> &Path {
        self.ledger.path()
    }

    /// Record signed nonces before they are submitted, so a crash or a stall
    /// cannot lead a later run to sign over them.
    pub(crate) fn record_inflight(
        &self,
        first_nonce: u64,
        last_nonce: u64,
        valid_until: u64,
    ) -> Result<()> {
        self.ledger.record(
            &self.account.to_string(),
            first_nonce,
            last_nonce,
            valid_until,
        )
    }

    /// Drop the in-flight record once the finalized chain shows it settled.
    /// Call after the upload is confirmed; a record that is not settled yet
    /// stays and is checked again on the next run.
    pub async fn settle(&self, client: &OnlineClient<BulletinConfig>) -> Result<()> {
        let Some(record) = self.ledger.load()? else {
            return Ok(());
        };
        let (finalized, nonce) = finalized_nonce(client, &self.account).await?;
        if record.settled(finalized, nonce) {
            self.ledger.clear()?;
        }
        Ok(())
    }
}

/// Why an account cannot sign this upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unusable {
    InUse,
    Inflight {
        record: InflightRecord,
        finalized: u64,
    },
    LedgerUnreadable {
        path: String,
        error: String,
    },
    NotAuthorized,
    AuthExpired {
        at: u32,
    },
    AuthExpiresSoon {
        at: u32,
    },
}

/// Whether an authorization expiring at `expiration` covers an upload starting
/// at `finalized`. It must outlive one era, the longest a signed store stays
/// valid.
pub(crate) fn auth_problem(expiration: Option<u32>, finalized: u64) -> Option<Unusable> {
    match expiration {
        None => Some(Unusable::NotAuthorized),
        Some(at) if u64::from(at) <= finalized => Some(Unusable::AuthExpired { at }),
        Some(at) if u64::from(at) <= finalized + ERA_PERIOD => {
            Some(Unusable::AuthExpiresSoon { at })
        }
        Some(_) => None,
    }
}

/// The error shown when no candidate account is usable. `renew` is the command
/// that renews authorizations for this signer source, if dotkit has one.
pub(crate) fn unusable_message(
    source: &str,
    renew: Option<&str>,
    rows: &[(String, AccountId32, Unusable)],
) -> String {
    let mut msg = format!("no usable Bulletin upload account in the {source}:");
    for (label, account, why) in rows {
        let why = match why {
            Unusable::InUse => "in use by another local dotkit process".to_string(),
            Unusable::Inflight { record, finalized } => format!(
                "signed store transactions from an earlier run (nonces {}..={}) can still be \
                 included until block #{} (finalized #{finalized})",
                record.first_nonce, record.last_nonce, record.valid_until
            ),
            Unusable::LedgerUnreadable { path, error } => format!(
                "its in-flight record {path} cannot be read ({error}); it is kept. Remove it \
                 by hand only once that account's last upload is more than {ERA_PERIOD} \
                 finalized blocks old"
            ),
            Unusable::NotAuthorized => "not authorized for Bulletin storage".to_string(),
            Unusable::AuthExpired { at } => format!("authorization expired at block #{at}"),
            Unusable::AuthExpiresSoon { at } => format!(
                "authorization expires at block #{at}, within one {ERA_PERIOD}-block \
                 transaction era"
            ),
        };
        msg.push_str(&format!("\n  {label} ({account}): {why}"));
    }
    let auth = rows.iter().any(|(_, _, why)| {
        matches!(
            why,
            Unusable::NotAuthorized
                | Unusable::AuthExpired { .. }
                | Unusable::AuthExpiresSoon { .. }
        )
    });
    if auth {
        match renew {
            Some(cmd) => msg.push_str(&format!("\nRenew the authorizations with `{cmd}`.")),
            None => msg.push_str("\nAn Authorizer must renew these authorizations."),
        }
    }
    if rows.iter().any(|(_, _, why)| *why == Unusable::InUse) {
        msg.push_str("\nWait for the other dotkit process to finish.");
    }
    if rows
        .iter()
        .any(|(_, _, why)| matches!(why, Unusable::Inflight { .. }))
    {
        msg.push_str(
            "\nWait until the listed block is finalized, then rerun: stored blocks are skipped \
             and the account is reused once its transactions are included or expired.",
        );
    }
    msg
}

async fn finalized_nonce(
    client: &OnlineClient<BulletinConfig>,
    account: &AccountId32,
) -> Result<(u64, u64)> {
    let at = tokio::time::timeout(RPC_TIMEOUT, client.at_current_block())
        .await
        .context("timed out reading the finalized Bulletin block")??;
    let nonce = tokio::time::timeout(RPC_TIMEOUT, at.transactions().account_nonce(account))
        .await
        .context("timed out reading the account nonce")?
        .context("reading the account nonce")?;
    Ok((at.block_number(), nonce))
}

/// Take the first usable account of `candidates`, in order.
async fn acquire(
    client: &OnlineClient<BulletinConfig>,
    source: &str,
    renew: Option<&str>,
    candidates: Vec<(String, Keypair)>,
    require_authorization: bool,
) -> Result<UploadSigner> {
    let dir = pool::dotkit_dir()?;
    let genesis = client.genesis_hash().0;
    let at = tokio::time::timeout(RPC_TIMEOUT, client.at_current_block())
        .await
        .context("timed out reading the finalized Bulletin block")??;
    let finalized = at.block_number();

    let mut rows = Vec::new();
    for (label, keypair) in candidates {
        let account = account_id(&keypair);
        let stem = inflight::stem(&genesis, &account.to_string());
        let Some(lock) = AccountLock::try_acquire(&dir, &stem)? else {
            rows.push((label, account, Unusable::InUse));
            continue;
        };

        let ledger = Ledger::new(&dir, &stem);
        let record = match ledger.load() {
            Ok(record) => record,
            Err(err) => {
                // Fail closed for this account only, and keep the file: it
                // may describe transactions that can still land.
                rows.push((
                    label,
                    account,
                    Unusable::LedgerUnreadable {
                        path: ledger.path().display().to_string(),
                        error: format!("{err:#}"),
                    },
                ));
                continue;
            }
        };
        if let Some(record) = record {
            let nonce =
                tokio::time::timeout(RPC_TIMEOUT, at.transactions().account_nonce(&account))
                    .await
                    .context("timed out reading the account nonce")?
                    .context("reading the account nonce")?;
            if record.settled(finalized, nonce) {
                ledger.clear()?;
            } else {
                rows.push((label, account, Unusable::Inflight { record, finalized }));
                continue;
            }
        }

        if require_authorization {
            let expiration = storage::authorization_at(&at, &account)
                .await?
                .map(|auth| auth.expiration);
            if let Some(problem) = auth_problem(expiration, finalized) {
                rows.push((label, account, problem));
                continue;
            }
        }

        ui::note(format!("signer: {label} ({account})"));
        return Ok(UploadSigner {
            keypair,
            account,
            label,
            ledger,
            _lock: lock,
        });
    }
    bail!("{}", unusable_message(source, renew, &rows))
}

/// A Bulletin upload signer from the pool `source` resolves to.
pub async fn acquire_pool_signer(
    client: &OnlineClient<BulletinConfig>,
    env: &Env,
    source: PoolSource,
) -> Result<UploadSigner> {
    let (kind, candidates) = pool::upload_candidates(source)?;
    match kind {
        PoolKind::Private => {
            let renew = format!("dotkit bulletin pool authorize --env {}", env.id);
            acquire(client, "private pool", Some(&renew), candidates, true).await
        }
        PoolKind::Shared => acquire(client, "shared pool", None, candidates, true).await,
    }
}

/// An explicitly supplied upload signer (`--mnemonic`). It gets the same lock
/// and in-flight checks; its authorization is not pre-checked.
pub async fn acquire_explicit_signer(
    client: &OnlineClient<BulletinConfig>,
    keypair: Keypair,
) -> Result<UploadSigner> {
    acquire(
        client,
        "--mnemonic signer",
        None,
        vec![("--mnemonic".to_string(), keypair)],
        false,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(byte: u8) -> AccountId32 {
        AccountId32([byte; 32])
    }

    #[test]
    fn authorization_must_outlive_one_era() {
        assert_eq!(auth_problem(None, 1_000), Some(Unusable::NotAuthorized));
        assert_eq!(
            auth_problem(Some(1_000), 1_000),
            Some(Unusable::AuthExpired { at: 1_000 })
        );
        assert_eq!(
            auth_problem(Some(1_000 + ERA_PERIOD as u32), 1_000),
            Some(Unusable::AuthExpiresSoon {
                at: 1_000 + ERA_PERIOD as u32
            })
        );
        assert_eq!(auth_problem(Some(1_001 + ERA_PERIOD as u32), 1_000), None);
    }

    #[test]
    fn expired_private_pool_points_at_renewal_not_the_shared_pool() {
        let rows = vec![
            (
                "private //deploy/0".to_string(),
                account(1),
                Unusable::AuthExpired { at: 900 },
            ),
            (
                "private //deploy/1".to_string(),
                account(2),
                Unusable::NotAuthorized,
            ),
        ];
        let msg = unusable_message(
            "private pool",
            Some("dotkit bulletin pool authorize --env paseo-next-v2"),
            &rows,
        );
        assert!(msg.contains("authorization expired at block #900"), "{msg}");
        assert!(
            msg.contains("`dotkit bulletin pool authorize --env paseo-next-v2`"),
            "{msg}"
        );
        assert!(!msg.contains("shared"), "{msg}");
    }

    #[test]
    fn unreadable_ledger_blocks_only_its_account_and_is_kept() {
        let rows = vec![(
            "private //deploy/2".to_string(),
            account(5),
            Unusable::LedgerUnreadable {
                path: "/tmp/inflight/x.json".into(),
                error: "expected value".into(),
            },
        )];
        let msg = unusable_message("private pool", Some("renew"), &rows);
        assert!(msg.contains("/tmp/inflight/x.json cannot be read"), "{msg}");
        assert!(msg.contains("it is kept"), "{msg}");
        assert!(!msg.contains("Renew"), "{msg}");
    }

    #[test]
    fn inflight_and_locked_accounts_explain_the_wait() {
        let record = InflightRecord {
            account: account(3).to_string(),
            first_nonce: 7,
            last_nonce: 9,
            valid_until: 2_064,
        };
        let rows = vec![
            ("a".to_string(), account(3), Unusable::InUse),
            (
                "b".to_string(),
                account(4),
                Unusable::Inflight {
                    record,
                    finalized: 2_010,
                },
            ),
        ];
        let msg = unusable_message("private pool", Some("renew"), &rows);
        assert!(msg.contains("nonces 7..=9"), "{msg}");
        assert!(msg.contains("until block #2064 (finalized #2010)"), "{msg}");
        assert!(msg.contains("Wait for the other dotkit process"), "{msg}");
        assert!(!msg.contains("Renew"), "{msg}");
    }
}
