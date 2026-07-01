use crate::chain;
use crate::env::Env;
use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum Cmd {
    /// Print the resolved environment configuration.
    Env,
    /// Derive the signer and prove connectivity to Asset Hub + Bulletin.
    Whoami,
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    match cmd {
        Cmd::Env => {
            println!("env                    {}", env.id);
            println!("bulletin_rpc           {}", env.bulletin_rpc);
            println!("asset_hub_rpc          {}", env.asset_hub_rpc);
            println!("ipfs_gateway           {}", env.ipfs_gateway);
            println!("dotns_content_resolver {}", env.dotns_content_resolver);
        }
        Cmd::Whoami => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let account = chain::account_id(&signer);
            let h160 = chain::revive_address(env, account.clone()).await?;
            let asset_hub_block = chain::latest_block_number(env.asset_hub_rpc).await?;
            let bulletin_block = chain::latest_block_number(env.bulletin_rpc).await?;

            println!("env         {}", env.id);
            println!("ss58        {account}");
            println!("h160        0x{}", hex::encode(h160.0));
            println!("asset_hub   {}  #{asset_hub_block}", env.asset_hub_rpc);
            println!("bulletin    {}  #{bulletin_block}", env.bulletin_rpc);
        }
    }
    Ok(())
}
