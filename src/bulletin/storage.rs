//! Bulletin chain `TransactionStorage`: content-addressed block storage
//! (idempotent single-block and concurrent CAR-batch uploads), authorization,
//! and the CID / content-hash helpers the upload layer keys blocks by.

use crate::bulletin::signer::UploadSigner;
use crate::chain::config::{bulletin, BulletinConfig};
use crate::chain::metadata::connect_with_cache;
use crate::env::Env;
use anyhow::{bail, Context, Result};
use cid::Cid;
use futures::StreamExt;
use multihash_codetable::{Code, MultihashDigest};
use std::collections::HashSet;
use std::time::Duration;
use subxt::config::transaction_extensions as tx_ext;
use subxt::utils::AccountId32;
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

/// Chain-enforced `MaxTransactionSize` (2 MiB) — the largest blob one store
/// extrinsic can carry.
pub const MAX_TRANSACTION_SIZE: usize = 2 * 1024 * 1024;

/// Multihash algorithm a blob is stored under. App content uses [`Sha2_256`]
/// (Kubo's default, byte-exact with native merkleization); product icons use
/// [`Blake2b256`], which the host's Browse / preimage icon resolver requires —
/// a `sha2-256` icon CID resolves on the IPFS gateway but not in Browse.
///
/// [`Sha2_256`]: Hashing::Sha2_256
/// [`Blake2b256`]: Hashing::Blake2b256
#[derive(Clone, Copy)]
pub enum Hashing {
    Sha2_256,
    Blake2b256,
}

impl Hashing {
    fn code(self) -> Code {
        match self {
            Hashing::Sha2_256 => Code::Sha2_256,
            Hashing::Blake2b256 => Code::Blake2b256,
        }
    }

    /// The runtime `HashingAlgorithm` variant passed in a `CidConfig`.
    fn runtime(
        self,
    ) -> bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::HashingAlgorithm
    {
        use bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::HashingAlgorithm as H;
        match self {
            Hashing::Sha2_256 => H::Sha2_256,
            Hashing::Blake2b256 => H::Blake2b256,
        }
    }

    /// CIDv1 for `data` under `codec` using this algorithm — the CID the Bulletin
    /// chain assigns to data stored via `store_with_cid_config`.
    pub fn cid(self, codec: u64, data: &[u8]) -> Cid {
        Cid::new_v1(codec, self.code().digest(data))
    }

    /// The 32-byte digest the chain keys `TransactionByContentHash` by.
    pub fn content_hash(self, data: &[u8]) -> [u8; 32] {
        let digest = self.code().digest(data);
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.digest());
        out
    }
}

/// CIDv1 (raw codec `0x55`, sha2-256 multihash) of a blob's bytes — the CID the
/// Bulletin chain assigns to data stored via `store_with_cid_config`.
pub fn raw_cid(data: &[u8]) -> Cid {
    Hashing::Sha2_256.cid(0x55, data)
}

/// sha2-256 of a blob's bytes; this is the key the chain uses in
/// `TransactionStorage.TransactionByContentHash`.
pub fn content_hash(data: &[u8]) -> [u8; 32] {
    Hashing::Sha2_256.content_hash(data)
}

/// Result of storing a single IPLD block via [`store_block`].
pub enum StoreOutcome {
    Stored { block: u32, index: u32 },
    AlreadyPresent { block: u32, index: u32 },
}

/// A block ready to upload: its IPLD `codec`, raw `data`, the [`Hashing`] the
/// chain keys it by, and that `content_hash` (the `TransactionByContentHash` key).
pub struct PreparedBlock {
    pub codec: u64,
    pub hashing: Hashing,
    pub data: Vec<u8>,
    pub content_hash: [u8; 32],
}

/// The `BulletinConfig` transaction-extension params, one slot per extension in
/// [`BulletinTxExtensions`]. Only `CheckMortality`, `CheckNonce` and
/// `ChargeTransactionPayment` take a non-`()` value; the rest are empty.
type StoreParams = (
    (),
    (),
    (),
    (),
    (),
    tx_ext::CheckMortalityParams<BulletinConfig>,
    tx_ext::CheckNonceParams,
    (),
    tx_ext::ChargeTransactionPaymentParams,
    (),
    (),
    (),
    (),
);

/// Blocks a signed store extrinsic stays valid for. A power of two no larger
/// than 4096, so the era's birth block is exactly the checkpoint block (no phase
/// quantization) and the checkpoint hash we sign is the one the runtime checks.
/// About 6.4 minutes of ~6s blocks: long enough for a round, short enough that
/// a stuck transaction stops mattering soon.
pub const ERA_PERIOD: u64 = 64;

/// The era checkpoint for a round: the block both the nonce and the
/// mortality are read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    pub number: u64,
    pub hash: subxt::utils::H256,
}

impl Checkpoint {
    /// First block number that can no longer include a transaction signed
    /// against this checkpoint.
    pub fn valid_until(self) -> u64 {
        self.number + ERA_PERIOD
    }
}

/// Params for a store extrinsic pinned to an explicit `nonce`, mortal from
/// `checkpoint` for [`ERA_PERIOD`] blocks, and without a tip
/// (`AllowanceBasedPriority` gives every store call the same max priority).
fn store_params(nonce: u64, checkpoint: Checkpoint) -> StoreParams {
    (
        (),
        (),
        (),
        (),
        (),
        tx_ext::CheckMortalityParams::mortal_from_unchecked(
            ERA_PERIOD,
            checkpoint.number,
            checkpoint.hash,
        ),
        tx_ext::CheckNonceParams::with_nonce(nonce),
        (),
        tx_ext::ChargeTransactionPaymentParams::no_tip(),
        (),
        (),
        (),
        (),
    )
}

async fn connect_bulletin(rpc_url: &str) -> Result<OnlineClient<BulletinConfig>> {
    connect_with_cache(rpc_url, |metadata_cache| BulletinConfig { metadata_cache }).await
}

/// Open a Bulletin client using the bespoke [`BulletinConfig`]. The client's
/// metadata cache is pre-seeded from the persistent on-disk cache, so an
/// unchanged runtime is served from disk with no metadata download.
pub async fn bulletin_client(env: &Env) -> Result<OnlineClient<BulletinConfig>> {
    connect_bulletin(env.bulletin_rpc()?).await
}

type AtBlock = subxt::client::OnlineClientAtBlock<BulletinConfig>;

/// Read `TransactionByContentHash` at `at` and decode the stored `(block, index)`
/// location, or `None` when the content hash isn't stored yet.
async fn stored_location(at: &AtBlock, content_hash: [u8; 32]) -> Result<Option<(u32, u32)>> {
    let existing = tokio::time::timeout(
        RPC_TIMEOUT,
        at.storage().try_fetch(
            bulletin::storage()
                .transaction_storage()
                .transaction_by_content_hash(),
            (content_hash,),
        ),
    )
    .await
    .context("timed out reading TransactionStorage.TransactionByContentHash")?
    .context("reading TransactionStorage.TransactionByContentHash")?;
    match existing {
        Some(value) => Ok(Some(value.decode().context("decoding stored location")?)),
        None => Ok(None),
    }
}

/// Which of `hashes` are stored at `at`, in order.
async fn stored_flags(at: &AtBlock, hashes: &[[u8; 32]]) -> Result<Vec<bool>> {
    let probes = hashes.iter().map(|hash| stored_location(at, *hash));
    futures::stream::iter(probes)
        .buffered(PROBE_CONCURRENCY)
        .map(|found| found.map(|found| found.is_some()))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect()
}

/// Build a signed store extrinsic for `block`, pinned to `nonce` and mortal
/// from `checkpoint`. Offline (no RPC).
fn build_store_submittable(
    tx_client: &subxt::tx::TransactionsClient<
        BulletinConfig,
        subxt::client::OnlineClientAtBlockImpl<BulletinConfig>,
    >,
    signer: &Keypair,
    block: &PreparedBlock,
    nonce: u64,
    checkpoint: Checkpoint,
) -> Result<
    subxt::tx::SubmittableTransaction<
        BulletinConfig,
        subxt::client::OnlineClientAtBlockImpl<BulletinConfig>,
    >,
> {
    let cid_config =
        bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::CidConfig {
            codec: block.codec,
            hashing: block.hashing.runtime(),
        };
    let call = bulletin::tx()
        .transaction_storage()
        .store_with_cid_config(cid_config, block.data.clone());
    tx_client
        .create_signable_offline(&call, store_params(nonce, checkpoint))
        .context(
            "building signed store extrinsic \
             (if this fails after a runtime upgrade the pinned metadata is stale — \
             regenerate artifacts/*.scale)",
        )?
        .sign(signer)
        .context("signing store extrinsic")
}

/// What the node said about one submitted store transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Submission {
    /// The node accepted it into its pool (or already held it).
    Accepted,
    /// The pool already holds another transaction for this nonce.
    Occupied,
    /// The node refused it, so it never entered the pool.
    Rejected(String),
    /// No answer we can trust (timeout or transport error). It may be pooled.
    Unknown(String),
}

impl Submission {
    /// Whether this submission may leave a transaction that can still be
    /// included: ours, or (for [`Submission::Occupied`]) the one holding our
    /// nonce.
    fn may_land(&self) -> bool {
        !matches!(self, Submission::Rejected(_))
    }
}

/// Match a transaction-pool answer by its wording. Covers both the
/// `transactionWatch_v1` (chainHead) event texts from polkadot-sdk
/// `rpc-spec-v2/src/transaction/error.rs` and the legacy `author_*` JSON-RPC
/// error texts. `None` when the text is not a known pool answer.
fn pool_answer(message: &str) -> Option<Submission> {
    let lower = message.to_lowercase();
    let has = |needle: &str| lower.contains(needle);
    if has("already imported") {
        Some(Submission::Accepted)
    } else if has("priority of the transaction is too low")
        || has("priority is too low")
        || has("too low priority")
    {
        Some(Submission::Occupied)
    } else if has("invalid transaction")
        || has("transaction is not valid")
        || has("transaction is outdated")
        || has("unknown transaction validity")
        || has("temporarily banned")
        || has("immediately dropped")
        || has("could not enter the pool")
        || has("not accepting future transactions")
        || has("verification error")
        || has("cyclic dependency")
    {
        Some(Submission::Rejected(message.to_string()))
    } else {
        None
    }
}

/// Classify a JSON-RPC submission error (the legacy backend's path). An
/// unrecognized error may be a transport failure after the node took the
/// transaction, so it stays [`Submission::Unknown`].
pub(crate) fn classify_rpc_error(message: &str) -> Submission {
    pool_answer(message).unwrap_or_else(|| Submission::Unknown(message.to_string()))
}

/// Classify the first transaction status when it is an error. Over chainHead
/// (`transactionWatch_v1`) the pool reports every import failure this way,
/// including "already imported" and a priority collision, so the text must be
/// matched. Any other terminal status is a node answer that the transaction is
/// not pooled, so it is [`Submission::Rejected`].
pub(crate) fn classify_status_error(error: &subxt::error::TransactionStatusError) -> Submission {
    use subxt::error::TransactionStatusError as E;
    let message = match error {
        E::Error(message) | E::Invalid(message) | E::Dropped(message) => message.clone(),
        other => other.to_string(),
    };
    pool_answer(&message).unwrap_or_else(|| Submission::Rejected(error.to_string()))
}

/// Submit `sub` and return once the node has answered for it (its first
/// status), without waiting for inclusion.
async fn submit_store(
    sub: subxt::tx::SubmittableTransaction<
        BulletinConfig,
        subxt::client::OnlineClientAtBlockImpl<BulletinConfig>,
    >,
) -> Submission {
    match tokio::time::timeout(FIRE_TIMEOUT, sub.submit()).await {
        Err(_) => Submission::Unknown(format!(
            "no answer from the node within {}s",
            FIRE_TIMEOUT.as_secs()
        )),
        Ok(Ok(_)) => Submission::Accepted,
        Ok(Err(subxt::error::ExtrinsicError::TransactionStatusError(e))) => {
            classify_status_error(&e)
        }
        Ok(Err(e)) => classify_rpc_error(&format!("{:#}", anyhow::Error::from(e))),
    }
}

/// How a confirmation round ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoundEnd {
    /// Every watched block is stored.
    Done,
    /// The best chain passed the last era that can include any live
    /// transaction from this upload.
    Expired,
    /// No submission from this upload can still be included.
    NothingPending,
    /// The best-block stream failed, so inclusion could not be observed.
    StreamFailed,
    /// No upload was confirmed for [`IDLE_TIMEOUT`].
    Idle,
}

/// The pure state of one confirmation round: when the live transactions die
/// and when progress was last seen. Time is passed in, so tests drive it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RoundWatch {
    /// First block that can include none of this upload's live transactions
    /// (this round's or an earlier round's). `None` when nothing is live.
    live_until: Option<u64>,
    last_progress: Duration,
}

impl RoundWatch {
    pub fn new(live_until: Option<u64>, now: Duration) -> Self {
        Self {
            live_until,
            last_progress: now,
        }
    }

    /// The end before watching any block, if there is nothing to watch for.
    pub fn start(&self) -> Option<RoundEnd> {
        self.live_until
            .is_none()
            .then_some(RoundEnd::NothingPending)
    }

    /// How long the round may wait for the next confirmation.
    pub fn idle_left(&self, now: Duration) -> Duration {
        IDLE_TIMEOUT.saturating_sub(now.saturating_sub(self.last_progress))
    }

    /// Account for best block `number`, after which `newly` more blocks are
    /// stored and `remaining` are still missing.
    pub fn on_block(
        &mut self,
        now: Duration,
        number: u64,
        newly: usize,
        remaining: usize,
    ) -> Option<RoundEnd> {
        if newly > 0 {
            self.last_progress = now;
        }
        if remaining == 0 {
            return Some(RoundEnd::Done);
        }
        // Block `live_until - 1` is the last that can include a live
        // transaction; once it is checked, none can land.
        match self.live_until {
            Some(until) if number + 1 >= until => Some(RoundEnd::Expired),
            _ => None,
        }
    }
}

/// The new `live_until` after a round signed against an era ending at
/// `valid_until` got `submissions` back.
pub(crate) fn extend_live_until(
    live_until: Option<u64>,
    valid_until: u64,
    submissions: &[Submission],
) -> Option<u64> {
    if submissions.iter().any(Submission::may_land) {
        Some(live_until.map_or(valid_until, |until| until.max(valid_until)))
    } else {
        live_until
    }
}

/// What to do after a round that did not store everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Sign the rest again from a fresh checkpoint and nonce.
    Retry,
    /// The next nonce the chain expects is held by a transaction the node
    /// accepted but does not include. New transactions would queue behind it.
    Stalled { head_nonce: u64 },
}

/// Decide the next step from how the round ended, the account nonce at the
/// best block, and this round's unconfirmed `(nonce, submission)` pairs.
pub(crate) fn round_verdict(
    end: RoundEnd,
    best_nonce: u64,
    unconfirmed: &[(u64, &Submission)],
) -> Verdict {
    if end != RoundEnd::Idle {
        return Verdict::Retry;
    }
    match unconfirmed.iter().find(|(nonce, _)| *nonce == best_nonce) {
        Some((nonce, Submission::Accepted | Submission::Occupied)) => {
            Verdict::Stalled { head_nonce: *nonce }
        }
        _ => Verdict::Retry,
    }
}

/// Counts rounds that stored nothing. A round that stores at least one block
/// shrinks the work, so only failed rounds spend the budget, and the total
/// number of rounds stays bounded by `batches + MAX_FAILED_ROUNDS`.
#[derive(Debug, Default)]
pub(crate) struct RoundBudget {
    failed: usize,
}

impl RoundBudget {
    /// Record a finished round that stored `stored` blocks. `false` when the
    /// budget is spent and the upload must stop.
    pub fn finish(&mut self, stored: usize) -> bool {
        if stored == 0 {
            self.failed += 1;
        }
        self.failed < MAX_FAILED_ROUNDS
    }
}

/// How many of the next blocks (by size, in order) one round signs: at most
/// [`BATCH_BYTES`] and [`BATCH_TXS`], and always at least one. Bounds the
/// signed extrinsics held in memory and keeps one round's submission time
/// small against its era.
pub(crate) fn batch_len(sizes: impl IntoIterator<Item = usize>) -> usize {
    let mut bytes = 0usize;
    let mut count = 0usize;
    for size in sizes {
        if count > 0 && (count == BATCH_TXS || bytes + size > BATCH_BYTES) {
            break;
        }
        bytes += size;
        count += 1;
    }
    count
}

/// Upper bound on store extrinsics submitted concurrently.
const UPLOAD_CONCURRENCY: usize = 20;
/// Upper bound on concurrent content-hash storage reads.
const PROBE_CONCURRENCY: usize = 64;
/// Rounds that store nothing before the upload gives up.
const MAX_FAILED_ROUNDS: usize = 5;
/// Most payload bytes signed in one round. Below the 20 MiB default
/// transaction-pool size of a polkadot-sdk node; not measured on Bulletin.
const BATCH_BYTES: usize = 16 * 1024 * 1024;
/// Most transactions signed in one round.
const BATCH_TXS: usize = 256;
/// Pause between rounds that stored nothing.
const RETRY_BACKOFF: Duration = Duration::from_secs(2);
/// Cap on how long the node may take to answer one submission.
const FIRE_TIMEOUT: Duration = Duration::from_secs(20);
/// A round ends when no block was confirmed for this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Cap on any single chain read.
pub(crate) const RPC_TIMEOUT: Duration = Duration::from_secs(30);
/// How long blocks confirmed at best may take to appear in a finalized block.
const FINALITY_TIMEOUT: Duration = Duration::from_secs(180);
/// Poll interval while waiting for finality.
const FINALITY_POLL: Duration = Duration::from_secs(6);

/// Summary of a [`store_blocks`] upload.
pub struct StoreReport {
    pub stored: usize,
    pub skipped: usize,
}

/// The current best block, reconnecting once when the client has failed.
async fn best_block(client: &mut OnlineClient<BulletinConfig>, rpc_url: &str) -> Result<AtBlock> {
    match try_best_block(client).await {
        Ok(at) => Ok(at),
        Err(_) => {
            *client = connect_bulletin(rpc_url).await?;
            try_best_block(client).await
        }
    }
}

async fn try_best_block(client: &OnlineClient<BulletinConfig>) -> Result<AtBlock> {
    let mut best = tokio::time::timeout(RPC_TIMEOUT, client.stream_best_blocks())
        .await
        .context("timed out opening the best-block stream")??;
    let block = tokio::time::timeout(RPC_TIMEOUT, best.next())
        .await
        .context("timed out waiting for a best block")?
        .context("best-block stream ended")??;
    Ok(tokio::time::timeout(RPC_TIMEOUT, block.at())
        .await
        .context("timed out loading the best block")??)
}

async fn account_nonce(at: &AtBlock, account: &AccountId32) -> Result<u64> {
    tokio::time::timeout(RPC_TIMEOUT, at.transactions().account_nonce(account))
        .await
        .context("timed out reading the account nonce")?
        .context("reading the account nonce")
}

/// Store every [`PreparedBlock`] on the Bulletin chain, confirmed at a **best**
/// block. Callers that publish the content must then call
/// [`confirm_finalized`].
///
/// Blocks already stored at the finalized block are skipped, and duplicate
/// content is stored once. The rest go out in rounds of at most one
/// [`batch_len`] batch, in order. Each round:
///
/// 1. reads the batch's content hashes, the account nonce and the era
///    checkpoint from one best block, and drops what is already stored;
/// 2. signs the rest with dense nonces, then writes the nonce range to the
///    signer's in-flight ledger (durably) before the first submission;
/// 3. confirms by reading `TransactionByContentHash` on each new best block,
///    so content stored by any transaction counts ([`RoundWatch`]).
///
/// A round watches while any transaction of this upload can still land, so a
/// resubmission that comes back rejected or occupied does not skip
/// confirmation of earlier pooled ones. After an idle round, if the node
/// accepted the transaction holding the account's next nonce but does not
/// include it, the upload stops with diagnostics instead of signing more
/// behind it ([`round_verdict`]). Rounds that store nothing are bounded by
/// [`RoundBudget`]. `on_progress(done, stored, skipped)` fires as blocks
/// confirm.
pub async fn store_blocks(
    client: &OnlineClient<BulletinConfig>,
    rpc_url: &str,
    signer: &UploadSigner,
    blocks: &[PreparedBlock],
    mut on_progress: impl FnMut(usize, usize, usize),
) -> Result<StoreReport> {
    let total = blocks.len();
    let account = *signer.account();
    let mut client = client.clone();

    let finalized = tokio::time::timeout(RPC_TIMEOUT, client.at_current_block())
        .await
        .context("timed out reading the finalized Bulletin block")??;
    let hashes: Vec<[u8; 32]> = blocks.iter().map(|b| b.content_hash).collect();
    let present = stored_flags(&finalized, &hashes).await?;
    drop(finalized);

    let mut skipped = 0usize;
    let mut todo = Vec::new();
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    for (idx, present) in present.into_iter().enumerate() {
        if present || !seen.insert(blocks[idx].content_hash) {
            skipped += 1;
        } else {
            todo.push(idx);
        }
    }
    let mut stored = 0usize;
    on_progress(stored + skipped, stored, skipped);

    let mut budget = RoundBudget::default();
    let mut live_until: Option<u64> = None;
    let mut last_error: Option<String> = None;
    let started = tokio::time::Instant::now();
    while !todo.is_empty() {
        let stored_before = stored;

        // One best block supplies the content re-check, the nonce and the
        // era checkpoint, so they always agree.
        let at = best_block(&mut client, rpc_url).await?;
        let take = batch_len(todo.iter().map(|&i| blocks[i].data.len()));
        let batch_hashes: Vec<[u8; 32]> = todo[..take]
            .iter()
            .map(|&i| blocks[i].content_hash)
            .collect();
        let landed = stored_flags(&at, &batch_hashes).await?;
        let mut batch = Vec::with_capacity(take);
        let mut rest = todo.split_off(take);
        for (idx, landed) in todo.into_iter().zip(landed) {
            if landed {
                stored += 1;
            } else {
                batch.push(idx);
            }
        }
        on_progress(stored + skipped, stored, skipped);
        if batch.is_empty() {
            todo = rest;
            continue;
        }

        let base_nonce = account_nonce(&at, &account).await?;
        let checkpoint = Checkpoint {
            number: at.block_number(),
            hash: at.block_hash(),
        };
        let last_nonce = base_nonce + batch.len() as u64 - 1;
        let tx_client = at.transactions();
        let mut signed = Vec::with_capacity(batch.len());
        for (offset, &idx) in batch.iter().enumerate() {
            signed.push(build_store_submittable(
                &tx_client,
                signer.keypair(),
                &blocks[idx],
                base_nonce + offset as u64,
                checkpoint,
            )?);
        }
        drop(at);
        signer
            .record_inflight(base_nonce, last_nonce, checkpoint.valid_until())
            .context("recording in-flight nonces before submitting (nothing was submitted)")?;

        let submissions: Vec<Submission> =
            futures::stream::iter(signed.into_iter().map(submit_store))
                .buffered(UPLOAD_CONCURRENCY)
                .collect()
                .await;
        if let Some(Submission::Rejected(e) | Submission::Unknown(e)) = submissions
            .iter()
            .find(|s| matches!(s, Submission::Rejected(_) | Submission::Unknown(_)))
        {
            last_error = Some(e.clone());
        }
        live_until = extend_live_until(live_until, checkpoint.valid_until(), &submissions);
        // (todo index, nonce, submission) for everything not yet confirmed.
        let mut pending: Vec<(usize, u64, Submission)> = batch
            .iter()
            .zip(submissions)
            .enumerate()
            .map(|(offset, (&idx, sub))| (idx, base_nonce + offset as u64, sub))
            .collect();

        let end = confirm_round(
            &mut client,
            blocks,
            &mut pending,
            RoundWatch::new(live_until, started.elapsed()),
            started,
            &mut last_error,
            |newly| {
                stored += newly;
                on_progress(stored + skipped, stored, skipped);
            },
        )
        .await;
        if end == RoundEnd::Expired {
            live_until = None;
        }
        let mut unconfirmed: Vec<usize> = pending.iter().map(|(idx, _, _)| *idx).collect();
        unconfirmed.append(&mut rest);
        todo = unconfirmed;
        if pending.is_empty() {
            continue;
        }

        if end == RoundEnd::Idle {
            let at = best_block(&mut client, rpc_url).await.with_context(|| {
                format!(
                    "reading Bulletin state after {} blocks stayed unconfirmed; their \
                     nonces are recorded in {}",
                    pending.len(),
                    signer.ledger_path().display()
                )
            })?;
            let best_nonce = account_nonce(&at, &account).await?;
            let heads: Vec<(u64, &Submission)> = pending
                .iter()
                .map(|(_, nonce, sub)| (*nonce, sub))
                .collect();
            if let Verdict::Stalled { head_nonce } = round_verdict(end, best_nonce, &heads) {
                let queued = pending
                    .iter()
                    .filter(|(_, _, sub)| {
                        matches!(sub, Submission::Accepted | Submission::Occupied)
                    })
                    .count();
                bail!(
                    "Bulletin is not including uploads from {label} ({account}): the node \
                     accepted the transaction with nonce {head_nonce}, the next one the chain \
                     expects, but no upload was confirmed for {idle}s.\n\
                     {queued} accepted transaction(s) may still be queued (nonces \
                     {base_nonce}..={last_nonce}); they can be included until block \
                     #{valid_until} at the latest (best block is #{best_number}).\n\
                     {stored} of {missing} blocks were stored before the stall. Not retrying: \
                     new transactions would queue behind the stuck ones.\n\
                     The nonces are recorded in {ledger}. A rerun skips stored blocks and does \
                     not sign with this account until the chain includes or expires them.",
                    label = signer.label(),
                    idle = IDLE_TIMEOUT.as_secs(),
                    valid_until = checkpoint.valid_until(),
                    best_number = at.block_number(),
                    missing = stored + todo.len(),
                    ledger = signer.ledger_path().display(),
                );
            }
        }

        if !budget.finish(stored - stored_before) {
            let detail = last_error
                .map(|e| format!(": last error: {e}"))
                .unwrap_or_default();
            bail!(
                "gave up storing {} of {total} blocks after {MAX_FAILED_ROUNDS} rounds that \
                 stored nothing{detail}",
                todo.len()
            );
        }
        if stored == stored_before {
            tokio::time::sleep(RETRY_BACKOFF).await;
        }
    }

    Ok(StoreReport { stored, skipped })
}

/// Watch best blocks until every `pending` block is stored or `watch` ends
/// the round. Confirmed entries are removed from `pending`; `on_stored(n)`
/// reports them. `started` is the clock `watch` was created against.
async fn confirm_round(
    client: &mut OnlineClient<BulletinConfig>,
    blocks: &[PreparedBlock],
    pending: &mut Vec<(usize, u64, Submission)>,
    mut watch: RoundWatch,
    started: tokio::time::Instant,
    last_error: &mut Option<String>,
    mut on_stored: impl FnMut(usize),
) -> RoundEnd {
    if let Some(end) = watch.start() {
        return end;
    }
    let mut best = match tokio::time::timeout(RPC_TIMEOUT, client.stream_best_blocks()).await {
        Ok(Ok(best)) => best,
        Ok(Err(e)) => {
            *last_error = Some(format!("best-block stream error: {e}"));
            return RoundEnd::StreamFailed;
        }
        Err(_) => {
            *last_error = Some("timed out opening the best-block stream".to_string());
            return RoundEnd::StreamFailed;
        }
    };
    loop {
        let idle_left = watch.idle_left(started.elapsed());
        if idle_left.is_zero() {
            return RoundEnd::Idle;
        }
        let block = match tokio::time::timeout(idle_left, best.next()).await {
            Ok(Some(Ok(block))) => block,
            Ok(Some(Err(e))) => {
                *last_error = Some(format!("best-block stream error: {e}"));
                return RoundEnd::StreamFailed;
            }
            Ok(None) => {
                *last_error = Some("best-block stream ended".to_string());
                return RoundEnd::StreamFailed;
            }
            Err(_) => return RoundEnd::Idle,
        };
        let number = block.number();
        let Ok(Ok(at)) = tokio::time::timeout(RPC_TIMEOUT, block.at()).await else {
            continue;
        };
        let hashes: Vec<[u8; 32]> = pending
            .iter()
            .map(|(idx, _, _)| blocks[*idx].content_hash)
            .collect();
        let newly = match stored_flags(&at, &hashes).await {
            Ok(flags) => {
                let before = pending.len();
                let mut flags = flags.into_iter();
                pending.retain(|_| !flags.next().unwrap_or(false));
                before - pending.len()
            }
            Err(e) => {
                *last_error = Some(format!("{e:#}"));
                0
            }
        };
        if newly > 0 {
            on_stored(newly);
        }
        if let Some(end) = watch.on_block(started.elapsed(), number, newly, pending.len()) {
            return end;
        }
    }
}

/// Wait until every content hash in `hashes` is stored at the **finalized**
/// block, bounded by [`FINALITY_TIMEOUT`]. Content must pass this before any
/// DotNS record points at it: a best block can still be reorganized away.
pub async fn confirm_finalized(
    client: &OnlineClient<BulletinConfig>,
    rpc_url: &str,
    hashes: &[[u8; 32]],
) -> Result<()> {
    let mut seen = HashSet::new();
    let mut remaining: Vec<[u8; 32]> = hashes.iter().copied().filter(|h| seen.insert(*h)).collect();
    let total = remaining.len();
    let mut client = client.clone();
    let deadline = tokio::time::Instant::now() + FINALITY_TIMEOUT;
    loop {
        let at = match tokio::time::timeout(RPC_TIMEOUT, client.at_current_block()).await {
            Ok(Ok(at)) => at,
            _ => {
                client = connect_bulletin(rpc_url).await?;
                tokio::time::timeout(RPC_TIMEOUT, client.at_current_block())
                    .await
                    .context("timed out reading the finalized Bulletin block")??
            }
        };
        let flags = stored_flags(&at, &remaining).await?;
        let mut flags = flags.into_iter();
        remaining.retain(|_| !flags.next().unwrap_or(false));
        if remaining.is_empty() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "{} of {total} blocks are in a best block but not yet in a finalized one after \
                 {}s (finalized #{}); nothing was bound. Rerun once Bulletin finalizes: stored \
                 blocks are skipped",
                remaining.len(),
                FINALITY_TIMEOUT.as_secs(),
                at.block_number()
            );
        }
        drop(at);
        tokio::time::sleep(FINALITY_POLL).await;
    }
}

/// Store one blob (an IPLD block) on the Bulletin chain under its own content
/// hash, using the block's `codec` and the given [`Hashing`] algorithm, and wait
/// until it is finalized. Idempotent: if the block is already stored it returns
/// [`StoreOutcome::AlreadyPresent`] without submitting. `data` must be no larger
/// than the chain's `MaxTransactionSize`; callers guard that.
pub async fn store_block(
    client: &OnlineClient<BulletinConfig>,
    rpc_url: &str,
    signer: &UploadSigner,
    codec: u64,
    hashing: Hashing,
    data: &[u8],
) -> Result<StoreOutcome> {
    let block = PreparedBlock {
        codec,
        hashing,
        content_hash: hashing.content_hash(data),
        data: data.to_vec(),
    };
    let report = store_blocks(
        client,
        rpc_url,
        signer,
        std::slice::from_ref(&block),
        |_, _, _| {},
    )
    .await?;
    confirm_finalized(client, rpc_url, &[block.content_hash]).await?;

    let at = tokio::time::timeout(RPC_TIMEOUT, client.at_current_block())
        .await
        .context("timed out reading the finalized Bulletin block")??;
    let (stored_block, index) = stored_location(&at, block.content_hash)
        .await?
        .context("store finalized but TransactionByContentHash is empty")?;
    Ok(if report.stored == 0 {
        StoreOutcome::AlreadyPresent {
            block: stored_block,
            index,
        }
    } else {
        StoreOutcome::Stored {
            block: stored_block,
            index,
        }
    })
}

/// Authorize `who` for Bulletin `TransactionStorage` with a `transactions`/`bytes`
/// quota, submitting a signed `authorize_account` extrinsic. The `signer` must
/// hold Authorizer privileges on the chain, else the extrinsic fails with
/// `BadOrigin` (surfaced to the caller). Returns the finalized extrinsic hash.
///
/// **Must stay a direct, top-level call.** Bulletin grants an Authorizer's
/// `feeless` exemption from the outer call via its custom `AuthorizeCall` /
/// `ValidateAuthorizedCalls` transaction extensions, so wrapping this in
/// `utility.batch_all` loses the exemption and validation fails with
/// `Inability to pay some fees` whenever the Authorizer holds no balance —
/// which is the normal case (PreviewNet's `//Eve` has a zero free balance).
/// Authorize several accounts by calling this once per account, exactly as
/// `paritytech/bulletin-deploy` does.
pub async fn authorize_bulletin_account(
    client: &OnlineClient<BulletinConfig>,
    signer: &Keypair,
    who: AccountId32,
    transactions: u32,
    bytes: u64,
) -> Result<[u8; 32]> {
    let call = bulletin::tx()
        .transaction_storage()
        .authorize_account(who, transactions, bytes);
    let events = client
        .tx()
        .await?
        .sign_and_submit_then_watch_default(&call, signer)
        .await
        .context("submitting TransactionStorage.authorize_account")?
        .wait_for_finalized_success()
        .await
        .context(
            "authorize_account did not finalize successfully \
             (the signer must hold Bulletin Authorizer privileges)",
        )?;
    Ok(events.extrinsic_hash().0)
}

/// Decoded Bulletin `TransactionStorage` authorization extent for an account.
pub struct AuthInfo {
    pub transactions: u32,
    pub transactions_allowance: u32,
    pub bytes: u64,
    pub bytes_allowance: u64,
    pub expiration: u32,
}

/// Read an account's Bulletin authorization + quota, or `None` if unauthorized.
pub async fn authorization(
    client: &OnlineClient<BulletinConfig>,
    who: &AccountId32,
) -> Result<Option<AuthInfo>> {
    let at = client.at_current_block().await?;
    authorization_at(&at, who).await
}

/// [`authorization`] at a given block.
pub async fn authorization_at(at: &AtBlock, who: &AccountId32) -> Result<Option<AuthInfo>> {
    let scope = bulletin::runtime_types::pallet_bulletin_transaction_storage::types::AuthorizationScope::Account(*who);
    let address = bulletin::storage().transaction_storage().authorizations();
    let got = tokio::time::timeout(RPC_TIMEOUT, at.storage().try_fetch(address, (scope,)))
        .await
        .context("timed out reading TransactionStorage.Authorizations")?
        .context("reading TransactionStorage.Authorizations")?;
    match got {
        Some(v) => {
            let a = v.decode().context("decoding Authorization")?;
            let e = a.extent;
            Ok(Some(AuthInfo {
                transactions: e.transactions,
                transactions_allowance: e.transactions_allowance,
                bytes: e.bytes,
                bytes_allowance: e.bytes_allowance,
                expiration: a.expiration,
            }))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use subxt::utils::Era;

    #[test]
    fn era_birth_is_the_checkpoint_block() {
        // A power of two up to 4096 is used as-is, without phase quantization,
        // so the runtime derives the same birth block whose hash was signed.
        assert!(ERA_PERIOD.is_power_of_two());
        assert!((4..=4096).contains(&ERA_PERIOD));
        for number in [0, 1, 63, 64, 1_000, 123_457, 9_999_999] {
            let Era::Mortal { period, phase } = Era::mortal(ERA_PERIOD, number) else {
                panic!("expected a mortal era");
            };
            assert_eq!(period, ERA_PERIOD);
            let birth = (number.max(phase) - phase) / period * period + phase;
            assert_eq!(birth, number);
        }
        let checkpoint = Checkpoint {
            number: 1_000,
            hash: Default::default(),
        };
        assert_eq!(checkpoint.valid_until(), 1_000 + ERA_PERIOD);
    }

    /// Live check that a store signed with dotkit's mortal era validates on
    /// paseo-next-v2, and that a wrong checkpoint hash makes it invalid, which
    /// shows the runtime checks the checkpoint we sign. Ignored by default: it
    /// dials the testnet. Read-only (`TaggedTransactionQueue_validate_transaction`;
    /// nothing is submitted), so it is safe to run on demand:
    ///   cargo test --locked -- --ignored mortal_store_validates_live
    #[tokio::test]
    #[ignore]
    async fn mortal_store_validates_live() {
        let env = Env::resolve("paseo-next-v2").unwrap();
        let client = bulletin_client(&env).await.unwrap();
        let at = try_best_block(&client).await.unwrap();
        let signer = crate::chain::build_signer(None, Some("//deploy/0")).unwrap();
        let nonce = account_nonce(&at, &crate::chain::account_id(&signer))
            .await
            .unwrap();
        let mut data = vec![0u8; 64];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut data);
        let block = PreparedBlock {
            codec: 0x55,
            hashing: Hashing::Sha2_256,
            content_hash: Hashing::Sha2_256.content_hash(&data),
            data,
        };
        let tx_client = at.transactions();
        let checkpoint = Checkpoint {
            number: at.block_number(),
            hash: at.block_hash(),
        };

        let valid = build_store_submittable(&tx_client, &signer, &block, nonce, checkpoint)
            .unwrap()
            .validate()
            .await
            .unwrap();
        assert!(valid.is_valid(), "{valid:?}");

        let wrong = Checkpoint {
            hash: Default::default(),
            ..checkpoint
        };
        let invalid = build_store_submittable(&tx_client, &signer, &block, nonce, wrong)
            .unwrap()
            .validate()
            .await
            .unwrap();
        assert!(!invalid.is_valid(), "{invalid:?}");
    }

    #[test]
    fn legacy_rpc_errors_are_classified_by_the_pool_wording() {
        assert_eq!(
            classify_rpc_error("Invalid Transaction: Transaction is outdated"),
            Submission::Rejected("Invalid Transaction: Transaction is outdated".into())
        );
        assert_eq!(
            classify_rpc_error("Priority is too low: (128 vs 128)"),
            Submission::Occupied
        );
        assert_eq!(
            classify_rpc_error("Transaction Already Imported"),
            Submission::Accepted
        );
        // A transport failure may come after the node took the transaction.
        assert!(matches!(
            classify_rpc_error("RPC error: the connection was lost"),
            Submission::Unknown(_)
        ));
    }

    /// The exact `transactionWatch_v1` event texts from polkadot-sdk
    /// `rpc-spec-v2/src/transaction/error.rs`, as subxt's `submit()` returns
    /// them: inside a `TransactionStatusError`, never as a JSON-RPC error.
    #[test]
    fn chain_head_status_errors_are_classified_by_their_text() {
        use subxt::error::TransactionStatusError as E;
        let invalid = |m: &str| classify_status_error(&E::Invalid(m.to_string()));

        assert_eq!(
            invalid("Transaction is already imported"),
            Submission::Accepted
        );
        assert_eq!(
            invalid("The priority of the transaction is too low (pool 128 > current 128)"),
            Submission::Occupied
        );
        for rejected in [
            "Invalid transaction: Transaction is outdated",
            "Invalid transaction with custom error: 3",
            "Unknown transaction validity: Could not lookup information required to validate the transaction",
            "Transaction is temporarily banned",
            "The transaction could not enter the pool because of the limit",
            "The pool is not accepting future transactions",
            "Verification error: bad signature",
        ] {
            assert!(
                matches!(invalid(rejected), Submission::Rejected(_)),
                "{rejected}"
            );
        }
        // Any other terminal status is still a node answer: not pooled.
        assert!(matches!(invalid("something new"), Submission::Rejected(_)));
        assert!(matches!(
            classify_status_error(&E::Dropped("dropped".into())),
            Submission::Rejected(_)
        ));
        assert!(matches!(
            classify_status_error(&E::Error("node error".into())),
            Submission::Rejected(_)
        ));
    }

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn round_with_nothing_live_ends_at_once() {
        assert_eq!(
            RoundWatch::new(None, Duration::ZERO).start(),
            Some(RoundEnd::NothingPending)
        );
        assert_eq!(RoundWatch::new(Some(1_064), Duration::ZERO).start(), None);
    }

    #[test]
    fn round_expires_after_the_last_includable_block() {
        let mut watch = RoundWatch::new(Some(1_064), Duration::ZERO);
        assert_eq!(watch.on_block(S, 1_062, 0, 3), None);
        // Block 1_063 is the last that can include the round; once checked,
        // nothing more can land.
        assert_eq!(watch.on_block(2 * S, 1_063, 0, 3), Some(RoundEnd::Expired));
        // Storing the last block wins over expiry.
        let mut watch = RoundWatch::new(Some(1_064), Duration::ZERO);
        assert_eq!(watch.on_block(S, 1_063, 3, 0), Some(RoundEnd::Done));
    }

    #[test]
    fn progress_resets_the_idle_timer() {
        let mut watch = RoundWatch::new(Some(10_000), Duration::ZERO);
        assert_eq!(watch.idle_left(Duration::ZERO), IDLE_TIMEOUT);
        assert_eq!(watch.idle_left(50 * S), IDLE_TIMEOUT - 50 * S);
        // A block without progress does not reset it.
        assert_eq!(watch.on_block(50 * S, 100, 0, 5), None);
        assert_eq!(watch.idle_left(55 * S), IDLE_TIMEOUT - 55 * S);
        // Progress does.
        assert_eq!(watch.on_block(55 * S, 101, 2, 3), None);
        assert_eq!(watch.idle_left(55 * S), IDLE_TIMEOUT);
        assert_eq!(watch.idle_left(55 * S + IDLE_TIMEOUT), Duration::ZERO);
    }

    #[test]
    fn earlier_live_rounds_keep_a_rejected_resubmission_watched() {
        let accepted = Submission::Accepted;
        let occupied = Submission::Occupied;
        let unknown = Submission::Unknown("timeout".into());
        let rejected = Submission::Rejected("Invalid transaction: Transaction is outdated".into());

        // Round 1 is accepted; its era ends at 1_064.
        let live = extend_live_until(None, 1_064, &[accepted.clone(), accepted]);
        assert_eq!(live, Some(1_064));
        // Round 2's resubmission is fully rejected: round 1 can still land,
        // so the round watches instead of ending as NothingPending.
        let live = extend_live_until(live, 1_090, &[rejected.clone(), rejected.clone()]);
        assert_eq!(live, Some(1_064));
        assert_eq!(RoundWatch::new(live, Duration::ZERO).start(), None);
        // Occupied and unknown answers may leave a transaction that lands.
        assert_eq!(extend_live_until(live, 1_090, &[occupied]), Some(1_090));
        assert_eq!(extend_live_until(None, 1_090, &[unknown]), Some(1_090));
        // Nothing ever accepted: nothing to watch.
        assert_eq!(extend_live_until(None, 1_090, &[rejected]), None);
    }

    /// Round 1's head times out (it was pooled after all) and the round goes
    /// idle: retry. Round 2 re-signs the same nonce; over chainHead the pool
    /// answers with a priority collision, which keeps the round watched, and
    /// the next idle round reports the stall instead of signing more.
    #[test]
    fn unknown_head_then_occupied_resubmission_is_reported_as_a_stall() {
        use subxt::error::TransactionStatusError as E;
        let round1 = [Submission::Unknown("no answer".into())];
        let live = extend_live_until(None, 1_064, &round1);
        assert_eq!(
            round_verdict(RoundEnd::Idle, 40, &[(40, &round1[0])]),
            Verdict::Retry
        );

        let round2 = [classify_status_error(&E::Invalid(
            "The priority of the transaction is too low (pool 128 > current 128)".into(),
        ))];
        assert_eq!(round2[0], Submission::Occupied);
        let live = extend_live_until(live, 1_080, &round2);
        assert_eq!(RoundWatch::new(live, Duration::ZERO).start(), None);
        assert_eq!(
            round_verdict(RoundEnd::Idle, 40, &[(40, &round2[0])]),
            Verdict::Stalled { head_nonce: 40 }
        );
    }

    #[test]
    fn only_rounds_that_store_nothing_spend_the_budget() {
        let mut budget = RoundBudget::default();
        for _ in 0..100 {
            assert!(budget.finish(3));
        }
        for _ in 0..MAX_FAILED_ROUNDS - 1 {
            assert!(budget.finish(0));
        }
        assert!(budget.finish(1));
        assert!(!budget.finish(0));
    }

    #[test]
    fn batches_are_bounded_by_bytes_and_count() {
        let mib = 1024 * 1024;
        assert_eq!(batch_len([2 * mib; 20]), 8);
        assert_eq!(batch_len([1_000; 1_000]), BATCH_TXS);
        assert_eq!(batch_len([3, 4]), 2);
        // One block larger than a batch still goes out alone.
        assert_eq!(batch_len([BATCH_BYTES + 1, 1]), 1);
        assert_eq!(batch_len(std::iter::empty()), 0);
    }

    #[test]
    fn accepted_head_of_queue_without_inclusion_is_a_stall() {
        let accepted = Submission::Accepted;
        let occupied = Submission::Occupied;
        let rejected = Submission::Rejected("Invalid Transaction".into());
        let unknown = Submission::Unknown("timeout".into());

        assert_eq!(
            round_verdict(RoundEnd::Idle, 12, &[(12, &accepted), (13, &accepted)]),
            Verdict::Stalled { head_nonce: 12 }
        );
        assert_eq!(
            round_verdict(RoundEnd::Idle, 12, &[(12, &occupied)]),
            Verdict::Stalled { head_nonce: 12 }
        );
        // The head never entered a pool: later nonces wait on it, so re-sign.
        assert_eq!(
            round_verdict(RoundEnd::Idle, 12, &[(12, &rejected), (13, &accepted)]),
            Verdict::Retry
        );
        // An unknown head is re-signed; if it was pooled after all, the retry
        // comes back `Occupied` (now also over chainHead) and the next idle
        // round reports the stall.
        assert_eq!(
            round_verdict(RoundEnd::Idle, 12, &[(12, &unknown)]),
            Verdict::Retry
        );
        // Nothing of ours holds the next nonce.
        assert_eq!(
            round_verdict(RoundEnd::Idle, 20, &[(12, &accepted)]),
            Verdict::Retry
        );
        // Expired or fully rejected rounds can no longer land anything.
        assert_eq!(
            round_verdict(RoundEnd::Expired, 12, &[(12, &accepted)]),
            Verdict::Retry
        );
        assert_eq!(
            round_verdict(RoundEnd::NothingPending, 12, &[(12, &rejected)]),
            Verdict::Retry
        );
        // A failed block stream says nothing about inclusion; re-signing is
        // bounded and pooled transactions come back `Occupied`.
        assert_eq!(
            round_verdict(RoundEnd::StreamFailed, 12, &[(12, &accepted)]),
            Verdict::Retry
        );
    }
}
