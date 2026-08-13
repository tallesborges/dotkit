//! Optional `deploy.toml`: DotNS metadata written alongside a deploy — raw
//! text records (`[text]`, e.g. `manifest`, `executable`) plus an optional
//! `[product]` section that generates the RFC root manifest (display name,
//! description, Bulletin-hosted icon) for the base DotNS name.

use anyhow::{bail, Context, Result};
use cid::Cid;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Parsed `deploy.toml`. Unknown fields are rejected so typos fail loudly.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployConfig {
    /// Text records to set on the domain (`key -> value`); `BTreeMap` for stable order.
    #[serde(default)]
    pub text: BTreeMap<String, String>,
    /// Optional product metadata; when present, `deploy` uploads the icon to
    /// Bulletin and writes the generated root manifest as the `manifest` record.
    #[serde(default)]
    pub product: Option<ProductConfig>,
    /// Directory the config was loaded from; icon paths resolve against it.
    #[serde(skip)]
    pub base_dir: PathBuf,
}

/// The `[product]` section: single-SPA product metadata Browse renders from the
/// base name's root manifest. The icon `format` is inferred from the file
/// extension (`png`/`jpeg`); the icon is stored on Bulletin and referenced by CID.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductConfig {
    /// Human-facing product name (RFC `displayName`).
    pub display_name: String,
    /// Short product description (RFC `description`).
    #[serde(default)]
    pub description: String,
    /// Path to the icon file (PNG or JPEG), relative to the config's directory.
    pub icon: String,
}

/// RFC root manifest written as the base name's `manifest` text record. Field
/// order and names match `@parity/polkadot-app-deploy` (`$v`, `displayName`,
/// `description`, `icon`), so `serde_json` emits the exact wire shape.
#[derive(Serialize)]
struct RootManifest<'a> {
    #[serde(rename = "$v")]
    v: u8,
    #[serde(rename = "displayName")]
    display_name: &'a str,
    description: &'a str,
    icon: RootIcon<'a>,
}

#[derive(Serialize)]
struct RootIcon<'a> {
    cid: String,
    format: &'a str,
}

impl ProductConfig {
    /// The icon's RFC format string (`png` or `jpeg`), inferred from its file
    /// extension. Anything else is rejected — the manifest schema only allows
    /// these two formats.
    pub fn icon_format(&self) -> Result<&'static str> {
        let ext = Path::new(&self.icon)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        match ext.as_str() {
            "png" => Ok("png"),
            "jpg" | "jpeg" => Ok("jpeg"),
            _ => bail!(
                "[product] icon '{}' must be a .png or .jpg/.jpeg file (root manifest icon.format supports png and jpeg only)",
                self.icon
            ),
        }
    }

    /// Absolute path to the icon file, resolved against the config's directory.
    pub fn icon_path(&self, base_dir: &Path) -> PathBuf {
        base_dir.join(&self.icon)
    }

    /// Serialize the RFC root manifest JSON for this product with the uploaded
    /// icon `cid`. Compact JSON matching the reference `JSON.stringify` output.
    pub fn root_manifest_json(&self, icon_cid: &Cid) -> Result<String> {
        let manifest = RootManifest {
            v: 1,
            display_name: &self.display_name,
            description: &self.description,
            icon: RootIcon {
                cid: icon_cid.to_string(),
                format: self.icon_format()?,
            },
        };
        serde_json::to_string(&manifest).context("serializing product root manifest")
    }
}

impl DeployConfig {
    /// Load `deploy.toml` from `explicit` (must exist) or auto-detect `./deploy.toml`
    /// (absent → empty config). The build dir is never scanned — its files get uploaded.
    pub fn load(explicit: Option<&str>) -> Result<DeployConfig> {
        let path = match explicit {
            Some(p) => Some(PathBuf::from(p)),
            None => {
                let default = PathBuf::from("deploy.toml");
                default.is_file().then_some(default)
            }
        };
        let Some(path) = path else {
            return Ok(DeployConfig::default());
        };
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading deploy config {}", path.display()))?;
        let mut config: DeployConfig = toml::from_str(&raw)
            .with_context(|| format!("parsing deploy config {}", path.display()))?;
        config.base_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        config.validate()?;
        Ok(config)
    }

    /// Reject configurations with conflicting metadata sources and validate the
    /// product icon format up front, before any chain writes.
    fn validate(&self) -> Result<()> {
        if let Some(product) = &self.product {
            if self.text.contains_key("manifest") {
                bail!(
                    "deploy config sets both [product] and a manual [text].manifest — \
                     [product] generates the manifest record, so remove one \
                     (they are conflicting sources of truth)"
                );
            }
            if product.display_name.trim().is_empty() {
                bail!("[product] display_name must be a non-empty string");
            }
            // Surface an invalid icon extension before we upload or write anything.
            product.icon_format()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Result<DeployConfig> {
        let config: DeployConfig = toml::from_str(raw)?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn text_only_config_parses() {
        let config = parse("[text]\nmanifest = \"https://example.com/m.json\"\n").unwrap();
        assert!(config.product.is_none());
        assert_eq!(config.text.len(), 1);
    }

    #[test]
    fn product_config_parses_and_infers_format() {
        let config = parse(
            "[product]\ndisplay_name = \"TV Explorer\"\ndescription = \"Live TV\"\nicon = \"icon.png\"\n",
        )
        .unwrap();
        let product = config.product.unwrap();
        assert_eq!(product.display_name, "TV Explorer");
        assert_eq!(product.icon_format().unwrap(), "png");
    }

    #[test]
    fn jpeg_variants_infer_jpeg() {
        for name in ["icon.jpg", "icon.jpeg", "ICON.JPEG"] {
            let p = ProductConfig {
                display_name: "x".into(),
                description: String::new(),
                icon: name.into(),
            };
            assert_eq!(p.icon_format().unwrap(), "jpeg", "{name}");
        }
    }

    #[test]
    fn unsupported_icon_format_rejected() {
        let err =
            parse("[product]\ndisplay_name = \"x\"\ndescription = \"\"\nicon = \"icon.svg\"\n")
                .unwrap_err()
                .to_string();
        assert!(err.contains("png"), "{err}");
    }

    #[test]
    fn product_and_manual_manifest_conflict_rejected() {
        let err = parse(
            "[text]\nmanifest = \"x\"\n\n[product]\ndisplay_name = \"x\"\ndescription = \"\"\nicon = \"icon.png\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("[product]") && err.contains("[text].manifest"),
            "{err}"
        );
    }

    #[test]
    fn empty_display_name_rejected() {
        let err =
            parse("[product]\ndisplay_name = \"  \"\ndescription = \"\"\nicon = \"icon.png\"\n")
                .unwrap_err()
                .to_string();
        assert!(err.contains("display_name"), "{err}");
    }

    #[test]
    fn unknown_key_rejected() {
        assert!(parse("[product]\ndisplay_name = \"x\"\nicon = \"i.png\"\nbogus = 1\n").is_err());
    }

    #[test]
    fn root_manifest_json_shape() {
        let product = ProductConfig {
            display_name: "TV Explorer".into(),
            description: "Live TV".into(),
            icon: "icon.png".into(),
        };
        let cid = crate::bulletin::Hashing::Blake2b256.cid(0x55, b"fake-icon-bytes");
        let json = product.root_manifest_json(&cid).unwrap();
        let expected = format!(
            "{{\"$v\":1,\"displayName\":\"TV Explorer\",\"description\":\"Live TV\",\"icon\":{{\"cid\":\"{cid}\",\"format\":\"png\"}}}}"
        );
        assert_eq!(json, expected);
    }
}
