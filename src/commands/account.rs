use crate::bulletin;
use crate::chain;
use crate::env::{self, Env};
use crate::ui;
use anyhow::Result;
use clap::Subcommand;
use serde_json::json;

#[derive(Subcommand)]
pub enum Cmd {
    /// Print the resolved environment configuration.
    Env {
        /// List every known environment and where its definition came from.
        #[arg(long)]
        list: bool,
    },
    /// Derive the signer and prove connectivity to Asset Hub + Bulletin.
    Whoami,
    /// Show the signer's Asset Hub native (PAS) balance.
    Info,
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    match cmd {
        Cmd::Env { list: true } => {
            let envs = Env::all()?;
            if ui::json() {
                let rows: Vec<_> = envs
                    .iter()
                    .map(|e| {
                        json!({
                            "env": e.id,
                            "name": e.name,
                            "tld": e.tld,
                            "source": e.source.as_str(),
                            "asset_hub": e.asset_hub_rpc,
                            "bulletin": e.bulletin_rpc,
                        })
                    })
                    .collect();
                ui::emit(&json!({
                    "overlay": env::overlay_path()?.display().to_string(),
                    "environments": rows,
                }));
            } else {
                for e in &envs {
                    ui::kv(
                        &e.id,
                        format!(".{}  ({}, {})", e.tld, e.name, e.source.as_str()),
                    );
                }
                println!();
                ui::note(format!(
                    "patch or add envs in {}",
                    env::overlay_path()?.display()
                ));
            }
        }
        Cmd::Env { list: false } => {
            if ui::json() {
                ui::emit(&json!({
                    "env": env.id,
                    "name": env.name,
                    "source": env.source.as_str(),
                    "bulletin": env.bulletin_rpc,
                    "asset_hub": env.asset_hub_rpc,
                    "gateway": env.ipfs_gateway,
                    "tld": env.tld,
                    "resolver": env.dotns_content_resolver,
                }));
            } else {
                ui::kv("env", &env.id);
                ui::kv("source", env.source.as_str());
                ui::kv("bulletin", &env.bulletin_rpc);
                ui::kv("asset_hub", &env.asset_hub_rpc);
                ui::kv("gateway", &env.ipfs_gateway);
                ui::kv("tld", format!(".{}", env.tld));
                ui::kv("resolver", &env.dotns_content_resolver);
            }
        }
        Cmd::Whoami => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let account = chain::account_id(&signer);
            let (asset_hub, bulletin) =
                tokio::try_join!(chain::asset_hub_client(env), bulletin::bulletin_client(env))?;
            let h160 = chain::revive_address(&asset_hub, account).await?;
            let asset_hub_block = asset_hub.at_current_block().await?.block_number();
            let bulletin_block = bulletin.at_current_block().await?.block_number();

            if ui::json() {
                ui::emit(&json!({
                    "env": env.id,
                    "ss58": account.to_string(),
                    "h160": format!("0x{}", hex::encode(h160.0)),
                    "asset_hub_rpc": env.asset_hub_rpc,
                    "asset_hub_block": asset_hub_block,
                    "bulletin_rpc": env.bulletin_rpc,
                    "bulletin_block": bulletin_block,
                }));
            } else {
                ui::kv("env", &env.id);
                ui::kv("ss58", account);
                ui::kv("h160", format!("0x{}", hex::encode(h160.0)));
                ui::kv(
                    "asset_hub",
                    format!("{}  #{asset_hub_block}", env.asset_hub_rpc),
                );
                ui::kv(
                    "bulletin",
                    format!("{}  #{bulletin_block}", env.bulletin_rpc),
                );
            }
        }
        Cmd::Info => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let account = chain::account_id(&signer);
            let asset_hub = chain::asset_hub_client(env).await?;
            let h160 = chain::revive_address(&asset_hub, account).await?;
            let (free, reserved) = chain::account_balance(&asset_hub, account).await?;

            if ui::json() {
                ui::emit(&json!({
                    "env": env.id,
                    "ss58": account.to_string(),
                    "h160": format!("0x{}", hex::encode(h160.0)),
                    "free_pas": free as f64 / 1e10,
                    "reserved_pas": reserved as f64 / 1e10,
                    "free_plancks": free,
                    "reserved_plancks": reserved,
                }));
            } else {
                ui::kv("env", &env.id);
                ui::kv("ss58", account);
                ui::kv("h160", format!("0x{}", hex::encode(h160.0)));
                ui::kv("free", format!("{} PAS", free as f64 / 1e10));
                if reserved > 0 {
                    ui::kv("reserved", format!("{} PAS", reserved as f64 / 1e10));
                }
            }
        }
    }
    Ok(())
}
