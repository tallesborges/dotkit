use crate::dotns;
use crate::env::Env;
use anyhow::{bail, Context, Result};
use cid::Cid;
use multihash_codetable::{Code, MultihashDigest};
use scale_info::PortableRegistry;
use std::str::FromStr;
use subxt::utils::{AccountId32, H160};
use subxt::{OnlineClient, PolkadotConfig};
use subxt_signer::{sr25519::Keypair, SecretUri};

#[subxt::subxt(runtime_metadata_path = "artifacts/paseo_next_v2_asset_hub.scale")]
pub mod asset_hub {}

#[subxt::subxt(runtime_metadata_path = "artifacts/paseo_next_v2_bulletin.scale")]
pub mod bulletin {}

/// The Bulletin chain declares three custom, empty transaction extensions on top
/// of the usual Substrate ones — `AuthorizeCall`, `ValidateStorageCalls` and
/// `AllowanceBasedPriority` — plus `CheckNonZeroSender`, `CheckWeight` and
/// `StorageWeightReclaim`, none of which subxt's `PolkadotConfig` provides.
/// Signing therefore needs a bespoke [`Config`] whose `TransactionExtensions`
/// tuple covers every extension the runtime lists, in declared order. Each of the
/// extensions below encodes nothing for both the value and the implicit payload.
macro_rules! empty_extension {
    ($ext:ident, $name:literal) => {
        pub struct $ext;

        impl<T: subxt::Config> subxt::config::TransactionExtension<T> for $ext {
            type Decoded = ();
            type Params = ();

            fn new(
                _client: &subxt::config::ClientState<T>,
                _params: Self::Params,
            ) -> core::result::Result<Self, subxt::error::TransactionExtensionError> {
                Ok($ext)
            }
        }

        impl subxt::ext::frame_decode::extrinsics::TransactionExtension<PortableRegistry> for $ext {
            const NAME: &str = $name;

            fn encode_value_to(
                &self,
                _type_id: u32,
                _type_resolver: &PortableRegistry,
                _out: &mut Vec<u8>,
            ) -> core::result::Result<
                (),
                subxt::ext::frame_decode::extrinsics::TransactionExtensionError,
            > {
                Ok(())
            }

            fn encode_implicit_to(
                &self,
                _type_id: u32,
                _type_resolver: &PortableRegistry,
                _out: &mut Vec<u8>,
            ) -> core::result::Result<
                (),
                subxt::ext::frame_decode::extrinsics::TransactionExtensionError,
            > {
                Ok(())
            }
        }
    };
}

empty_extension!(AuthorizeCall, "AuthorizeCall");
empty_extension!(CheckNonZeroSender, "CheckNonZeroSender");
empty_extension!(CheckWeight, "CheckWeight");
empty_extension!(ValidateStorageCalls, "ValidateStorageCalls");
empty_extension!(AllowanceBasedPriority, "AllowanceBasedPriority");
empty_extension!(StorageWeightReclaim, "StorageWeightReclaim");

use subxt::config::transaction_extensions as tx_ext;

type BulletinTxExtensions = (
    AuthorizeCall,
    CheckNonZeroSender,
    tx_ext::CheckSpecVersion,
    tx_ext::CheckTxVersion,
    tx_ext::CheckGenesis<BulletinConfig>,
    tx_ext::CheckMortality<BulletinConfig>,
    tx_ext::CheckNonce,
    CheckWeight,
    tx_ext::ChargeTransactionPayment,
    ValidateStorageCalls,
    AllowanceBasedPriority,
    tx_ext::CheckMetadataHash,
    StorageWeightReclaim,
);

/// subxt [`Config`] for the Bulletin chain. Account/address/signature/hashing all
/// match a standard Substrate chain; only the transaction-extension set differs.
/// All other config hooks fall back to their defaults, so the `OnlineClient`
/// fetches genesis hash, runtime version and metadata straight from the node.
#[derive(Debug, Clone, Default)]
pub struct BulletinConfig;

impl subxt::Config for BulletinConfig {
    type AccountId = AccountId32;
    type Address = subxt::utils::MultiAddress<AccountId32, ()>;
    type Signature = subxt::utils::MultiSignature;
    type Hasher = <PolkadotConfig as subxt::Config>::Hasher;
    type Header = <PolkadotConfig as subxt::Config>::Header;
    type AssetId = u32;
    type TransactionExtensions = BulletinTxExtensions;
}

/// CIDv1 (raw codec `0x55`, sha2-256 multihash) of a blob's bytes — the CID the
/// Bulletin chain assigns to data stored via `store_with_cid_config`.
pub fn raw_cid(data: &[u8]) -> Cid {
    Cid::new_v1(0x55, Code::Sha2_256.digest(data))
}

/// sha2-256 of a blob's bytes; this is the key the chain uses in
/// `TransactionStorage.TransactionByContentHash`.
pub fn content_hash(data: &[u8]) -> [u8; 32] {
    let digest = Code::Sha2_256.digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.digest());
    out
}

/// Result of storing a single IPLD block via [`store_block`].
pub enum StoreOutcome {
    Stored { block: u32, index: u32 },
    AlreadyPresent { block: u32, index: u32 },
}

/// Store one blob (an IPLD block) on the Bulletin chain under its own content
/// hash, using the block's `codec` and sha2-256. Idempotent: if the block is
/// already stored (keyed by `sha256(data)` in `TransactionByContentHash`) it
/// returns [`StoreOutcome::AlreadyPresent`] without submitting. `data` must be
/// no larger than the chain's `MaxTransactionSize`; callers guard that.
pub async fn store_block(
    client: &OnlineClient<BulletinConfig>,
    signer: &Keypair,
    codec: u64,
    data: &[u8],
) -> Result<StoreOutcome> {
    let content_hash = content_hash(data);

    let at = client.at_current_block().await?;
    let existing = at
        .storage()
        .try_fetch(
            bulletin::storage()
                .transaction_storage()
                .transaction_by_content_hash(),
            (content_hash,),
        )
        .await
        .context("reading TransactionStorage.TransactionByContentHash")?;
    if let Some(value) = existing {
        let (block, index) = value.decode().context("decoding stored location")?;
        return Ok(StoreOutcome::AlreadyPresent { block, index });
    }
    drop(at);

    let cid_config =
        bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::CidConfig {
            codec,
            hashing:
                bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::HashingAlgorithm::Sha2_256,
        };
    let call = bulletin::tx()
        .transaction_storage()
        .store_with_cid_config(cid_config, data.to_vec());

    client
        .tx()
        .await?
        .sign_and_submit_then_watch_default(&call, signer)
        .await
        .context("submitting store_with_cid_config")?
        .wait_for_finalized_success()
        .await
        .context("store_with_cid_config did not finalize successfully")?;

    let at = client.at_current_block().await?;
    let (block, index) = at
        .storage()
        .try_fetch(
            bulletin::storage()
                .transaction_storage()
                .transaction_by_content_hash(),
            (content_hash,),
        )
        .await
        .context("re-reading TransactionByContentHash after store")?
        .context("store finalized but TransactionByContentHash is still empty")?
        .decode()
        .context("decoding stored location")?;
    Ok(StoreOutcome::Stored { block, index })
}

/// Build an sr25519 signer from a mnemonic (+ optional Substrate derivation path).
/// When no mnemonic is supplied, fall back to the `//Alice` dev key so the demo
/// works out of the box. The mnemonic is never logged.
pub fn build_signer(mnemonic: Option<&str>, derivation_path: Option<&str>) -> Result<Keypair> {
    match mnemonic {
        Some(phrase) => {
            let suffix = derivation_path.unwrap_or("");
            let uri = SecretUri::from_str(&format!("{phrase}{suffix}"))
                .context("failed to parse mnemonic + derivation path")?;
            Keypair::from_uri(&uri).context("failed to derive sr25519 keypair")
        }
        None => Ok(subxt_signer::sr25519::dev::alice()),
    }
}

pub fn account_id(signer: &Keypair) -> AccountId32 {
    AccountId32(signer.public_key().0)
}

/// Resolve the H160 (EVM) address for an account via the `ReviveApi.address`
/// runtime API on the env's Asset Hub.
pub async fn revive_address(env: &Env, account: AccountId32) -> Result<H160> {
    let client = OnlineClient::<PolkadotConfig>::from_url(env.asset_hub_rpc)
        .await
        .with_context(|| format!("connecting to Asset Hub RPC {}", env.asset_hub_rpc))?;
    let call = asset_hub::runtime_apis().revive_api().address(account);
    let h160 = client
        .at_current_block()
        .await?
        .runtime_apis()
        .call(call)
        .await
        .context("ReviveApi.address runtime call failed")?;
    Ok(h160)
}

/// Latest finalized block number for an RPC, used as a connectivity proof.
pub async fn latest_block_number(rpc: &str) -> Result<u64> {
    let client = OnlineClient::<PolkadotConfig>::from_url(rpc)
        .await
        .with_context(|| format!("connecting to RPC {rpc}"))?;
    Ok(client.at_current_block().await?.block_number())
}

fn parse_h160(addr: &str) -> Result<H160> {
    let raw = addr.strip_prefix("0x").unwrap_or(addr);
    let bytes = hex::decode(raw).with_context(|| format!("invalid H160 hex '{addr}'"))?;
    let arr: [u8; 20] = bytes
        .as_slice()
        .try_into()
        .with_context(|| format!("expected 20-byte H160, got {} bytes", bytes.len()))?;
    Ok(H160(arr))
}

/// Read a `.dot` name's raw DotNS contenthash bytes (EIP-1577, e.g. `0xe301…`)
/// by dry-running the resolver's `contenthash(bytes32)` view via `ReviveApi.call`.
/// Returns empty when no contenthash is set. `name` must be normalized already.
pub async fn resolve_contenthash(env: &Env, name: &str) -> Result<Vec<u8>> {
    let node = dotns::namehash(name);
    let input_data = dotns::encode_contenthash_call(node);
    let dest = parse_h160(env.dotns_content_resolver)?;
    let origin = account_id(&build_signer(None, None)?);

    let client = OnlineClient::<PolkadotConfig>::from_url(env.asset_hub_rpc)
        .await
        .with_context(|| format!("connecting to Asset Hub RPC {}", env.asset_hub_rpc))?;
    let call = asset_hub::runtime_apis()
        .revive_api()
        .call(origin, dest, 0, None, None, input_data);
    let result = client
        .at_current_block()
        .await?
        .runtime_apis()
        .call(call)
        .await
        .context("ReviveApi.call runtime call failed")?;

    let exec = match result.result {
        Ok(exec) => exec,
        Err(err) => bail!("resolver call failed on chain: {err:?}"),
    };
    if exec.flags.bits & 1 != 0 {
        bail!("resolver call reverted");
    }
    dotns::decode_contenthash_return(&exec.data)
}
