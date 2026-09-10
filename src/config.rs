//! Optional `deploy.toml`: DotNS metadata written alongside a deploy — raw
//! text records (`[text]`, e.g. `manifest`, `executable`), an optional
//! `[product]` section that generates the RFC root manifest (display name,
//! description, Bulletin-hosted icon) for the base DotNS name, and optional
//! `[[executables]]` entries that publish App / Worker executables to
//! `app.<domain>` / `worker.<domain>`.

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
    /// Executables to publish under the domain, one subdomain per entry.
    #[serde(default)]
    pub executables: Vec<ExecutableConfig>,
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

/// Which kind of executable an `[[executables]]` entry publishes. The kind is
/// also the subdomain label, matching what ships on chain today
/// (`app.<name>` / `worker.<name>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutableKind {
    App,
    Worker,
}

impl ExecutableKind {
    /// The `kind` string in the `executable` record and the subdomain label.
    pub fn label(self) -> &'static str {
        match self {
            ExecutableKind::App => "app",
            ExecutableKind::Worker => "worker",
        }
    }
}

/// One `[[executables]]` entry: a build directory published as a DotNS
/// executable under `<kind>.<domain>`, with its `executable` text record
/// generated from these fields.
///
/// ```toml
/// [[executables]]
/// kind = "app"
/// path = "dist/app"
/// app_version = [0, 0, 1]
/// runtime = "web"
/// entrypoint = "index.html"
///
/// [[executables]]
/// kind = "worker"
/// path = "dist/worker"
/// app_version = [0, 0, 1]
/// entrypoint = "index.js"
/// includes = { chat = true, pocket = false }
/// ```
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableConfig {
    pub kind: ExecutableKind,
    /// Build directory for this executable, relative to the config's directory.
    pub path: String,
    /// Product version as `[major, minor, patch]` (record `appVersion`).
    pub app_version: [u16; 3],
    /// Runtime entrypoint inside the build dir. Required for workers and for
    /// App v2; unused by App v1.
    #[serde(default)]
    pub entrypoint: Option<String>,
    /// App runtime kind (e.g. `web`). Its presence selects the **v2** app
    /// manifest, which is embedded into the build as `manifest.json`.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Worker capability flags (record `includes`), e.g. `chat` / `pocket`.
    #[serde(default)]
    pub includes: Option<BTreeMap<String, bool>>,
}

/// App executable record, v2: the runtime is declared, and this manifest is
/// also embedded in the build directory as `manifest.json`.
#[derive(Serialize)]
struct AppManifestV2<'a> {
    #[serde(rename = "$v")]
    v: u8,
    kind: &'a str,
    #[serde(rename = "appVersion")]
    app_version: [u16; 3],
    runtime: RuntimeManifest<'a>,
}

#[derive(Serialize)]
struct RuntimeManifest<'a> {
    kind: &'a str,
    entrypoint: &'a str,
}

/// App executable record, v1 — no runtime block, nothing embedded in the build.
#[derive(Serialize)]
struct AppManifestV1<'a> {
    #[serde(rename = "$v")]
    v: u8,
    kind: &'a str,
    #[serde(rename = "appVersion")]
    app_version: [u16; 3],
}

/// Worker executable record. Field order matches what is deployed on chain
/// (`$v`, `kind`, `appVersion`, `entrypoint`, `includes`).
#[derive(Serialize)]
struct WorkerManifest<'a> {
    #[serde(rename = "$v")]
    v: u8,
    kind: &'a str,
    #[serde(rename = "appVersion")]
    app_version: [u16; 3],
    entrypoint: &'a str,
    includes: &'a BTreeMap<String, bool>,
}

/// Empty `includes` map for workers that declare none, so the record still
/// carries the key.
static NO_INCLUDES: BTreeMap<String, bool> = BTreeMap::new();

impl ExecutableConfig {
    /// Absolute build directory for this executable.
    pub fn dir(&self, base_dir: &Path) -> PathBuf {
        base_dir.join(&self.path)
    }

    /// The subdomain label this executable publishes to.
    pub fn label(&self) -> &'static str {
        self.kind.label()
    }

    /// Whether this entry uses the **v2** app manifest, which has to be
    /// embedded in the build directory as `manifest.json` *before*
    /// merkleization (it changes the content CID).
    pub fn embeds_manifest(&self) -> bool {
        self.kind == ExecutableKind::App && self.runtime.is_some()
    }

    /// The compact `executable` record JSON for this entry.
    pub fn executable_json(&self) -> Result<String> {
        let json = match self.kind {
            ExecutableKind::App => match (&self.runtime, &self.entrypoint) {
                (Some(runtime), Some(entrypoint)) => serde_json::to_string(&AppManifestV2 {
                    v: 2,
                    kind: "app",
                    app_version: self.app_version,
                    runtime: RuntimeManifest {
                        kind: runtime,
                        entrypoint,
                    },
                }),
                _ => serde_json::to_string(&AppManifestV1 {
                    v: 1,
                    kind: "app",
                    app_version: self.app_version,
                }),
            },
            ExecutableKind::Worker => {
                let entrypoint = self
                    .entrypoint
                    .as_deref()
                    .context("worker executable has no entrypoint")?;
                serde_json::to_string(&WorkerManifest {
                    v: 1,
                    kind: "worker",
                    app_version: self.app_version,
                    entrypoint,
                    includes: self.includes.as_ref().unwrap_or(&NO_INCLUDES),
                })
            }
        };
        json.context("serializing executable record")
    }

    /// Reject entries whose fields don't apply to their kind, before any upload
    /// or chain write.
    fn validate(&self) -> Result<()> {
        if self.path.trim().is_empty() {
            bail!("[[executables]] kind = \"{}\" needs a `path`", self.label());
        }
        match self.kind {
            ExecutableKind::App => {
                if self.includes.is_some() {
                    bail!(
                        "[[executables]] kind = \"app\" cannot set `includes` \
                         (it is a worker-only field)"
                    );
                }
                if self.runtime.is_some() && self.entrypoint.is_none() {
                    bail!(
                        "[[executables]] kind = \"app\" sets `runtime` but no `entrypoint` — \
                         the v2 app manifest needs both (e.g. entrypoint = \"index.html\")"
                    );
                }
                if self.runtime.is_none() && self.entrypoint.is_some() {
                    bail!(
                        "[[executables]] kind = \"app\" sets `entrypoint` but no `runtime` — \
                         add runtime = \"web\" for a v2 app manifest, or drop `entrypoint` for v1"
                    );
                }
            }
            ExecutableKind::Worker => {
                if self.runtime.is_some() {
                    bail!(
                        "[[executables]] kind = \"worker\" cannot set `runtime` \
                         (it is an app-only field)"
                    );
                }
                if self.entrypoint.as_deref().unwrap_or_default().is_empty() {
                    bail!(
                        "[[executables]] kind = \"worker\" needs an `entrypoint` \
                         (e.g. entrypoint = \"index.js\")"
                    );
                }
            }
        }
        Ok(())
    }
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

        let mut seen: Vec<ExecutableKind> = Vec::new();
        for executable in &self.executables {
            executable.validate()?;
            if seen.contains(&executable.kind) {
                bail!(
                    "deploy config declares two [[executables]] with kind = \"{}\" — \
                     the kind is the subdomain label, so they would overwrite each other",
                    executable.label()
                );
            }
            seen.push(executable.kind);
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

    /// The `executable` record shapes are pinned against what is actually
    /// deployed on paseo-next-v2 today (read from `DotnsContentResolver.text`
    /// for `worker.jollity.paseo`, 2026-09-10):
    ///   {"$v":1,"kind":"worker","appVersion":[0,1,0],"entrypoint":"index.js",
    ///    "includes":{"chat":true,"pocket":false}}
    /// Key order is part of the record, so this asserts the exact string.
    #[test]
    fn worker_executable_record_matches_the_live_shape() {
        let config = parse(
            "[[executables]]\nkind = \"worker\"\npath = \"dist/worker\"\n\
             app_version = [0, 1, 0]\nentrypoint = \"index.js\"\n\
             includes = { chat = true, pocket = false }\n",
        )
        .unwrap();
        assert_eq!(
            config.executables[0].executable_json().unwrap(),
            r#"{"$v":1,"kind":"worker","appVersion":[0,1,0],"entrypoint":"index.js","includes":{"chat":true,"pocket":false}}"#
        );
        assert!(!config.executables[0].embeds_manifest());
        assert_eq!(config.executables[0].label(), "worker");
    }

    #[test]
    fn app_v2_record_declares_its_runtime_and_is_embedded() {
        let config = parse(
            "[[executables]]\nkind = \"app\"\npath = \"dist/app\"\n\
             app_version = [0, 0, 1]\nruntime = \"web\"\nentrypoint = \"index.html\"\n",
        )
        .unwrap();
        assert_eq!(
            config.executables[0].executable_json().unwrap(),
            r#"{"$v":2,"kind":"app","appVersion":[0,0,1],"runtime":{"kind":"web","entrypoint":"index.html"}}"#
        );
        // v2 manifests go into the build dir before merkleization.
        assert!(config.executables[0].embeds_manifest());
    }

    /// An app with no runtime is the v1 shape observed on `app.jollity.paseo`:
    /// {"$v":1,"kind":"app","appVersion":[0,1,0]} — and nothing is embedded.
    #[test]
    fn app_without_runtime_is_the_v1_record() {
        let config = parse(
            "[[executables]]\nkind = \"app\"\npath = \"dist/app\"\napp_version = [0, 1, 0]\n",
        )
        .unwrap();
        assert_eq!(
            config.executables[0].executable_json().unwrap(),
            r#"{"$v":1,"kind":"app","appVersion":[0,1,0]}"#
        );
        assert!(!config.executables[0].embeds_manifest());
    }

    #[test]
    fn worker_without_entrypoint_rejected() {
        let err = parse(
            "[[executables]]\nkind = \"worker\"\npath = \"dist/worker\"\napp_version = [0, 0, 1]\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("entrypoint"), "{err}");
    }

    #[test]
    fn app_runtime_without_entrypoint_rejected() {
        let err = parse(
            "[[executables]]\nkind = \"app\"\npath = \"dist/app\"\n\
             app_version = [0, 0, 1]\nruntime = \"web\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("entrypoint"), "{err}");
    }

    #[test]
    fn worker_only_and_app_only_fields_are_not_mixed() {
        let app_includes = parse(
            "[[executables]]\nkind = \"app\"\npath = \"d\"\napp_version = [0,0,1]\n\
             runtime = \"web\"\nentrypoint = \"i.html\"\nincludes = { chat = true }\n",
        )
        .unwrap_err()
        .to_string();
        assert!(app_includes.contains("includes"), "{app_includes}");

        let worker_runtime = parse(
            "[[executables]]\nkind = \"worker\"\npath = \"d\"\napp_version = [0,0,1]\n\
             entrypoint = \"i.js\"\nruntime = \"web\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(worker_runtime.contains("runtime"), "{worker_runtime}");
    }

    /// The kind is the subdomain label, so two entries of one kind would race
    /// each other onto the same node.
    #[test]
    fn duplicate_executable_kinds_rejected() {
        let err = parse(
            "[[executables]]\nkind = \"app\"\npath = \"a\"\napp_version = [0,0,1]\n\
             [[executables]]\nkind = \"app\"\npath = \"b\"\napp_version = [0,0,1]\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("subdomain label"), "{err}");
    }

    #[test]
    fn unknown_executable_kind_rejected() {
        assert!(parse(
            "[[executables]]\nkind = \"sidecar\"\npath = \"d\"\napp_version = [0,0,1]\n"
        )
        .is_err());
    }

    /// A website-only config must keep parsing with no executables at all.
    #[test]
    fn config_without_executables_still_parses() {
        let config = parse("[text]\nmanifest = \"x\"\n").unwrap();
        assert!(config.executables.is_empty());
    }

    /// Executable paths resolve against the config's directory, not the cwd, so
    /// `dotkit deploy` can be run from anywhere.
    #[test]
    fn executable_paths_resolve_against_the_config_dir() {
        let dir = std::env::temp_dir().join(format!("dotkit-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deploy.toml");
        std::fs::write(
            &path,
            "[[executables]]\nkind = \"worker\"\npath = \"dist/worker\"\n\
             app_version = [0, 0, 1]\nentrypoint = \"index.js\"\n",
        )
        .unwrap();

        let config = DeployConfig::load(Some(path.to_str().unwrap())).unwrap();
        assert_eq!(config.base_dir, dir);
        assert_eq!(
            config.executables[0].dir(&config.base_dir),
            dir.join("dist/worker")
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
