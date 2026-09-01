use crate::chain;
use crate::dotns;
use crate::env::Env;
use crate::ui;
use anyhow::{Context, Result};
use clap::Subcommand;
use std::str::FromStr;
use subxt::utils::AccountId32;

#[derive(Subcommand)]
pub enum Cmd {
    /// Send native PAS to an account (Balances.transfer_keep_alive).
    Transfer {
        /// Destination SS58 address.
        dest: String,
        /// Amount in plancks (native PAS smallest unit).
        plancks: u128,
    },
    /// Ensure the signer has an H160 mapping (Revive.map_account).
    Map,
    /// Report whether this env's DotNS contracts are deployed (code-at-address).
    Status,
    /// DotNS naming ops (resolve, register, content records).
    #[command(subcommand)]
    Name(super::name::Cmd),
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    match cmd {
        Cmd::Transfer { dest, plancks } => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let dest = AccountId32::from_str(&dest)
                .map_err(|e| anyhow::anyhow!("invalid SS58 dest address: {e}"))?;
            let tx = chain::transfer_keep_alive(env, &signer, dest, plancks)
                .await
                .context("transfer failed")?;
            if ui::json() {
                ui::emit(&serde_json::json!({
                    "from": chain::account_id(&signer).to_string(),
                    "to": dest.to_string(),
                    "plancks": plancks,
                    "tx": format!("0x{}", hex::encode(tx)),
                }));
            } else {
                ui::success(format!("transferred {plancks} plancks"));
                ui::kv("from", chain::account_id(&signer));
                ui::kv("to", dest);
                ui::kv("tx", format!("0x{}", hex::encode(tx)));
            }
        }
        Cmd::Map => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let client = chain::asset_hub_client(env).await?;
            chain::ensure_mapped(&client, &signer).await?;
            let account = chain::account_id(&signer);
            if ui::json() {
                ui::emit(&serde_json::json!({
                    "account": account.to_string(),
                    "mapped": true,
                }));
            } else {
                ui::success(format!("account {account} is mapped on Asset Hub"));
            }
        }
        Cmd::Name(cmd) => super::name::run(env, cmd, mnemonic, derivation_path).await?,
        Cmd::Status => status(env).await?,
    }
    Ok(())
}

/// `asset-hub status` — per-address deployment state for the env's DotNS
/// contracts. This is the check for "has the post-wipe redeployment landed
/// yet?", so it reports every contract rather than failing on the first absent
/// one, and exits non-zero only when nothing is deployed.
async fn status(env: &Env) -> Result<()> {
    let client = chain::asset_hub_client(env).await?;
    let statuses = dotns::probe(&client, env, &dotns::Contract::ALL).await?;

    let deployed = statuses
        .iter()
        .filter(|s| s.state == dotns::State::Deployed)
        .count();
    let configured = statuses
        .iter()
        .filter(|s| s.state != dotns::State::Unconfigured)
        .count();

    if ui::json() {
        ui::emit(&serde_json::json!({
            "env": env.id,
            "asset_hub": env.asset_hub_rpc,
            "deployed": deployed,
            "configured": configured,
            "contracts": statuses.iter().map(|s| serde_json::json!({
                "contract": s.contract.label(),
                "address": s.address,
                "state": match s.state {
                    dotns::State::Deployed => "deployed",
                    dotns::State::Absent => "absent",
                    dotns::State::Unconfigured => "unconfigured",
                },
            })).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    ui::kv("env", &env.id);
    ui::kv("asset_hub", &env.asset_hub_rpc);
    for s in &statuses {
        let line = match s.state {
            dotns::State::Deployed => format!("✓ deployed  {}", s.address),
            dotns::State::Absent => format!("✗ absent    {}", s.address),
            dotns::State::Unconfigured => "– not configured for this env".to_string(),
        };
        ui::kv(s.contract.key(), line);
    }
    if deployed == 0 {
        ui::note(format!(
            "no DotNS contracts are deployed on {}; awaiting the post-wipe redeployment",
            env.id
        ));
    } else {
        ui::note(format!(
            "{deployed}/{configured} configured contracts deployed"
        ));
    }
    Ok(())
}
