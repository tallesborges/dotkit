use crate::bulletin;
use crate::car;
use crate::chain;
use crate::config::DeployConfig;
use crate::dotns;
use crate::env::Env;
use crate::merkle;
use crate::ui;
use anyhow::{bail, Context, Result};
use cid::Cid;
use clap::Args as ClapArgs;
use std::process::Command;

#[derive(ClapArgs)]
pub struct Args {
    /// Build directory to deploy (e.g. ./dist).
    pub dir: String,
    /// Target DotNS domain (e.g. myapp00.paseo on paseo-next-v2). The env's TLD
    /// is appended when omitted.
    pub domain: String,
    /// Deploy a pre-built CAR instead of merkleizing the directory.
    #[arg(long)]
    pub input_car: Option<String>,
    /// Merkleize with the Kubo `ipfs` binary instead of the native encoder (fallback).
    #[arg(long)]
    pub kubo: bool,
    /// Deploy manifest: text records + optional [product] metadata to write
    /// (defaults to ./deploy.toml if present).
    #[arg(long)]
    pub config: Option<String>,
    /// Register the domain (open-tier) if it isn't already owned by the signer.
    #[arg(long)]
    pub register: bool,
    /// After deploy, list the domain in Browse via the Publisher registry
    /// (signer must own the label).
    #[arg(long)]
    pub publish: bool,
    /// Make a `--publish` failure hard-fail the command (default: warn, exit 0).
    #[arg(long)]
    pub fail_on_publish_error: bool,
}

pub async fn run(
    env: &Env,
    args: Args,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
    pool_source: crate::pool::PoolSource,
) -> Result<()> {
    let domain = dotns::normalize_name(&args.domain, &env.tld);
    let config = DeployConfig::load(args.config.as_deref())?;

    let owner = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
    let asset_hub = chain::asset_hub_client(env).await?;
    // Deploy touches ownership, optional registration and the contenthash bind,
    // so probe all three up front in one burst rather than failing partway
    // through an upload that can no longer be bound to the name.
    dotns::ensure_deployed(
        &asset_hub,
        env,
        &[
            dotns::Contract::Registry,
            dotns::Contract::RegistrarController,
            dotns::Contract::ContentResolver,
        ],
    )
    .await?;
    dotns::ensure_domain(&asset_hub, env, &owner, &domain, args.register).await?;

    let (content_cid, prepared) = match &args.input_car {
        Some(car) => {
            ui::step(format!("read CAR {car}"));
            bulletin::read_car_prepared(car).await?
        }
        None if args.kubo => {
            require_ipfs()?;
            ui::step(format!("merkleize {} (kubo)", args.dir));
            let cid = merkleize_kubo(&args.dir)?;
            let tmp = TempCar::for_cid(&cid);
            export_car(&cid, tmp.path())?;
            bulletin::read_car_prepared(tmp.path()).await?
        }
        None => {
            ui::step(format!("merkleize {}", args.dir));
            let m = merkle::merkleize_dir(&args.dir)?;
            (m.root, m.blocks)
        }
    };
    ui::kv("content", content_cid);

    // Read and package everything bound for Bulletin before uploading any of it,
    // so a bad icon or executable fails before the first transaction.
    let icon = match &config.product {
        Some(product) => {
            let icon_path = product.icon_path(&config.base_dir);
            let icon_bytes = std::fs::read(&icon_path)
                .with_context(|| format!("reading [product] icon {}", icon_path.display()))?;
            if icon_bytes.len() > bulletin::MAX_TRANSACTION_SIZE {
                bail!(
                    "[product] icon {} is {} bytes, exceeding the chain's MaxTransactionSize of {} bytes (2 MiB)",
                    icon_path.display(),
                    icon_bytes.len(),
                    bulletin::MAX_TRANSACTION_SIZE
                );
            }
            let hashing = bulletin::Hashing::Blake2b256;
            let cid = hashing.cid(0x55, &icon_bytes);
            Some((
                product,
                cid,
                bulletin::PreparedBlock {
                    codec: 0x55,
                    hashing,
                    content_hash: hashing.content_hash(&icon_bytes),
                    data: icon_bytes,
                },
            ))
        }
        None => None,
    };
    let mut packaged = Vec::new();
    for executable in &config.executables {
        packaged.push(package_executable(executable, &config.base_dir).await?);
    }

    // Upload everything, then wait for finality. No DotNS record is written
    // until every block it can reference is stored in a finalized block.
    let client = bulletin::bulletin_client(env).await?;
    let rpc = env.bulletin_rpc()?;
    let pool = bulletin::acquire_pool_signer(&client, env, pool_source).await?;
    ui::step("upload to Bulletin");
    let stored =
        bulletin::store_prepared_blocks(env, &client, content_cid, prepared, &pool).await?;
    ui::kv(
        "blocks",
        format!(
            "{} stored · {} skipped · {} total",
            stored.stored,
            stored.skipped,
            stored.stored + stored.skipped
        ),
    );
    let mut required = stored.content_hashes.clone();

    if let Some((product, cid, block)) = &icon {
        ui::step(format!("upload icon {}", ui::ellipsize(&cid.to_string())));
        bulletin::store_blocks(
            &client,
            rpc,
            &pool,
            std::slice::from_ref(block),
            |_, _, _| {},
        )
        .await?;
        required.push(block.content_hash);
        ui::kv("icon", format!("{cid} ({})", product.icon_format()?));
    }

    for executable in &mut packaged {
        ui::step(format!("upload {} to Bulletin", executable.kind));
        let blocks = std::mem::take(&mut executable.blocks);
        let stored =
            bulletin::store_prepared_blocks(env, &client, executable.root, blocks, &pool).await?;
        ui::kv(
            "blocks",
            format!(
                "{} stored · {} skipped · {} total",
                stored.stored,
                stored.skipped,
                stored.stored + stored.skipped
            ),
        );
        required.extend(stored.content_hashes);
    }

    ui::step("confirm finalized");
    bulletin::confirm_finalized(&client, rpc, &required).await?;
    super::bulletin::settle_signer(&client, &pool).await;
    drop(pool);
    ui::kv(
        "gateway",
        format!("{}/ipfs/{content_cid}/", env.ipfs_gateway),
    );

    ui::step(format!(
        "bind {domain} → {}",
        ui::ellipsize(&content_cid.to_string())
    ));
    let expected = dotns::set_contenthash(&asset_hub, env, &owner, &domain, &content_cid).await?;
    let onchain = dotns::resolve_contenthash(&asset_hub, env, &domain).await?;
    if onchain != expected {
        bail!(
            "read-back mismatch: set 0x{} but chain has 0x{}",
            hex::encode(&expected),
            hex::encode(&onchain)
        );
    }

    let icon_cid = icon.as_ref().map(|(_, cid, _)| *cid);
    if let (Some(product), Some(cid)) = (&config.product, icon_cid) {
        let manifest = product.root_manifest_json(&cid, dotns::product_label(&domain, &env.tld))?;
        ui::step(format!("set 'manifest' on {domain}"));
        dotns::set_text(&asset_hub, env, &owner, &domain, "manifest", &manifest).await?;
        ui::kv("manifest", ui::ellipsize(&manifest));
    }

    for (key, value) in &config.text {
        ui::step(format!("set '{key}' on {domain}"));
        dotns::set_text(&asset_hub, env, &owner, &domain, key, value).await?;
        ui::kv(key, ui::ellipsize(value));
    }

    let mut executables = Vec::new();
    for executable in packaged {
        executables.push(bind_executable(env, &asset_hub, &owner, &domain, executable).await?);
    }

    let mut published = false;
    if args.publish {
        ui::step(format!("publish {domain} to Browse"));
        match crate::publisher::publish(env, &owner, &domain).await {
            Ok(outcome) => {
                ui::kv("tx", format!("0x{}", hex::encode(outcome.tx)));
                published = true;
            }
            Err(err) if args.fail_on_publish_error => {
                return Err(err.context("publish failed"));
            }
            Err(err) => ui::note(format!("publish failed (non-fatal): {err}")),
        }
    }

    let label = dotns::strip_tld(&domain, &env.tld);
    let url = (!env.web_gateway.is_empty()).then(|| format!("https://{label}.{}", env.web_gateway));
    if ui::json() {
        ui::emit(&serde_json::json!({
            "domain": domain,
            "content": content_cid.to_string(),
            "url": url,
            "published": published,
            "icon": icon_cid.map(|c| c.to_string()),
            "manifest": icon_cid.is_some(),
            "blocks": { "stored": stored.stored, "skipped": stored.skipped },
            "executables": executables.iter().map(|e| serde_json::json!({
                "kind": e.kind,
                "domain": e.domain,
                "content": e.root.to_string(),
                "car_bytes": e.car_len,
                "chunks": e.chunks,
                "record": e.record,
                "embedded_manifest": e.embedded_manifest,
                "unchanged": e.unchanged,
            })).collect::<Vec<_>>(),
        }));
    } else {
        println!();
        ui::success(format!("deployed {domain}"));
        ui::kv("content", content_cid);
        if let Some(cid) = icon_cid {
            ui::kv("icon", cid);
            ui::kv("manifest", "written");
        }
        if let Some(url) = url {
            ui::kv("url", url);
        }
        if published {
            ui::kv("browse", "published");
        }
        for executable in &executables {
            ui::kv(
                &executable.domain,
                format!(
                    "{} · {} ({} chunk{} of {} CAR bytes){}",
                    executable.kind,
                    ui::ellipsize(&executable.root.to_string()),
                    executable.chunks,
                    if executable.chunks == 1 { "" } else { "s" },
                    executable.car_len,
                    if executable.unchanged {
                        " · unchanged"
                    } else {
                        ""
                    }
                ),
            );
        }
    }
    Ok(())
}

/// A published executable, for the deploy summary.
struct PublishedExecutable {
    kind: &'static str,
    domain: String,
    root: Cid,
    car_len: usize,
    chunks: usize,
    record: String,
    embedded_manifest: bool,
    /// `true` when the chain already held this exact executable, so nothing was
    /// written for it.
    unchanged: bool,
}

/// An executable packaged for upload, then bound once its blocks are final.
struct PackagedExecutable {
    kind: &'static str,
    record: String,
    root: Cid,
    blocks: Vec<bulletin::PreparedBlock>,
    car_len: usize,
    chunks: usize,
    embedded_manifest: bool,
}

/// Package one executable for `<kind>.<domain>`.
///
/// Executables use a different content model from the website root: the build
/// directory's DAG is serialized to a CARv1 archive and that archive is stored
/// **as a chunked file**, so the bound CID is the archive's file root rather
/// than a browsable directory (see [`crate::car`]). Only the chunks and the
/// file root reach Bulletin; the inner directory blocks ride inside the archive.
async fn package_executable(
    executable: &crate::config::ExecutableConfig,
    base_dir: &std::path::Path,
) -> Result<PackagedExecutable> {
    let kind = executable.label();
    let dir = executable.dir(base_dir);
    let dir_str = dir
        .to_str()
        .with_context(|| format!("[[executables]] path {} is not valid UTF-8", dir.display()))?;
    if !dir.is_dir() {
        bail!(
            "[[executables]] kind = \"{kind}\" path {} is not a directory",
            dir.display()
        );
    }

    let record = executable.executable_json()?;

    println!();
    ui::step(format!("package {kind} from {}", dir.display()));
    // The v2 app manifest is part of the content, so it has to be in the DAG
    // before merkleization — injecting it in memory keeps the caller's build
    // output untouched while producing the same CID as a file on disk.
    let injected = if executable.embeds_manifest() {
        ui::kv("embed", "manifest.json (App v2)");
        vec![("manifest.json".to_string(), record.clone().into_bytes())]
    } else {
        Vec::new()
    };
    let merkleized = merkle::merkleize_dir_with(dir_str, &injected)?;
    ui::kv("inner root", ui::ellipsize(&merkleized.root.to_string()));

    let car = car::car_bytes(&merkleized.root, &merkleized.blocks).await?;
    let packaged = car::chunked_file(&car)?;
    ui::kv(
        "car",
        format!(
            "{} bytes · {} chunk{}",
            packaged.car_len,
            packaged.chunks,
            if packaged.chunks == 1 { "" } else { "s" }
        ),
    );
    ui::kv("content", packaged.root);

    Ok(PackagedExecutable {
        kind,
        record,
        root: packaged.root,
        blocks: packaged.blocks,
        car_len: packaged.car_len,
        chunks: packaged.chunks,
        embedded_manifest: executable.embeds_manifest(),
    })
}

/// Bind an uploaded executable to `<kind>.<domain>`.
///
/// Chain writes go out as two atomic `Utility.batch_all` groups —
/// `setSubnodeOwner` + `setResolver`, then `setText("executable")` +
/// `setContenthash` — so a consumer never sees a subnode with no resolver, or a
/// contenthash with no record describing how to run it. Both groups are skipped
/// when the chain already holds the wanted state, which makes re-running a
/// deploy after a partial failure (or with only one executable changed) cheap.
async fn bind_executable(
    env: &Env,
    asset_hub: &subxt::OnlineClient<crate::chain::config::AssetHubConfig>,
    owner: &subxt_signer::sr25519::Keypair,
    domain: &str,
    executable: PackagedExecutable,
) -> Result<PublishedExecutable> {
    let kind = executable.kind;
    ui::step(format!("bind {kind}.{domain}"));
    let (subdomain, subnode) =
        dotns::ensure_subnode_with_resolver(asset_hub, env, owner, domain, kind).await?;
    if subnode.unchanged {
        ui::kv("subnode", format!("{subdomain} · already owned + resolved"));
    }

    let records = dotns::set_executable_records(
        asset_hub,
        env,
        owner,
        &subdomain,
        &executable.record,
        &executable.root,
    )
    .await?;
    if records.unchanged {
        ui::kv("records", "unchanged · nothing written");
    } else {
        ui::kv("executable", ui::ellipsize(&executable.record));
    }

    Ok(PublishedExecutable {
        kind,
        domain: subdomain,
        root: executable.root,
        car_len: executable.car_len,
        chunks: executable.chunks,
        record: executable.record,
        embedded_manifest: executable.embedded_manifest,
        unchanged: subnode.unchanged && records.unchanged,
    })
}

fn require_ipfs() -> Result<()> {
    Command::new("ipfs")
        .arg("--version")
        .output()
        .context("`ipfs` (Kubo) not found on PATH; drop --kubo to use the native encoder")?;
    Ok(())
}

/// Merkleize a directory with Kubo into a CIDv1 (raw leaves, unpinned) without
/// adding it to the local pinset — just to compute the content DAG + root CID.
fn merkleize_kubo(dir: &str) -> Result<Cid> {
    let out = Command::new("ipfs")
        .args([
            "add",
            "-Q",
            "-r",
            "--hidden",
            "--cid-version=1",
            "--raw-leaves",
            "--pin=false",
            dir,
        ])
        .output()
        .context("running `ipfs add`")?;
    if !out.status.success() {
        bail!(
            "`ipfs add` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let cid_str = String::from_utf8(out.stdout)
        .context("`ipfs add` produced non-UTF8 output")?
        .trim()
        .to_string();
    Cid::try_from(cid_str.as_str()).with_context(|| format!("parsing content CID '{cid_str}'"))
}

/// Export a CID's full DAG to a CARv1 file via `ipfs dag export`. Captures
/// stderr so Kubo's progress bar doesn't leak into our output.
fn export_car(cid: &Cid, path: &str) -> Result<()> {
    let file = std::fs::File::create(path).with_context(|| format!("creating CAR file {path}"))?;
    let out = Command::new("ipfs")
        .args(["dag", "export", &cid.to_string()])
        .stdout(file)
        .output()
        .context("running `ipfs dag export`")?;
    if !out.status.success() {
        bail!(
            "`ipfs dag export {cid}` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// A temp CAR file removed on drop.
struct TempCar(std::path::PathBuf);

impl TempCar {
    fn for_cid(cid: &Cid) -> Self {
        TempCar(std::env::temp_dir().join(format!("dotkit-deploy-{cid}.car")))
    }

    fn path(&self) -> &str {
        self.0.to_str().expect("temp path is valid UTF-8")
    }
}

impl Drop for TempCar {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
