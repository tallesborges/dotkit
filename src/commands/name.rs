use crate::chain;
use crate::dotns;
use crate::env::Env;
use crate::ui;
use anyhow::{bail, Context, Result};
use cid::Cid;
use clap::Subcommand;
use serde_json::json;
use subxt::utils::H160;

#[derive(Subcommand)]
pub enum Cmd {
    /// Resolve a DotNS name to its contenthash CID.
    Resolve {
        /// The name; the env's TLD is appended when omitted (e.g. myapp00.paseo).
        name: String,
    },
    /// Show whether a DotNS name is registered and who owns it.
    #[command(name = "owner-of", alias = "oo")]
    OwnerOf {
        /// The name; the env's TLD is appended when omitted (e.g. myapp00.paseo).
        name: String,
    },
    /// Read-only overview of a DotNS name (owner, tier, price, contenthash).
    Lookup {
        /// The name; the env's TLD is appended when omitted (e.g. myapp00.paseo).
        name: String,
    },
    /// Register an open-tier DotNS name (commit/reveal) to the signer.
    Register {
        /// The name to register; the env's TLD is appended when omitted.
        name: String,
    },
    /// Transfer a DotNS name you own to another account (0x H160 or SS58).
    Transfer {
        /// The name to transfer (must be owned by the signer).
        name: String,
        /// Recipient address: a 0x-prefixed H160 or an SS58 address.
        to: String,
    },
    /// List a DotNS name in Browse via the Publisher registry.
    Publish {
        /// The name to publish (must be owned by the signer).
        name: String,
    },
    /// Remove a DotNS name from Browse via the Publisher registry.
    Unpublish {
        /// The name to unpublish (must be owned by the signer).
        name: String,
    },
    /// Read or set a DotNS name's raw contenthash record.
    #[command(subcommand)]
    Content(ContentCmd),
    /// Read or set a DotNS name's text records (e.g. manifest, executable).
    #[command(subcommand)]
    Text(TextCmd),
    /// Create a subnode (subdomain) under a name you own.
    #[command(subcommand)]
    Subnode(SubnodeCmd),
}

#[derive(Subcommand)]
pub enum ContentCmd {
    /// Bind a CID to a DotNS name's contenthash record (signed Revive.call).
    Set {
        /// The name (must be owned by the signer).
        name: String,
        /// The CIDv1 to bind (e.g. bafy...).
        cid: String,
    },
    /// Read the raw contenthash record of a DotNS name (`asset-hub name content <name>`).
    #[command(external_subcommand)]
    Read(Vec<String>),
}

#[derive(Subcommand)]
pub enum TextCmd {
    /// Read a text record (e.g. `asset-hub name text get myapp00 manifest`).
    Get {
        /// The name.
        name: String,
        /// Record key (e.g. manifest, executable, url).
        key: String,
    },
    /// Set a text record on a DotNS name (signed Revive.call).
    Set {
        /// The name (must be owned by the signer).
        name: String,
        /// Record key (e.g. manifest, executable).
        key: String,
        /// Record value.
        value: String,
    },
}

#[derive(Subcommand)]
pub enum SubnodeCmd {
    /// Create/reassign a subnode under a parent name you own (signed Revive.call).
    Create {
        /// The full child name, e.g. app.myapp.paseo (TLD appended when omitted).
        name: String,
        /// Owner of the new subnode: a 0x H160 or SS58 address. Defaults to the signer.
        to: Option<String>,
    },
}

/// Extract the name from `name content <name>`.
///
/// [`ContentCmd::Read`] is an `external_subcommand`, so clap hands it every
/// remaining token verbatim — including global flags like `--env`, which are
/// then never parsed and silently fall back to their defaults. Reading the
/// wrong environment's contenthash is worse than refusing, so anything beyond a
/// single bare name is rejected with the working invocation spelled out.
fn content_read_name(args: &[String]) -> Result<&String> {
    let name = args.first().context("usage: name content <name>")?;
    if let Some(extra) = args.get(1) {
        bail!(
            "`name content` takes exactly one argument, got an extra `{extra}`.\n  It is an \
             external subcommand, so trailing global flags are swallowed rather than applied — \
             put them before the subcommand: `dotkit --env <id> asset-hub name content {name}`"
        );
    }
    if name.starts_with('-') {
        bail!("expected a name, got the flag `{name}` — see `name content --help`");
    }
    Ok(name)
}

/// Condense a contract-call error down to the protocol's own words.
///
/// `revive_view` reports a revert as `contract call reverted: custom error 0x…:
/// "<message>"`. The quoted message is the part a reader can act on, so prefer
/// it and fall back to the full chain when the revert carried no string.
fn revert_summary(err: &anyhow::Error) -> String {
    let text = err.to_string();
    match (text.find('"'), text.rfind('"')) {
        (Some(open), Some(close)) if close > open + 1 => text[open + 1..close].to_string(),
        _ => text,
    }
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    match cmd {
        Cmd::Resolve { name } => {
            let name = dotns::normalize_name(&name, &env.tld);
            let client = chain::asset_hub_client(env).await?;
            let contenthash = dotns::resolve_contenthash(&client, env, &name).await?;
            let cid = if contenthash.is_empty() {
                None
            } else {
                Some(dotns::contenthash_to_cid(&contenthash)?)
            };
            if ui::json() {
                ui::emit(&json!({ "name": name, "cid": cid }));
            } else {
                match cid {
                    Some(cid) => println!("{cid}"),
                    None => println!("no contenthash set for {name}"),
                }
            }
        }
        Cmd::OwnerOf { name } => {
            owner_of(env, &name).await?;
        }
        Cmd::Lookup { name } => {
            lookup(env, &name).await?;
        }
        Cmd::Register { name } => {
            register(env, &name, mnemonic, derivation_path).await?;
        }
        Cmd::Transfer { name, to } => {
            transfer(env, &name, &to, mnemonic, derivation_path).await?;
        }
        Cmd::Publish { name } => {
            publish(env, &name, true, mnemonic, derivation_path).await?;
        }
        Cmd::Unpublish { name } => {
            publish(env, &name, false, mnemonic, derivation_path).await?;
        }
        Cmd::Content(ContentCmd::Read(args)) => {
            let raw = content_read_name(&args)?;
            let name = dotns::normalize_name(raw, &env.tld);
            let client = chain::asset_hub_client(env).await?;
            let contenthash = dotns::resolve_contenthash(&client, env, &name).await?;
            let hex = (!contenthash.is_empty()).then(|| format!("0x{}", hex::encode(&contenthash)));
            if ui::json() {
                ui::emit(&json!({ "name": name, "contenthash": hex }));
            } else {
                match hex {
                    Some(hex) => println!("{hex}"),
                    None => println!("no contenthash set for {name}"),
                }
            }
        }
        Cmd::Content(ContentCmd::Set { name, cid }) => {
            set(env, &name, &cid, mnemonic, derivation_path).await?;
        }
        Cmd::Text(TextCmd::Get { name, key }) => {
            let name = dotns::normalize_name(&name, &env.tld);
            let client = chain::asset_hub_client(env).await?;
            let value = dotns::resolve_text(&client, env, &name, &key).await?;
            if ui::json() {
                ui::emit(&json!({ "name": name, "key": key, "value": value }));
            } else if value.is_empty() {
                println!("no '{key}' text record set for {name}");
            } else {
                println!("{value}");
            }
        }
        Cmd::Text(TextCmd::Set { name, key, value }) => {
            text_set(env, &name, &key, &value, mnemonic, derivation_path).await?;
        }
        Cmd::Subnode(SubnodeCmd::Create { name, to }) => {
            subnode_create(env, &name, to.as_deref(), mnemonic, derivation_path).await?;
        }
    }
    Ok(())
}

async fn owner_of(env: &Env, name: &str) -> Result<()> {
    let name = dotns::normalize_name(name, &env.tld);
    let client = chain::asset_hub_client(env).await?;
    let owner = dotns::name_owner(&client, env, &name).await?;
    let owner_hex = owner.map(|o| format!("0x{}", hex::encode(o.0)));

    if ui::json() {
        ui::emit(&json!({
            "name": name,
            "registered": owner.is_some(),
            "owner": owner_hex,
        }));
    } else {
        ui::kv("name", &name);
        match owner_hex {
            Some(owner) => {
                ui::kv("registered", "yes");
                ui::kv("owner", owner);
            }
            None => ui::kv("registered", "no (unregistered)"),
        }
    }
    Ok(())
}

async fn lookup(env: &Env, name: &str) -> Result<()> {
    let name = dotns::normalize_name(name, &env.tld);
    let client = chain::asset_hub_client(env).await?;

    let owner = dotns::name_owner(&client, env, &name).await?;
    let owner_hex = owner.map(|o| format!("0x{}", hex::encode(o.0)));

    let contenthash = dotns::resolve_contenthash(&client, env, &name).await?;
    let cid = if contenthash.is_empty() {
        None
    } else {
        Some(dotns::contenthash_to_cid(&contenthash)?)
    };

    // Classification (required PoP tier + human status) reverts for labels that
    // break the digit-suffix rule; treat that as "unavailable" rather than fatal.
    let classify = dotns::classify_name(&client, env, &name).await.ok();
    let (tier, status) = match &classify {
        Some((tier, status)) => (Some(*tier), Some(status.clone())),
        None => (None, None),
    };

    // Base list price for a fresh registrant (zero owner ⇒ no discount). Since
    // the 2026-09-01 pricing rework this reverts outright for personhood-gated
    // labels ("Short names are not for sale"), which is a fact worth showing
    // rather than an error worth hiding: those names cannot be bought at any
    // price, only minted through the PoP gateway.
    let price = dotns::name_price_native(&client, env, &name, H160([0u8; 20])).await;

    if ui::json() {
        ui::emit(&json!({
            "name": name,
            "registered": owner.is_some(),
            "owner": owner_hex,
            "required_tier": tier,
            "tier_name": tier.map(dotns::tier_name),
            "status": status,
            "price_pas": price.as_ref().ok().map(|p| *p as f64 / 1e10),
            "price_unavailable": price.as_ref().err().map(revert_summary),
            "cid": cid,
        }));
    } else {
        ui::kv("name", &name);
        match owner_hex {
            Some(owner) => {
                ui::kv("registered", "yes");
                ui::kv("owner", owner);
            }
            None => ui::kv("registered", "no (available)"),
        }
        if let Some(tier) = tier {
            ui::kv("tier", format!("{} ({tier})", dotns::tier_name(tier)));
        }
        if let Some(status) = &status {
            ui::kv("status", status);
        }
        match &price {
            Ok(price) => ui::kv("price", format!("~{} PAS", *price as f64 / 1e10)),
            Err(err) => ui::kv("price", format!("not for sale ({})", revert_summary(err))),
        }
        ui::kv("content", cid.as_deref().unwrap_or("(none)"));
    }
    Ok(())
}

async fn register(
    env: &Env,
    name: &str,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let name = dotns::normalize_name(name, &env.tld);
    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;

    let (owner, value_native) = dotns::register_name(env, &signer, &name).await?;
    let cost_pas = value_native as f64 / 1e10;
    if ui::json() {
        ui::emit(&json!({
            "name": name,
            "owner": format!("0x{}", hex::encode(owner.0)),
            "cost_pas": cost_pas,
        }));
    } else {
        println!();
        ui::success(format!("registered {name}"));
        ui::kv("owner", format!("0x{}", hex::encode(owner.0)));
        ui::kv("cost", format!("~{cost_pas} PAS"));
    }
    Ok(())
}

async fn transfer(
    env: &Env,
    name: &str,
    to: &str,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let name = dotns::normalize_name(name, &env.tld);
    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;

    let outcome = dotns::transfer_name(env, &signer, &name, to).await?;
    let fee_pas = outcome.fee_native as f64 / 1e10;
    if ui::json() {
        ui::emit(&json!({
            "name": name,
            "from": format!("0x{}", hex::encode(outcome.from.0)),
            "to": format!("0x{}", hex::encode(outcome.to.0)),
            "fee_pas": fee_pas,
            "tx": format!("0x{}", hex::encode(outcome.tx)),
        }));
    } else {
        ui::success(format!("transferred {name}"));
        ui::kv("from", format!("0x{}", hex::encode(outcome.from.0)));
        ui::kv("to", format!("0x{}", hex::encode(outcome.to.0)));
        ui::kv("fee", format!("~{fee_pas} PAS"));
    }
    Ok(())
}

async fn publish(
    env: &Env,
    name: &str,
    publish: bool,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
    let outcome = if publish {
        crate::publisher::publish(env, &signer, name).await?
    } else {
        crate::publisher::unpublish(env, &signer, name).await?
    };
    let verb = if publish { "published" } else { "unpublished" };
    if ui::json() {
        ui::emit(&json!({
            "name": dotns::normalize_name(name, &env.tld),
            "label": outcome.label,
            "published": publish,
            "publisher": format!("0x{}", hex::encode(outcome.publisher.0)),
            "tx": format!("0x{}", hex::encode(outcome.tx)),
        }));
    } else {
        ui::success(format!("{verb} {}.{}", outcome.label, env.tld));
        ui::kv("tx", format!("0x{}", hex::encode(outcome.tx)));
    }
    Ok(())
}

async fn set(
    env: &Env,
    name: &str,
    cid: &str,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let name = dotns::normalize_name(name, &env.tld);
    let cid = Cid::try_from(cid).with_context(|| format!("invalid CID '{cid}'"))?;
    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;

    ui::step(format!("bind {name} → {}", ui::ellipsize(&cid.to_string())));
    let client = chain::asset_hub_client(env).await?;
    let expected = dotns::set_contenthash(&client, env, &signer, &name, &cid).await?;

    let onchain = dotns::resolve_contenthash(&client, env, &name).await?;
    if onchain != expected {
        bail!(
            "read-back mismatch: set 0x{} but chain has 0x{}",
            hex::encode(&expected),
            hex::encode(&onchain)
        );
    }
    if ui::json() {
        ui::emit(&json!({ "name": name, "cid": cid.to_string() }));
    } else {
        ui::success(format!("bound {name}"));
        ui::kv("cid", cid);
    }
    Ok(())
}

async fn text_set(
    env: &Env,
    name: &str,
    key: &str,
    value: &str,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let name = dotns::normalize_name(name, &env.tld);
    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;

    ui::step(format!("set '{key}' on {name}"));
    let client = chain::asset_hub_client(env).await?;
    dotns::set_text(&client, env, &signer, &name, key, value).await?;

    let onchain = dotns::resolve_text(&client, env, &name, key).await?;
    if onchain != value {
        bail!("read-back mismatch: set '{value}' but chain has '{onchain}'");
    }
    if ui::json() {
        ui::emit(&json!({ "name": name, "key": key, "value": value }));
    } else {
        ui::success(format!("set '{key}' on {name}"));
        ui::kv(key, value);
    }
    Ok(())
}

async fn subnode_create(
    env: &Env,
    child: &str,
    to: Option<&str>,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let child = dotns::normalize_name(child, &env.tld);
    let (sub_label, parent) = child
        .split_once('.')
        .context("expected a child name like app.myapp — got a bare label")?;
    if parent == env.tld {
        bail!(
            "{child} has no parent name — give a child of a registered name, e.g. app.myapp.{}",
            env.tld
        );
    }

    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
    let outcome = dotns::create_subnode(env, &signer, parent, sub_label, to).await?;

    if ui::json() {
        ui::emit(&json!({
            "name": outcome.subnode_name,
            "node": format!("0x{}", hex::encode(outcome.subnode)),
            "owner": format!("0x{}", hex::encode(outcome.owner.0)),
            "tx": format!("0x{}", hex::encode(outcome.tx)),
        }));
    } else {
        ui::success(format!("created {}", outcome.subnode_name));
        ui::kv("owner", format!("0x{}", hex::encode(outcome.owner.0)));
        ui::kv("node", format!("0x{}", hex::encode(outcome.subnode)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::content_read_name;

    #[test]
    fn content_read_accepts_a_bare_name() {
        let args = vec!["myapp.paseo".to_string()];
        assert_eq!(content_read_name(&args).unwrap(), "myapp.paseo");
    }

    #[test]
    fn content_read_rejects_swallowed_global_flags() {
        // `--env` after the subcommand is captured by the external subcommand and
        // never applied, so it must fail loudly instead of reading the default env.
        let args = vec![
            "myapp.paseo".to_string(),
            "--env".to_string(),
            "preview".to_string(),
        ];
        let err = content_read_name(&args).unwrap_err().to_string();
        assert!(err.contains("--env"), "{err}");
        assert!(err.contains("before the subcommand"), "{err}");
    }

    #[test]
    fn content_read_rejects_a_leading_flag() {
        let args = vec!["--json".to_string()];
        assert!(content_read_name(&args).is_err());
    }

    #[test]
    fn revert_summary_prefers_the_quoted_protocol_message() {
        let err = anyhow::anyhow!(
            "contract call reverted: custom error 0x2dfc7d98: \"Short names are not for sale\""
        );
        assert_eq!(super::revert_summary(&err), "Short names are not for sale");
    }

    #[test]
    fn revert_summary_falls_back_to_the_whole_error() {
        let err = anyhow::anyhow!("contract call reverted: custom error 0xdeadbeef");
        assert_eq!(
            super::revert_summary(&err),
            "contract call reverted: custom error 0xdeadbeef"
        );
    }

    #[test]
    fn content_read_requires_a_name() {
        assert!(content_read_name(&[]).is_err());
    }
}
