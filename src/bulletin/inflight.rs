//! Cross-run safety for signed Bulletin store transactions.
//!
//! Two pieces, both keyed by `(chain genesis, account)` under `~/.dotkit`:
//!
//! - [`AccountLock`] — an OS advisory lock, so two local dotkit processes never
//!   sign with the same account at once. The OS drops it when the process exits,
//!   including on a crash, so it can never go stale.
//! - [`Ledger`] — a record of the nonces this machine signed and the block from
//!   which none of them can still be included. Releasing the lock does not
//!   resolve those transactions: they can sit in a node's pool after dotkit
//!   exits. The record is written *before* submission, and the account is not
//!   reused until the chain shows the nonces consumed or their era expired.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};

/// File-name stem shared by an account's lock and ledger on one chain.
pub fn stem(genesis: &[u8; 32], account: &str) -> String {
    format!("{}-{account}", hex::encode(&genesis[..8]))
}

/// An exclusive per-account lock, held until dropped or the process exits.
pub struct AccountLock {
    _file: File,
}

impl AccountLock {
    /// Take the lock for `stem`, or `None` when another process holds it. The
    /// lock file itself is never removed: deleting it would race a concurrent
    /// acquirer that already opened it.
    pub fn try_acquire(dir: &Path, stem: &str) -> Result<Option<Self>> {
        let locks = dir.join("locks");
        std::fs::create_dir_all(&locks).with_context(|| format!("creating {}", locks.display()))?;
        let path = locks.join(format!("{stem}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => {
                Err(e).with_context(|| format!("locking {}", path.display()))
            }
        }
    }
}

/// Store transactions signed for one account that may still be included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InflightRecord {
    pub account: String,
    pub first_nonce: u64,
    pub last_nonce: u64,
    /// First block number at which none of the signed transactions can be
    /// included any more (`era birth + period`, the latest across rounds).
    pub valid_until: u64,
}

impl InflightRecord {
    /// Whether every recorded transaction is resolved: each nonce is consumed
    /// on the finalized chain, or every block that could still include one is
    /// finalized. Either way, signing these nonces again cannot collide.
    pub fn settled(&self, finalized_block: u64, finalized_nonce: u64) -> bool {
        finalized_nonce > self.last_nonce || finalized_block >= self.valid_until
    }

    fn merge(&mut self, first_nonce: u64, last_nonce: u64, valid_until: u64) {
        self.first_nonce = self.first_nonce.min(first_nonce);
        self.last_nonce = self.last_nonce.max(last_nonce);
        self.valid_until = self.valid_until.max(valid_until);
    }
}

/// The on-disk in-flight record for one account. Only touch it while holding
/// the account's [`AccountLock`].
pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    pub fn new(dir: &Path, stem: &str) -> Self {
        Self {
            path: dir.join("inflight").join(format!("{stem}.json")),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current record. A file that cannot be parsed is an error, not an
    /// empty ledger: guessing would risk signing over live nonces.
    pub fn load(&self) -> Result<Option<InflightRecord>> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.path.display())),
        };
        let record = serde_json::from_str(&raw).with_context(|| {
            format!(
                "parsing in-flight record {} (delete it only once its transactions are \
                 included or expired)",
                self.path.display()
            )
        })?;
        Ok(Some(record))
    }

    /// Add a signed nonce range to the record, writing it atomically.
    pub fn record(
        &self,
        account: &str,
        first_nonce: u64,
        last_nonce: u64,
        valid_until: u64,
    ) -> Result<()> {
        let record = match self.load()? {
            Some(mut existing) => {
                existing.merge(first_nonce, last_nonce, valid_until);
                existing
            }
            None => InflightRecord {
                account: account.to_string(),
                first_nonce,
                last_nonce,
                valid_until,
            },
        };
        let dir = self
            .path
            .parent()
            .context("in-flight record has no parent")?;
        if !dir.is_dir() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            if let Some(parent) = dir.parent() {
                sync_dir(parent)?;
            }
        }
        // Write a temp file, flush it to disk, rename it over the record, then
        // flush the directory, so a power loss leaves the old or the new
        // record and never loses one that was already acknowledged.
        let tmp = self.path.with_extension("json.tmp");
        let mut file = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(&serde_json::to_vec_pretty(&record)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("flushing {}", tmp.display()))?;
        drop(file);
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("replacing {}", self.path.display()))?;
        sync_dir(dir)
    }

    /// Remove the record. Only call once [`InflightRecord::settled`] holds.
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing {}", self.path.display())),
        }
    }
}

/// Flush a directory entry change (create, rename) to disk.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("flushing directory {}", dir.display()))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dotkit-inflight-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stem_scopes_by_chain_and_account() {
        let a = stem(&[0xab; 32], "5Account");
        let b = stem(&[0xcd; 32], "5Account");
        assert_eq!(a, "abababababababab-5Account");
        assert_ne!(a, b);
    }

    #[test]
    fn lock_is_exclusive_until_dropped() {
        let dir = temp_dir("lock");
        let first = AccountLock::try_acquire(&dir, "chain-acct").unwrap();
        assert!(first.is_some());
        assert!(AccountLock::try_acquire(&dir, "chain-acct")
            .unwrap()
            .is_none());
        assert!(AccountLock::try_acquire(&dir, "chain-other")
            .unwrap()
            .is_some());
        drop(first);
        assert!(AccountLock::try_acquire(&dir, "chain-acct")
            .unwrap()
            .is_some());
    }

    #[test]
    fn record_is_settled_by_nonce_or_era_expiry() {
        let record = InflightRecord {
            account: "5Account".into(),
            first_nonce: 10,
            last_nonce: 14,
            valid_until: 1_064,
        };
        // Nonce 14 still unconsumed and block 1_063 could still include it.
        assert!(!record.settled(1_063, 14));
        // Every recorded nonce consumed on the finalized chain.
        assert!(record.settled(1_000, 15));
        // Every block that could include one is finalized.
        assert!(record.settled(1_064, 12));
    }

    #[test]
    fn ledger_merges_rounds_and_survives_reload() {
        let dir = temp_dir("ledger");
        let ledger = Ledger::new(&dir, "chain-acct");
        assert_eq!(ledger.load().unwrap(), None);

        ledger.record("5Account", 10, 19, 1_064).unwrap();
        ledger.record("5Account", 15, 24, 1_130).unwrap();
        let reloaded = Ledger::new(&dir, "chain-acct").load().unwrap().unwrap();
        assert_eq!(
            reloaded,
            InflightRecord {
                account: "5Account".into(),
                first_nonce: 10,
                last_nonce: 24,
                valid_until: 1_130,
            }
        );

        ledger.clear().unwrap();
        assert_eq!(ledger.load().unwrap(), None);
        ledger.clear().unwrap();
    }

    #[test]
    fn corrupt_ledger_fails_closed() {
        let dir = temp_dir("corrupt");
        let ledger = Ledger::new(&dir, "chain-acct");
        std::fs::create_dir_all(ledger.path().parent().unwrap()).unwrap();
        std::fs::write(ledger.path(), b"{not json").unwrap();
        assert!(ledger.load().is_err());
        // A failed load never removes the record.
        assert!(ledger.path().exists());
        assert!(ledger.load().is_err());
    }
}
