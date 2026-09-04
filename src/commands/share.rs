use crate::bulletin;
use crate::dotshare;
use crate::env::Env;
use crate::pool;
use crate::ui;
use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use serde_json::json;
use std::path::Path;

#[derive(ClapArgs)]
pub struct Args {
    /// Path to the file to share.
    pub path: String,
    /// Display name shown in the viewer (defaults to the file name).
    #[arg(long)]
    pub name: Option<String>,
    /// MIME type driving how the viewer renders it (defaults to the extension).
    #[arg(long)]
    pub mime: Option<String>,
}

/// Store a file on Bulletin wrapped in the Dotshare envelope and print the
/// viewer link. Same single-block store path as `bulletin store`; the envelope is
/// what makes the drop open with its real name and type.
pub async fn run(
    env: &Env,
    args: Args,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
    pool_source: pool::PoolSource,
) -> Result<()> {
    let path = Path::new(&args.path);
    let data = std::fs::read(path).with_context(|| format!("reading file {}", args.path))?;

    let name = args.name.unwrap_or_else(|| dotshare::file_name(path));
    let mime = args
        .mime
        .unwrap_or_else(|| dotshare::infer_mime(path).to_string());
    let payload = dotshare::wrap(&name, &mime, &data);

    if payload.len() > bulletin::MAX_TRANSACTION_SIZE {
        bail!(
            "{} is {} bytes ({} wrapped in the Dotshare envelope), exceeding the chain's \
             MaxTransactionSize of {} bytes (2 MiB)",
            args.path,
            data.len(),
            payload.len(),
            bulletin::MAX_TRANSACTION_SIZE
        );
    }

    let cid = bulletin::raw_cid(&payload);
    let link = if env.web_gateway.is_empty() {
        None
    } else {
        Some(dotshare::share_url(&env.web_gateway, &cid.to_string()))
    };
    let host_link = dotshare::host_share_url(&env.tld, &cid.to_string());

    ui::step(format!("share {name} ({mime})"));
    let client = bulletin::bulletin_client(env).await?;
    let signer = super::bulletin::resolve_signer(mnemonic, derivation_path, pool_source)?;
    let (stored, block, index) = match bulletin::store_block(
        &client,
        &signer,
        0x55,
        bulletin::Hashing::Sha2_256,
        &payload,
    )
    .await?
    {
        bulletin::StoreOutcome::AlreadyPresent { block, index } => (false, block, index),
        bulletin::StoreOutcome::Stored { block, index } => (true, block, index),
    };

    if ui::json() {
        ui::emit(&json!({
            "cid": cid.to_string(),
            "link": link,
            "host_link": host_link,
            "gateway": format!("{}/ipfs/{cid}", env.ipfs_gateway),
            "name": name,
            "mime": mime,
            "size": data.len(),
            "stored": stored,
            "block": block,
            "index": index,
        }));
    } else {
        if stored {
            ui::success(format!("stored (block #{block} index {index})"));
        } else {
            ui::success(format!("already stored (block #{block} index {index})"));
        }
        match &link {
            Some(link) => ui::kv("link", link),
            None => ui::note(format!(
                "env '{}' has no public web gateway — browser link unavailable",
                env.id
            )),
        }
        ui::kv("host link", &host_link);
        ui::kv("cid", cid);
    }
    Ok(())
}
