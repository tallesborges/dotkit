use crate::chain::{self, bulletin, BulletinConfig};
use crate::env::Env;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::str::FromStr;
use subxt::utils::AccountId32;
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

/// Standard Substrate dev phrase. Its `//deploy/0` derivation is authorized to
/// write to the paseo-next-v2 Bulletin chain, so it's the default signer here.
const DEV_PHRASE: &str = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";

/// Chain-enforced `MaxTransactionSize` (2 MiB).
const MAX_TRANSACTION_SIZE: usize = 2 * 1024 * 1024;

#[derive(Subcommand)]
pub enum Cmd {
    /// Store a single blob/file on the Bulletin chain.
    Store {
        /// Path to the file to store.
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
    }
}

/// Resolve the write signer: the caller's mnemonic when supplied, otherwise the
/// authorized dev-phrase `//deploy/0` account (dev Alice has no Bulletin quota).
fn resolve_signer(mnemonic: Option<String>, derivation_path: Option<String>) -> Result<Keypair> {
    match mnemonic {
        Some(phrase) => chain::build_signer(Some(&phrase), derivation_path.as_deref()),
        None => chain::build_signer(Some(DEV_PHRASE), Some("//deploy/0")),
    }
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

    let client = OnlineClient::<BulletinConfig>::from_url(env.bulletin_rpc)
        .await
        .with_context(|| format!("connecting to Bulletin RPC {}", env.bulletin_rpc))?;

    let scope = bulletin::runtime_types::pallet_bulletin_transaction_storage::types::AuthorizationScope::Account(account.clone());
    let address = bulletin::storage().transaction_storage().authorizations();
    let at = client.at_current_block().await?;
    let authorization = at
        .storage()
        .try_fetch(address, (scope,))
        .await
        .context("reading TransactionStorage.Authorizations")?;

    println!("address                {account}");
    match authorization {
        Some(value) => {
            let auth = value.decode().context("decoding Authorization")?;
            let e = auth.extent;
            println!("authorized             yes");
            println!(
                "transactions           {} / {}",
                e.transactions, e.transactions_allowance
            );
            println!("bytes_stored           {}", e.bytes);
            println!("bytes_allowance        {}", e.bytes_allowance);
            println!("expiration_block       {}", auth.expiration);
        }
        None => println!("authorized             no (not authorized)"),
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
    let content_hash = chain::content_hash(&data);
    let gateway_url = format!("{}/ipfs/{cid}", env.ipfs_gateway);

    let client = OnlineClient::<BulletinConfig>::from_url(env.bulletin_rpc)
        .await
        .with_context(|| format!("connecting to Bulletin RPC {}", env.bulletin_rpc))?;

    let location_address = bulletin::storage()
        .transaction_storage()
        .transaction_by_content_hash();

    let at = client.at_current_block().await?;
    let existing = at
        .storage()
        .try_fetch(location_address, (content_hash,))
        .await
        .context("reading TransactionStorage.TransactionByContentHash")?;
    if let Some(value) = existing {
        let (block, index) = value.decode().context("decoding stored location")?;
        println!("already stored at block #{block} index {index}");
        println!("cid      {cid}");
        println!("gateway  {gateway_url}");
        return Ok(());
    }
    drop(at);

    let signer = resolve_signer(mnemonic, derivation_path)?;
    let cid_config =
        bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::CidConfig {
            codec: 0x55,
            hashing:
                bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::HashingAlgorithm::Sha2_256,
        };
    let call = bulletin::tx()
        .transaction_storage()
        .store_with_cid_config(cid_config, data);

    client
        .tx()
        .await?
        .sign_and_submit_then_watch_default(&call, &signer)
        .await
        .context("submitting store_with_cid_config")?
        .wait_for_finalized_success()
        .await
        .context("store_with_cid_config did not finalize successfully")?;

    let location_address = bulletin::storage()
        .transaction_storage()
        .transaction_by_content_hash();
    let at = client.at_current_block().await?;
    let (block, index) = at
        .storage()
        .try_fetch(location_address, (content_hash,))
        .await
        .context("re-reading TransactionByContentHash after store")?
        .context("store finalized but TransactionByContentHash is still empty")?
        .decode()
        .context("decoding stored location")?;

    println!("stored at block #{block} index {index}");
    println!("cid      {cid}");
    println!("gateway  {gateway_url}");
    Ok(())
}
