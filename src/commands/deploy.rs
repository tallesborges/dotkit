use crate::env::Env;
use anyhow::Result;
use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
    /// Build directory to deploy (e.g. ./dist).
    pub dir: String,
    /// Target .dot domain (e.g. myapp00.dot).
    pub domain: String,
    /// Deploy a pre-built CAR instead of merkleizing the directory.
    #[arg(long)]
    pub input_car: Option<String>,
}

pub fn run(env: &Env, args: Args) -> Result<()> {
    println!(
        "trikit deploy [{}]  dir={}  domain={}",
        env.id, args.dir, args.domain
    );
    println!("not yet implemented — Slice 3: Kubo merkleize -> Bulletin upload -> setContenthash");
    Ok(())
}
