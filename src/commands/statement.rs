use crate::env::Env;
use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum Cmd {
    /// Publish a statement (People chain / Statement Store).
    Publish {
        /// Topic to publish under.
        topic: String,
    },
}

pub fn run(_env: &Env, _cmd: Cmd) -> Result<()> {
    println!(
        "statement — not yet implemented. Wire format (People-chain submission path) is unverified; \
         research-first (see plan: Later / Deferred)."
    );
    Ok(())
}
