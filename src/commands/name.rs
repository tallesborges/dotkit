use crate::chain;
use crate::dotns;
use crate::env::Env;
use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum Cmd {
    /// Resolve a .dot name to its contenthash CID.
    Resolve {
        /// The .dot name (e.g. myapp00.dot).
        name: String,
    },
    /// Read the raw contenthash record of a .dot name.
    Content {
        /// The .dot name.
        name: String,
    },
}

pub async fn run(env: &Env, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Resolve { name } => {
            let name = dotns::normalize_name(&name);
            let contenthash = chain::resolve_contenthash(env, &name).await?;
            if contenthash.is_empty() {
                println!("no contenthash set for {name}");
            } else {
                println!("{}", dotns::contenthash_to_cid(&contenthash)?);
            }
        }
        Cmd::Content { name } => {
            let name = dotns::normalize_name(&name);
            let contenthash = chain::resolve_contenthash(env, &name).await?;
            if contenthash.is_empty() {
                println!("no contenthash set for {name}");
            } else {
                println!("0x{}", hex::encode(&contenthash));
            }
        }
    }
    Ok(())
}
