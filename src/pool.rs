//! Per-machine private Bulletin upload pool keystore (`~/.dotkit/pool.toml`).
//!
//! A locally-generated BIP39 mnemonic whose `//deploy/{0..N}` sub-accounts form
//! a private Bulletin upload pool — isolated from the shared `DEV_PHRASE` pool so
//! uploads don't contend on nonces/quota with everyone else. **Testnet only**:
//! the mnemonic is stored in plaintext and holds no mainnet value.

use anyhow::{bail, Context, Result};
use rand::seq::SliceRandom;
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use subxt::utils::AccountId32;
use subxt_signer::sr25519::Keypair;

use crate::chain;
use crate::ui;

/// Default number of derived pool accounts, matching the shared pool's `0..=9`.
pub const DEFAULT_ACCOUNTS: u32 = 10;

const DIR_NAME: &str = ".dotkit";
const FILE_NAME: &str = "pool.toml";

const HEADER: &str = "\
# dotkit private Bulletin upload pool — TESTNET ONLY.
# Plaintext BIP39 mnemonic for a per-machine pool (no mainnet value).
# Regenerate with `dotkit bulletin pool init --force`.
";

/// Persisted keystore contents.
#[derive(Debug, Serialize, Deserialize)]
pub struct Pool {
    /// BIP39 mnemonic for the private pool root. Testnet-only, low value.
    pub mnemonic: String,
    /// Number of `//deploy/{0..accounts-1}` sub-accounts.
    pub accounts: u32,
    /// Unix creation timestamp (informational).
    pub created_unix: u64,
}

/// `~/.dotkit/pool.toml`.
pub fn keystore_path() -> Result<PathBuf> {
    Ok(dotkit_dir()?.join(FILE_NAME))
}

/// Load the keystore, or `None` when it doesn't exist yet.
pub fn load() -> Result<Option<Pool>> {
    let path = keystore_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading pool keystore {}", path.display()))?;
    let pool = toml::from_str(&raw)
        .with_context(|| format!("parsing pool keystore {}", path.display()))?;
    Ok(Some(pool))
}

/// Generate a fresh pool with a new random 12-word mnemonic.
pub fn generate(accounts: u32) -> Result<Pool> {
    let mut entropy = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut entropy);
    let mnemonic = subxt_signer::bip39::Mnemonic::from_entropy(&entropy)
        .context("generating BIP39 mnemonic")?
        .to_string();
    Ok(Pool {
        mnemonic,
        accounts,
        created_unix: now_unix(),
    })
}

/// Persist the keystore to `~/.dotkit/pool.toml`, creating the dir if needed and
/// locking file perms to `0600` (dir `0700`) so the mnemonic isn't world-readable.
pub fn save(pool: &Pool) -> Result<PathBuf> {
    let path = keystore_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        set_mode(dir, 0o700)?;
    }
    let body = format!(
        "{HEADER}{}",
        toml::to_string_pretty(pool).context("serializing keystore")?
    );
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    set_mode(&path, 0o600)?;
    Ok(path)
}

/// Derive the `//deploy/{index}` signer for a stored pool mnemonic.
pub fn pool_keypair(mnemonic: &str, index: u32) -> Result<Keypair> {
    chain::build_signer(Some(mnemonic), Some(&format!("//deploy/{index}")))
}

/// Every derived `(index, account)` pair for a pool.
pub fn accounts(pool: &Pool) -> Result<Vec<(u32, AccountId32)>> {
    (0..pool.accounts)
        .map(|i| Ok((i, chain::account_id(&pool_keypair(&pool.mnemonic, i)?))))
        .collect()
}

/// The `(label, accounts)` a `--pool` selection resolves to, for inspection:
/// the private keystore's `//deploy/N` unless `--pool shared` was passed.
pub fn accounts_for(source: PoolSource) -> Result<(&'static str, Vec<(u32, AccountId32)>)> {
    match resolve_kind(source, keystore_exists())? {
        PoolKind::Private => Ok(("private", accounts(&load_required()?)?)),
        PoolKind::Shared => {
            let accts = (0u32..SHARED_ACCOUNTS)
                .map(|n| Ok((n, chain::account_id(&shared_keypair(n)?))))
                .collect::<Result<Vec<_>>>()?;
            Ok(("shared", accts))
        }
    }
}

/// Which Bulletin upload pool a command should sign with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolSource {
    /// The default: the local private pool. Errors when no keystore exists; it
    /// never falls back to the shared pool.
    Auto,
    /// Force the local private pool (error if no keystore).
    Local,
    /// The shared `DEV_PHRASE//deploy/N` test pool, opted into explicitly.
    Shared,
}

/// The pool a [`PoolSource`] resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolKind {
    Private,
    Shared,
}

/// Number of accounts in the shared `DEV_PHRASE//deploy/{0..9}` pool.
const SHARED_ACCOUNTS: u32 = 10;

/// Resolve `source` given whether a private keystore exists. Only an explicit
/// `--pool shared` selects the shared pool.
pub fn resolve_kind(source: PoolSource, keystore_exists: bool) -> Result<PoolKind> {
    match source {
        PoolSource::Shared => Ok(PoolKind::Shared),
        PoolSource::Local | PoolSource::Auto if keystore_exists => Ok(PoolKind::Private),
        PoolSource::Local => bail!(
            "--pool local requested but no keystore at {} — run `dotkit bulletin pool init` first",
            keystore_display()
        ),
        PoolSource::Auto => bail!(
            "no private upload pool at {}. Create and authorize one with \
             `dotkit bulletin pool init`, or pass `--pool shared` to upload with the public \
             shared test pool (other people sign with the same accounts)",
            keystore_display()
        ),
    }
}

fn keystore_exists() -> bool {
    keystore_path().map(|p| p.exists()).unwrap_or(false)
}

fn keystore_display() -> String {
    keystore_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| format!("~/{DIR_NAME}/{FILE_NAME}"))
}

fn load_required() -> Result<Pool> {
    load()?.with_context(|| {
        format!(
            "no pool keystore at {} — run `dotkit bulletin pool init` first",
            keystore_display()
        )
    })
}

fn shared_keypair(index: u32) -> Result<Keypair> {
    chain::build_signer(None, Some(&format!("//deploy/{index}")))
}

/// `~/.dotkit`, where the keystore, account locks and in-flight records live.
pub fn dotkit_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME env var not set")?;
    Ok(PathBuf::from(home).join(DIR_NAME))
}

/// Every account a store upload may sign with for `source`, labelled for
/// diagnostics, in random order so concurrent uploads spread across the pool.
/// Selecting the shared pool prints a warning: it is a test opt-in.
pub fn upload_candidates(source: PoolSource) -> Result<(PoolKind, Vec<(String, Keypair)>)> {
    let kind = resolve_kind(source, keystore_exists())?;
    let mut candidates = match kind {
        PoolKind::Private => {
            let pool = load_required()?;
            (0..pool.accounts)
                .map(|n| {
                    Ok((
                        format!("private //deploy/{n}"),
                        pool_keypair(&pool.mnemonic, n)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?
        }
        PoolKind::Shared => {
            ui::warn(
                "--pool shared: uploading with the public shared test pool. Other people \
                 sign with these accounts, so transactions can collide or stall. Use it for \
                 tests only; `dotkit bulletin pool init` creates a private pool.",
            );
            (0..SHARED_ACCOUNTS)
                .map(|n| Ok((format!("shared //deploy/{n}"), shared_keypair(n)?)))
                .collect::<Result<Vec<_>>>()?
        }
    };
    candidates.shuffle(&mut rand::thread_rng());
    Ok((kind, candidates))
}

/// A random account of the pool `source` resolves to, for read-only display
/// (`bulletin status` without `--address`). Uploads use [`upload_candidates`].
pub fn pool_signer(source: PoolSource) -> Result<Keypair> {
    match resolve_kind(source, keystore_exists())? {
        PoolKind::Private => {
            let pool = load_required()?;
            let n = rand::thread_rng().gen_range(0..pool.accounts);
            pool_keypair(&pool.mnemonic, n)
        }
        PoolKind::Shared => chain::shared_pool_signer(),
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_selection_requires_a_private_pool() {
        assert_eq!(
            resolve_kind(PoolSource::Auto, true).unwrap(),
            PoolKind::Private
        );
        let err = resolve_kind(PoolSource::Auto, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("bulletin pool init"), "{err}");
        assert!(err.contains("--pool shared"), "{err}");
    }

    #[test]
    fn shared_pool_is_only_an_explicit_opt_in() {
        assert_eq!(
            resolve_kind(PoolSource::Shared, true).unwrap(),
            PoolKind::Shared
        );
        assert_eq!(
            resolve_kind(PoolSource::Shared, false).unwrap(),
            PoolKind::Shared
        );
        assert_eq!(
            resolve_kind(PoolSource::Local, true).unwrap(),
            PoolKind::Private
        );
        assert!(resolve_kind(PoolSource::Local, false).is_err());
    }
}
