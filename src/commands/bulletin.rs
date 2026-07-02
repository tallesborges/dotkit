use crate::chain::{self, bulletin, BulletinConfig, DEV_PHRASE};
use crate::env::Env;
use crate::ui;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use rand::Rng;
use std::str::FromStr;
use subxt::utils::AccountId32;
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

/// Chain-enforced `MaxTransactionSize` (2 MiB).
const MAX_TRANSACTION_SIZE: usize = 2 * 1024 * 1024;

#[derive(Subcommand)]
pub enum Cmd {
    /// Store a single blob/file on the Bulletin chain.
    Store {
        /// Path to the file to store.
        path: String,
    },
    /// Store every IPLD block of a CARv1 so its root content CID resolves.
    StoreCar {
        /// Path to the `.car` file (CARv1, sha2-256 blocks; e.g. `ipfs dag export`).
        path: String,
    },
    /// Show authorization / quota for an account.
    Status {
        /// SS58 address to inspect (defaults to the signer's account).
        #[arg(long)]
        address: Option<String>,
    },
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    match cmd {
        Cmd::Status { address } => status(env, address, mnemonic, derivation_path).await,
        Cmd::Store { path } => store(env, path, mnemonic, derivation_path).await,
        Cmd::StoreCar { path } => store_car(env, path, mnemonic, derivation_path).await,
    }
}

/// Resolve the write signer: the caller's mnemonic when supplied, otherwise a
/// random authorized account from the shared Bulletin storage pool (the default
/// owner signer has no Bulletin quota — only the pool accounts do).
fn resolve_signer(mnemonic: Option<String>, derivation_path: Option<String>) -> Result<Keypair> {
    match mnemonic {
        Some(phrase) => chain::build_signer(Some(&phrase), derivation_path.as_deref()),
        None => pool_signer(),
    }
}

/// A random authorized account from the shared Bulletin storage pool, spreading
/// load and cutting nonce contention across concurrent deploys.
pub fn pool_signer() -> Result<Keypair> {
    let n = rand::thread_rng().gen_range(0u32..=9);
    chain::build_signer(Some(DEV_PHRASE), Some(&format!("//deploy/{n}")))
}

async fn status(
    env: &Env,
    address: Option<String>,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let account = match address {
        Some(addr) => AccountId32::from_str(&addr)
            .map_err(|e| anyhow::anyhow!("invalid SS58 address: {e}"))?,
        None => chain::account_id(&resolve_signer(mnemonic, derivation_path)?),
    };

    let client = chain::bulletin_client(env).await?;

    let scope = bulletin::runtime_types::pallet_bulletin_transaction_storage::types::AuthorizationScope::Account(account);
    let address = bulletin::storage().transaction_storage().authorizations();
    let at = client.at_current_block().await?;
    let authorization = at
        .storage()
        .try_fetch(address, (scope,))
        .await
        .context("reading TransactionStorage.Authorizations")?;

    ui::kv("address", account);
    match authorization {
        Some(value) => {
            let auth = value.decode().context("decoding Authorization")?;
            let e = auth.extent;
            ui::kv("authorized", "yes");
            ui::kv(
                "txs",
                format!("{} / {}", e.transactions, e.transactions_allowance),
            );
            ui::kv(
                "bytes",
                format!("{} / {} allowance", e.bytes, e.bytes_allowance),
            );
            ui::kv("expires", format!("block #{}", auth.expiration));
        }
        None => ui::kv("authorized", "no (not authorized)"),
    }
    Ok(())
}

async fn store(
    env: &Env,
    path: String,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let data = std::fs::read(&path).with_context(|| format!("reading file {path}"))?;
    if data.len() > MAX_TRANSACTION_SIZE {
        bail!(
            "file {path} is {} bytes, exceeding the chain's MaxTransactionSize of {MAX_TRANSACTION_SIZE} bytes (2 MiB)",
            data.len()
        );
    }

    let cid = chain::raw_cid(&data);
    let gateway_url = format!("{}/ipfs/{cid}", env.ipfs_gateway);

    let client = chain::bulletin_client(env).await?;
    let signer = resolve_signer(mnemonic, derivation_path)?;

    match chain::store_block(&client, &signer, 0x55, &data).await? {
        chain::StoreOutcome::AlreadyPresent { block, index } => {
            ui::success(format!("already stored (block #{block} index {index})"));
        }
        chain::StoreOutcome::Stored { block, index } => {
            ui::success(format!("stored (block #{block} index {index})"));
        }
    }
    ui::kv("cid", cid);
    ui::kv("gateway", gateway_url);
    Ok(())
}

/// Summary of storing a CAR's blocks on the Bulletin chain.
pub struct CarStored {
    pub root: cid::Cid,
    pub stored: usize,
    pub skipped: usize,
}

/// Read a CARv1 file into its root CID + validated, upload-ready blocks. Verifies
/// each block is sha2-256, hashes to its CID, and fits one ≤2 MiB extrinsic.
pub async fn read_car_prepared(path: &str) -> Result<(cid::Cid, Vec<chain::PreparedBlock>)> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening CAR file {path}"))?;
    let mut car = iroh_car::CarReader::new(tokio::io::BufReader::new(file))
        .await
        .with_context(|| format!("parsing CARv1 header from {path}"))?;

    let root = *car
        .header()
        .roots()
        .first()
        .context("CAR header has no roots")?;

    let mut prepared = Vec::new();
    while let Some((cid, data)) = car.next_block().await.context("reading next CAR block")? {
        let hash = cid.hash();
        if hash.code() != 0x12 {
            bail!(
                "block {cid} uses multihash code 0x{:x}; only sha2-256 (0x12) CARs are supported",
                hash.code()
            );
        }
        let content_hash = chain::content_hash(&data);
        if hash.digest() != content_hash {
            bail!("block {cid}: CAR data does not hash to the CID digest (corrupt CAR?)");
        }
        if data.len() > MAX_TRANSACTION_SIZE {
            bail!(
                "block {cid} is {} bytes, exceeding the chain's MaxTransactionSize of {MAX_TRANSACTION_SIZE} bytes (2 MiB)",
                data.len()
            );
        }
        prepared.push(chain::PreparedBlock {
            codec: cid.codec(),
            data,
            content_hash,
        });
    }
    Ok((root, prepared))
}

/// Store a prepared block set on the Bulletin chain (each block keyed by its own
/// content hash) so `root`'s DAG resolves on the IPFS gateway. Reuses the single
/// `client` for the whole upload so metadata is downloaded once.
pub async fn store_prepared_blocks(
    env: &Env,
    client: &OnlineClient<BulletinConfig>,
    root: cid::Cid,
    prepared: Vec<chain::PreparedBlock>,
    signer: &Keypair,
) -> Result<CarStored> {
    let total = prepared.len();
    let (stored, skipped) = chain::store_car_blocks(
        client,
        env.bulletin_rpc,
        signer,
        &prepared,
        |done, stored, skipped| {
            ui::progress(format!(
                "blocks     {done}/{total} · stored {stored} · skipped {skipped}"
            ));
        },
    )
    .await?;
    ui::progress_clear();

    Ok(CarStored {
        root,
        stored,
        skipped,
    })
}

/// Store every IPLD block of a CARv1 individually (each keyed by its own content
/// hash) so the CAR's root DAG resolves on the IPFS gateway. Kubo chunks files
/// into ≤256 KiB blocks, so every block fits a single ≤2 MiB extrinsic.
pub async fn store_car_file(
    env: &Env,
    client: &OnlineClient<BulletinConfig>,
    path: &str,
    signer: &Keypair,
) -> Result<CarStored> {
    let (root, prepared) = read_car_prepared(path).await?;
    store_prepared_blocks(env, client, root, prepared, signer).await
}

async fn store_car(
    env: &Env,
    path: String,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let signer = resolve_signer(mnemonic, derivation_path)?;
    let client = chain::bulletin_client(env).await?;
    ui::step(format!("upload {path} to Bulletin"));
    let summary = store_car_file(env, &client, &path, &signer).await?;

    let total = summary.stored + summary.skipped;
    ui::kv("root", summary.root);
    ui::kv(
        "blocks",
        format!(
            "{} stored · {} skipped · {total} total",
            summary.stored, summary.skipped
        ),
    );
    ui::kv(
        "gateway",
        format!("{}/ipfs/{}/", env.ipfs_gateway, summary.root),
    );
    Ok(())
}
