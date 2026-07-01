use anyhow::{bail, Result};

/// Resolved environment. `--env` selects the Bulletin RPC *and* the Asset Hub
/// contract addresses as one matched set so v1/v2 addresses can never drift.
///
/// Values verified 2026-07 against paritytech/bulletin-deploy assets/environments.json
/// and dotli-community packages/config. Registry/Publisher addresses are added later
/// (name register / publish) — the fields here cover Slices 0-3.
#[derive(Debug, Clone)]
pub struct Env {
    pub id: &'static str,
    pub bulletin_rpc: &'static str,
    pub asset_hub_rpc: &'static str,
    pub ipfs_gateway: &'static str,
    pub dotns_content_resolver: &'static str,
}

impl Env {
    pub fn resolve(id: &str) -> Result<Env> {
        Ok(match id {
            "paseo-next-v2" => Env {
                id: "paseo-next-v2",
                bulletin_rpc: "wss://paseo-bulletin-next-rpc.polkadot.io",
                asset_hub_rpc: "wss://paseo-asset-hub-next-rpc.polkadot.io",
                ipfs_gateway: "https://paseo-bulletin-next-ipfs.polkadot.io",
                dotns_content_resolver: "0x8A26480b0B5Df3d4D9b95adc24a5Ecb33A5b8F64",
            },
            "preview" => Env {
                id: "preview",
                bulletin_rpc: "wss://previewnet.substrate.dev/bulletin",
                asset_hub_rpc: "wss://previewnet.substrate.dev/asset-hub",
                ipfs_gateway: "https://previewnet.substrate.dev/ipfs",
                dotns_content_resolver: "0xBD003d5Dd04E68aC60d529a46AEfBdEf8941868C",
            },
            other => bail!("unknown --env '{other}' (known: paseo-next-v2, preview)"),
        })
    }
}
