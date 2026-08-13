//! Environment table: which chains dotkit talks to, and with which DotNS TLD and
//! contract addresses.
//!
//! Two layers, merged by env id:
//!
//! 1. **Built-in defaults** — `assets/envs.toml`, compiled in with [`include_str!`]
//!    so dotkit needs no config file to run.
//! 2. **User overlay** — `~/.dotkit/envs.toml`, if it exists. An id that already
//!    exists patches only the fields it lists; a new id adds an environment.
//!
//! The overlay is what makes a local or unlisted chain usable without a rebuild: a
//! devnet on per-machine localhost ports needs no release, and after a chain wipe a
//! single redeployed address can be corrected in one line. `dotkit account env
//! --list` shows every known env and where it came from.
//!
//! Only envs that can be defined centrally *and* have been verified against a live
//! chain belong in the built-in table; everything else lives in the overlay.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Built-in environment table, compiled into the binary.
const BUILTIN: &str = include_str!("../assets/envs.toml");

const OVERLAY_DIR: &str = ".dotkit";
const OVERLAY_FILE: &str = "envs.toml";

/// One entry as written in a TOML file. Every field is optional so an overlay can
/// patch a single value; [`Env::from_entry`] enforces what a usable env needs.
/// Unknown fields are rejected so typos fail loudly instead of being ignored.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvEntry {
    name: Option<String>,
    bulletin_rpc: Option<String>,
    asset_hub_rpc: Option<String>,
    ipfs_gateway: Option<String>,
    tld: Option<String>,
    web_gateway: Option<String>,
    dotns_content_resolver: Option<String>,
    registrar_controller: Option<String>,
    registrar: Option<String>,
    registry: Option<String>,
    pop_rules: Option<String>,
    publisher: Option<String>,
}

impl EnvEntry {
    /// Overlay `other` onto `self`: every field `other` sets wins, the rest are kept.
    fn patch(&mut self, other: EnvEntry) {
        macro_rules! take {
            ($($field:ident),+ $(,)?) => {$(
                if let Some(v) = other.$field {
                    self.$field = Some(v);
                }
            )+};
        }
        take!(
            name,
            bulletin_rpc,
            asset_hub_rpc,
            ipfs_gateway,
            tld,
            web_gateway,
            dotns_content_resolver,
            registrar_controller,
            registrar,
            registry,
            pop_rules,
            publisher,
        );
    }
}

/// Where an env's definition came from, for `account env --list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvSource {
    /// Only in `assets/envs.toml`.
    Builtin,
    /// In the built-in table, with fields patched by `~/.dotkit/envs.toml`.
    Patched,
    /// Only in `~/.dotkit/envs.toml`.
    User,
}

impl EnvSource {
    pub fn as_str(self) -> &'static str {
        match self {
            EnvSource::Builtin => "builtin",
            EnvSource::Patched => "builtin+user",
            EnvSource::User => "user",
        }
    }
}

/// A resolved environment. `--env` selects the RPCs, the DotNS TLD *and* the Asset
/// Hub contract addresses as one matched set so they can never drift apart.
///
/// Addresses that dotkit doesn't know for an env are empty strings rather than
/// missing; the commands that need one check for empty and explain what's absent.
#[derive(Debug, Clone)]
pub struct Env {
    pub id: String,
    /// Human-facing label, e.g. "Paseo Next v2". Falls back to `id`.
    pub name: String,
    pub bulletin_rpc: String,
    pub asset_hub_rpc: String,
    pub ipfs_gateway: String,
    /// DotNS top-level domain, without the leading dot. Paseo v2 serves `.paseo`;
    /// PreviewNet is still `.dot` until its next wipe. Every name normalization,
    /// namehash and label strip goes through this — a wrong TLD silently reads and
    /// writes the wrong namespace.
    pub tld: String,
    /// Public web gateway domain that resolves `<name>` in a browser (e.g.
    /// `paseo.li`). Empty when the env has none.
    pub web_gateway: String,
    pub dotns_content_resolver: String,
    pub registrar_controller: String,
    /// DotNS Registrar — the ERC721 name-NFT contract (distinct from the
    /// RegistrarController). `ownerOf`/`quoteTransferFee`/`transferFrom` for name
    /// transfers live here.
    pub registrar: String,
    pub registry: String,
    pub pop_rules: String,
    /// Browse Publisher registry (`publish`/`unpublish` a label so it shows up in
    /// Browse). Deployed per-env from `paritytech/browse` and bound to that env's
    /// TLD; empty when the env has none (dotkit refuses `--publish` then).
    pub publisher: String,
    pub source: EnvSource,
}

/// `~/.dotkit/envs.toml`.
pub fn overlay_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME env var not set")?;
    Ok(PathBuf::from(home).join(OVERLAY_DIR).join(OVERLAY_FILE))
}

fn parse(toml_str: &str, what: &str) -> Result<BTreeMap<String, EnvEntry>> {
    toml::from_str(toml_str).with_context(|| format!("parsing {what}"))
}

/// The built-in table merged with the user overlay, keyed by env id.
fn table() -> Result<BTreeMap<String, (EnvEntry, EnvSource)>> {
    let mut merged: BTreeMap<String, (EnvEntry, EnvSource)> = parse(BUILTIN, "built-in envs.toml")?
        .into_iter()
        .map(|(id, entry)| (id, (entry, EnvSource::Builtin)))
        .collect();

    let path = overlay_path()?;
    if !path.exists() {
        return Ok(merged);
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let overlay = parse(&raw, &format!("{}", path.display()))?;

    for (id, entry) in overlay {
        match merged.get_mut(&id) {
            Some((base, source)) => {
                base.patch(entry);
                *source = EnvSource::Patched;
            }
            None => {
                merged.insert(id, (entry, EnvSource::User));
            }
        }
    }
    Ok(merged)
}

impl Env {
    /// Resolve one env by id, or explain which ids exist.
    pub fn resolve(id: &str) -> Result<Env> {
        let table = table()?;
        let Some((entry, source)) = table.get(id) else {
            let known = table.keys().cloned().collect::<Vec<_>>().join(", ");
            bail!(
                "unknown --env '{id}' (known: {known}). \
                 Add it to {} — see `dotkit account env --list`",
                overlay_path()?.display()
            );
        };
        Env::from_entry(id, entry.clone(), *source)
    }

    /// Every known env, built-in and user, for `account env --list`.
    pub fn all() -> Result<Vec<Env>> {
        table()?
            .into_iter()
            .map(|(id, (entry, source))| Env::from_entry(&id, entry, source))
            .collect()
    }

    /// Validate a merged entry into a usable [`Env`]. An env is only required to
    /// name its TLD — everything else may legitimately be unknown, and the command
    /// that needs a given endpoint or address reports it missing in context.
    fn from_entry(id: &str, entry: EnvEntry, source: EnvSource) -> Result<Env> {
        let tld = entry.tld.filter(|t| !t.is_empty()).with_context(|| {
            format!(
                "env '{id}' has no `tld` — DotNS names are namehashed with it, so it \
                 cannot be guessed. Set it in {}",
                overlay_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "~/.dotkit/envs.toml".into())
            )
        })?;
        if let Some(stripped) = tld.strip_prefix('.') {
            bail!("env '{id}' has `tld = \".{stripped}\"` — write it without the leading dot");
        }

        Ok(Env {
            id: id.to_string(),
            name: entry.name.unwrap_or_else(|| id.to_string()),
            bulletin_rpc: entry.bulletin_rpc.unwrap_or_default(),
            asset_hub_rpc: entry.asset_hub_rpc.unwrap_or_default(),
            ipfs_gateway: entry.ipfs_gateway.unwrap_or_default(),
            tld,
            web_gateway: entry.web_gateway.unwrap_or_default(),
            dotns_content_resolver: entry.dotns_content_resolver.unwrap_or_default(),
            registrar_controller: entry.registrar_controller.unwrap_or_default(),
            registrar: entry.registrar.unwrap_or_default(),
            registry: entry.registry.unwrap_or_default(),
            pop_rules: entry.pop_rules.unwrap_or_default(),
            publisher: entry.publisher.unwrap_or_default(),
            source,
        })
    }

    /// The Asset Hub RPC, or a clear error naming the env when it isn't configured.
    pub fn asset_hub_rpc(&self) -> Result<&str> {
        self.endpoint(&self.asset_hub_rpc, "asset_hub_rpc")
    }

    /// The Bulletin RPC, or a clear error naming the env when it isn't configured.
    pub fn bulletin_rpc(&self) -> Result<&str> {
        self.endpoint(&self.bulletin_rpc, "bulletin_rpc")
    }

    fn endpoint<'a>(&self, value: &'a str, field: &str) -> Result<&'a str> {
        if value.is_empty() {
            bail!(
                "env '{}' has no `{field}` configured — set it in {}",
                self.id,
                overlay_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "~/.dotkit/envs.toml".into())
            );
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_table_parses_and_validates() {
        let table = parse(BUILTIN, "builtin").unwrap();
        assert!(table.contains_key("paseo-next-v2"));
        for (id, entry) in table {
            Env::from_entry(&id, entry, EnvSource::Builtin)
                .unwrap_or_else(|e| panic!("built-in env '{id}' is invalid: {e}"));
        }
    }

    #[test]
    fn paseo_defaults_match_the_verified_deployment() {
        let entry = parse(BUILTIN, "builtin").unwrap();
        let env = Env::from_entry(
            "paseo-next-v2",
            entry["paseo-next-v2"].clone(),
            EnvSource::Builtin,
        )
        .unwrap();
        assert_eq!(env.tld, "paseo");
        assert_eq!(
            env.dotns_content_resolver,
            "0x7F74D7CD50f5a834270E2ad395a01b01891AB37d"
        );
        assert_eq!(env.registry, "0xf34054fd76BbF85f216cf9908226D5f0A72E50CA");
    }

    #[test]
    fn overlay_patches_only_listed_fields() {
        let mut base = parse(BUILTIN, "builtin").unwrap();
        let overlay = parse(
            r#"
            [paseo-next-v2]
            publisher = "0xdeadbeef"
            "#,
            "overlay",
        )
        .unwrap();
        base.get_mut("paseo-next-v2")
            .unwrap()
            .patch(overlay["paseo-next-v2"].clone());
        let env = Env::from_entry(
            "paseo-next-v2",
            base["paseo-next-v2"].clone(),
            EnvSource::Patched,
        )
        .unwrap();
        assert_eq!(env.publisher, "0xdeadbeef");
        // Untouched fields survive the patch.
        assert_eq!(env.tld, "paseo");
        assert_eq!(
            env.asset_hub_rpc,
            "wss://paseo-asset-hub-next-rpc.polkadot.io"
        );
    }

    #[test]
    fn overlay_can_add_a_new_env() {
        let entry = parse(
            r#"
            [mynet]
            tld = "test"
            asset_hub_rpc = "ws://127.0.0.1:9944"
            "#,
            "overlay",
        )
        .unwrap();
        let env = Env::from_entry("mynet", entry["mynet"].clone(), EnvSource::User).unwrap();
        assert_eq!(env.tld, "test");
        assert_eq!(env.asset_hub_rpc, "ws://127.0.0.1:9944");
        // Unset endpoints error in context instead of silently connecting nowhere.
        assert!(env.bulletin_rpc().is_err());
        assert!(env.asset_hub_rpc().is_ok());
    }

    #[test]
    fn tld_must_not_carry_a_leading_dot() {
        let entry = parse("[oops]\ntld = \".paseo\"\n", "overlay").unwrap();
        let err = Env::from_entry("oops", entry["oops"].clone(), EnvSource::User).unwrap_err();
        assert!(err.to_string().contains("without the leading dot"));
    }

    #[test]
    fn missing_tld_is_rejected() {
        let entry = parse("[oops]\nasset_hub_rpc = \"ws://x\"\n", "overlay").unwrap();
        assert!(Env::from_entry("oops", entry["oops"].clone(), EnvSource::User).is_err());
    }

    #[test]
    fn unknown_field_is_rejected() {
        assert!(parse("[oops]\ntld = \"dot\"\nreslover = \"0x1\"\n", "overlay").is_err());
    }
}
