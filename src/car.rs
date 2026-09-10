//! Executable packaging: the "CAR as a chunked UnixFS file" layer that DotNS
//! *executables* (App / Worker records) are stored as, distinct from the plain
//! directory root a website is stored as.
//!
//! Two content models share one resolver and are indistinguishable by CID:
//!
//! - **Website** (`dotkit deploy`, [`crate::merkle`]): the bound CID *is* the
//!   UnixFS directory root, so a gateway serves `index.html` from it.
//! - **Executable** (this module): the directory DAG is serialized to a CARv1
//!   archive, and the bound CID is that archive stored **as a file** — chunked,
//!   with raw leaves under a dag-pb UnixFS file root. Fetching `index.js` under
//!   it fails ("no link named"); the consumer is the Store, which downloads the
//!   whole archive and imports it. The inner CAR root is the real directory.
//!
//! Only the chunks and the file root are uploaded to Bulletin. The inner
//! directory blocks are *not* stored individually — they travel inside the
//! archive bytes.
//!
//! # Wire format (verified byte-exact against chain)
//!
//! Leaves are `≤ 2 MiB` slices of the archive stored as raw blocks (codec
//! `0x55`, sha2-256). Above them sits a single dag-pb node: `Links` (field 2)
//! carrying one `PBLink` per chunk — `Hash`, an **empty but present** `Name`,
//! and `Tsize` — followed by `Data` (field 1) holding a UnixFS `Data` message
//! of `Type = File(2)`, `filesize`, and one `blocksizes` entry per chunk. The
//! file root is emitted even for a single chunk, i.e. there is **no
//! single-leaf-to-self reduction**, which is what makes a small executable a
//! dag-pb CID (`bafybei…`) rather than a raw one (`bafkrei…`).
//!
//! That layout was pinned by decoding two live records on paseo-next-v2 rather
//! than by reading upstream source (see the tests): a single-chunk archive
//! reproduces its on-chain CID exactly.
//!
//! # Chunk boundaries
//!
//! Chunks are **CAR-section aligned**: a chunk never splits a CAR section
//! (`varint(len) ++ cid ++ block`), and sections are packed greedily up to the
//! 2 MiB budget. That invariant was measured, not assumed — decoding
//! `app.jollity.paseo`'s 25-chunk root shows every boundary landing exactly on a
//! section boundary, with no chunk over 2 MiB.
//!
//! Upstream's *exact* boundaries are not reproducible: the same 19.9 MB archive
//! yields 25 unevenly sized chunks (49910, 1836476, …, a lone 396-byte chunk,
//! then a run of single-section chunks), i.e. it emits whatever its CAR stream
//! has buffered at each flush, so two upstream deploys of identical content
//! disagree with each other. Greedy packing satisfies both structural
//! invariants and is deterministic; for that archive it produces 11 chunks
//! where upstream produced 25. Same archive bytes, same inner root, same
//! per-chunk block model — only the split points differ, and the consumer
//! reassembles the byte stream regardless.

use crate::bulletin::{content_hash, PreparedBlock, MAX_TRANSACTION_SIZE};
use anyhow::{bail, Context, Result};
use cid::multihash::Multihash;
use cid::Cid;

/// dag-pb IPLD codec.
const DAG_PB: u64 = 0x70;
/// raw IPLD codec — the archive chunks are stored as raw leaves.
const RAW: u64 = 0x55;
/// sha2-256 multihash code.
const SHA2_256: u64 = 0x12;
/// UnixFS `DataType::File`.
const UNIXFS_TYPE_FILE: u64 = 2;

/// Width of one archive chunk. Matches the chain's `MaxTransactionSize`, so
/// every leaf still fits a single store extrinsic.
const CHUNK_SIZE: usize = MAX_TRANSACTION_SIZE;

/// An executable packaged for Bulletin: the dag-pb file `root` to bind as the
/// subdomain's contenthash, and the blocks to upload (every chunk, then the
/// root node itself).
pub struct PackagedExecutable {
    pub root: Cid,
    pub blocks: Vec<PreparedBlock>,
    pub car_len: usize,
    pub chunks: usize,
}

/// The CIDv1 of an already-prepared block, rebuilt from its codec and the
/// sha2-256 digest the Bulletin chain keys it by (no re-hashing).
fn block_cid(block: &PreparedBlock) -> Result<Cid> {
    let mh = Multihash::wrap(SHA2_256, &block.content_hash)
        .context("wrapping a block's sha2-256 digest into a multihash")?;
    Ok(Cid::new_v1(block.codec, mh))
}

/// Serialize a merkleized DAG into CARv1 archive bytes with `root` as the sole
/// declared root. The root block is written first (as `ipfs dag export` does),
/// then the remaining blocks in merkleization order.
pub async fn car_bytes(root: &Cid, blocks: &[PreparedBlock]) -> Result<Vec<u8>> {
    let header = iroh_car::CarHeader::new_v1(vec![*root]);
    let mut writer = iroh_car::CarWriter::new(header, Vec::new());

    let mut wrote_root = false;
    for block in blocks {
        if block_cid(block)? == *root {
            writer
                .write(*root, block.data.as_slice())
                .await
                .context("writing the root block into the CAR")?;
            wrote_root = true;
            break;
        }
    }
    if !wrote_root {
        bail!("merkleized DAG does not contain its own root block {root}");
    }

    for block in blocks {
        let cid = block_cid(block)?;
        if cid == *root {
            continue;
        }
        writer
            .write(cid, block.data.as_slice())
            .await
            .with_context(|| format!("writing block {cid} into the CAR"))?;
    }

    writer.finish().await.context("finishing the CARv1 archive")
}

/// Append a protobuf varint.
fn varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Append a protobuf field key (`field_number << 3 | wire_type`).
fn key(field: u64, wire: u64, out: &mut Vec<u8>) {
    varint((field << 3) | wire, out);
}

/// Append a length-delimited protobuf field.
fn bytes_field(field: u64, value: &[u8], out: &mut Vec<u8>) {
    key(field, 2, out);
    varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

/// Append a varint protobuf field.
fn varint_field(field: u64, value: u64, out: &mut Vec<u8>) {
    key(field, 0, out);
    varint(value, out);
}

/// The UnixFS `Data` message for a file split across `blocksizes`.
fn unixfs_file_data(filesize: u64, blocksizes: &[usize]) -> Vec<u8> {
    let mut out = Vec::new();
    varint_field(1, UNIXFS_TYPE_FILE, &mut out);
    varint_field(3, filesize, &mut out);
    for size in blocksizes {
        varint_field(4, *size as u64, &mut out);
    }
    out
}

/// One `PBLink` to a raw leaf: `Hash`, an empty-but-present `Name`, `Tsize`.
///
/// The empty `Name` is load-bearing. Omitting the field changes the encoded
/// bytes and therefore the root CID, which is why it is asserted by the
/// on-chain golden vectors in this module's tests rather than assumed.
fn pb_link(cid: &Cid, tsize: usize) -> Vec<u8> {
    let mut out = Vec::new();
    bytes_field(1, &cid.to_bytes(), &mut out);
    bytes_field(2, b"", &mut out);
    varint_field(3, tsize as u64, &mut out);
    out
}

/// The dag-pb file root over `chunk_cids`: `Links` first, then `Data`.
fn file_root_block(chunks: &[(Cid, usize)], filesize: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for (cid, size) in chunks {
        let link = pb_link(cid, *size);
        bytes_field(2, &link, &mut out);
    }
    let sizes: Vec<usize> = chunks.iter().map(|(_, size)| *size).collect();
    let data = unixfs_file_data(filesize, &sizes);
    bytes_field(1, &data, &mut out);
    out
}

/// Read a protobuf/CAR varint at `offset`, returning `(value, bytes_consumed)`.
fn read_varint(data: &[u8], offset: usize) -> Result<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0u32;
    let mut i = offset;
    loop {
        let byte = *data
            .get(i)
            .context("CAR archive ends inside a section length varint")?;
        i += 1;
        value |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, i - offset));
        }
        shift += 7;
        if shift > 63 {
            bail!("CAR archive has an overlong section length varint");
        }
    }
}

/// Byte ranges of a CARv1's sections — the header, then one per block. Each
/// section is `varint(len) ++ len bytes`, so the archive is a flat sequence of
/// them with no index to consult.
fn car_sections(car: &[u8]) -> Result<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < car.len() {
        let (len, header) = read_varint(car, at)?;
        let end = at
            .checked_add(header)
            .and_then(|s| s.checked_add(len as usize))
            .context("CAR section length overflows the archive")?;
        if end > car.len() {
            bail!(
                "CAR section at offset {at} claims {len} bytes but only {} remain",
                car.len() - at - header
            );
        }
        out.push((at, end));
        at = end;
    }
    if out.is_empty() {
        bail!("CAR archive contains no sections");
    }
    Ok(out)
}

/// Split `car` into chunks that never straddle a CAR section boundary, packing
/// sections greedily up to [`CHUNK_SIZE`].
///
/// A section larger than the budget cannot be kept whole and is split at byte
/// boundaries instead — the leaf still has to fit one store extrinsic. Our own
/// merkleizer caps file leaves at 256 KiB, so the largest section it emits is
/// ~256 KiB (measured: 262183 bytes on a real 19.9 MB archive); the fallback
/// only matters for a `--input-car` archive carrying a near-2 MiB block, whose
/// section is a few dozen bytes of CID and varint over the limit.
fn section_aligned_chunks(car: &[u8]) -> Result<Vec<&[u8]>> {
    let sections = car_sections(car)?;
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut current = 0usize;

    for (section_start, section_end) in sections {
        let size = section_end - section_start;

        if size > CHUNK_SIZE {
            if current > 0 {
                chunks.push(&car[start..section_start]);
                current = 0;
            }
            for slice in car[section_start..section_end].chunks(CHUNK_SIZE) {
                chunks.push(slice);
            }
            start = section_end;
            continue;
        }

        if current > 0 && current + size > CHUNK_SIZE {
            chunks.push(&car[start..section_start]);
            start = section_start;
            current = 0;
        }
        current += size;
    }

    if current > 0 {
        chunks.push(&car[start..]);
    }
    Ok(chunks)
}

/// Package archive bytes as a chunked UnixFS file: split on CAR section
/// boundaries within a [`CHUNK_SIZE`] budget, store each chunk as a raw leaf,
/// and build the dag-pb file root over them.
pub fn chunked_file(car: &[u8]) -> Result<PackagedExecutable> {
    if car.is_empty() {
        bail!("refusing to package an empty CAR archive");
    }

    let mut blocks = Vec::new();
    let mut chunk_cids = Vec::new();
    for chunk in section_aligned_chunks(car)? {
        let hash = content_hash(chunk);
        let mh = Multihash::wrap(SHA2_256, &hash).context("wrapping a chunk digest")?;
        chunk_cids.push((Cid::new_v1(RAW, mh), chunk.len()));
        blocks.push(PreparedBlock {
            codec: RAW,
            data: chunk.to_vec(),
            content_hash: hash,
        });
    }

    let root_block = file_root_block(&chunk_cids, car.len() as u64);
    if root_block.len() > MAX_TRANSACTION_SIZE {
        bail!(
            "the executable's dag-pb file root is {} bytes across {} chunks, over the chain's \
             2 MiB MaxTransactionSize; this packager emits a single-level file DAG, which caps \
             an executable at roughly 68 GB",
            root_block.len(),
            chunk_cids.len()
        );
    }
    let root_hash = content_hash(&root_block);
    let root = Cid::new_v1(
        DAG_PB,
        Multihash::wrap(SHA2_256, &root_hash).context("wrapping the file root digest")?,
    );
    blocks.push(PreparedBlock {
        codec: DAG_PB,
        data: root_block,
        content_hash: root_hash,
    });

    Ok(PackagedExecutable {
        root,
        chunks: chunk_cids.len(),
        car_len: car.len(),
        blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `worker.jollity.paseo`'s live contenthash on paseo-next-v2, and the exact
    /// byte length the gateway serves for it. A 597049-byte CAR archive fits one
    /// 2 MiB chunk, so this vector pins the whole encoding *except* the chunk
    /// boundaries: raw leaf codec, the empty-but-present `Name`, `Tsize`, the
    /// UnixFS field order, and the fact that a single chunk is still wrapped in a
    /// dag-pb file root instead of being reduced to the raw leaf.
    ///
    /// Reproduce the source bytes with:
    ///   curl -s https://paseo-bulletin-next-ipfs.polkadot.io/ipfs/<WORKER_ROOT> -o worker.car
    const WORKER_ROOT: &str = "bafybeihwguy272fc4xoyngbrmzeewvevmf7dyppggtotjtvg5ycc6h3u7e";
    const WORKER_CAR_LEN: usize = 597049;
    /// sha2-256 of that archive's single 597049-byte chunk, i.e. its raw leaf
    /// digest — lets the vector be asserted without vendoring 583 KiB.
    const WORKER_LEAF: &str = "bafkreign7p7m3jqnwdjzjgn2sfbmmvg25hyiqo6lzf2cuff4xrx2dojusu";

    /// Rebuild the file root from a chunk's CID + length alone, bypassing the
    /// need to hold the archive bytes in the test.
    fn root_from_chunks(chunks: &[(Cid, usize)]) -> Cid {
        let filesize = chunks.iter().map(|(_, s)| *s as u64).sum();
        let block = file_root_block(chunks, filesize);
        Cid::new_v1(
            DAG_PB,
            Multihash::wrap(SHA2_256, &content_hash(&block)).unwrap(),
        )
    }

    #[test]
    fn single_chunk_file_root_matches_the_live_worker_record() {
        let leaf = Cid::try_from(WORKER_LEAF).unwrap();
        assert_eq!(leaf.codec(), RAW);
        let root = root_from_chunks(&[(leaf, WORKER_CAR_LEN)]);
        assert_eq!(root.to_string(), WORKER_ROOT);
        assert_eq!(root.codec(), DAG_PB);
    }

    /// The empty `Name` field is not cosmetic: dropping it silently produces a
    /// different CID. Guard the alternative encoding explicitly.
    #[test]
    fn omitting_the_empty_link_name_would_change_the_root() {
        let leaf = Cid::try_from(WORKER_LEAF).unwrap();
        let mut without_name = Vec::new();
        let mut link = Vec::new();
        bytes_field(1, &leaf.to_bytes(), &mut link);
        varint_field(3, WORKER_CAR_LEN as u64, &mut link);
        bytes_field(2, &link, &mut without_name);
        let data = unixfs_file_data(WORKER_CAR_LEN as u64, &[WORKER_CAR_LEN]);
        bytes_field(1, &data, &mut without_name);
        let root = Cid::new_v1(
            DAG_PB,
            Multihash::wrap(SHA2_256, &content_hash(&without_name)).unwrap(),
        );
        assert_ne!(root.to_string(), WORKER_ROOT);
    }

    /// A single chunk must still get a dag-pb parent. If this ever reduced to the
    /// leaf, executables would bind a `bafkrei…` raw CID and the Store would be
    /// handed a chunk instead of a file.
    #[test]
    fn a_one_chunk_executable_is_not_reduced_to_its_leaf() {
        let car = synthetic_car(2, 64);
        let packaged = chunked_file(&car).unwrap();
        assert_eq!(packaged.chunks, 1);
        assert_eq!(packaged.root.codec(), DAG_PB);
        // chunk + root node.
        assert_eq!(packaged.blocks.len(), 2);
        assert_eq!(packaged.blocks[0].codec, RAW);
        assert_eq!(packaged.blocks[1].codec, DAG_PB);
    }

    #[test]
    fn chunking_respects_the_two_mib_budget_and_keeps_every_leaf_submittable() {
        // A real archive over several chunks: many files, each becoming its own
        // CAR section, so packing has genuine boundaries to choose.
        let car = synthetic_car(600, 12_000);
        let packaged = chunked_file(&car).unwrap();
        assert!(
            packaged.chunks > 1,
            "expected a multi-chunk archive, got {}",
            packaged.chunks
        );
        assert_eq!(packaged.car_len, car.len());
        for block in &packaged.blocks {
            assert!(
                block.data.len() <= MAX_TRANSACTION_SIZE,
                "every block must fit one store extrinsic"
            );
        }
    }

    /// The invariant measured on `app.jollity.paseo`: no chunk boundary ever
    /// falls inside a CAR section. Asserted structurally so a future rewrite to
    /// plain byte slicing fails here.
    #[test]
    fn every_chunk_boundary_lands_on_a_car_section() {
        let car = synthetic_car(600, 12_000);
        let sections = car_sections(&car).unwrap();
        let boundaries: std::collections::HashSet<usize> =
            sections.iter().map(|(_, end)| *end).collect();

        let chunks = section_aligned_chunks(&car).unwrap();
        assert!(chunks.len() > 1);
        let mut at = 0usize;
        for chunk in &chunks {
            at += chunk.len();
            assert!(
                boundaries.contains(&at),
                "chunk boundary at {at} splits a CAR section"
            );
        }
        assert_eq!(at, car.len());
    }

    /// Greedy packing must actually use the budget — a boundary is only taken
    /// when the next whole section would overflow it.
    #[test]
    fn chunks_are_packed_greedily_up_to_the_budget() {
        let car = synthetic_car(600, 12_000);
        let sections = car_sections(&car).unwrap();
        let sizes: Vec<usize> = sections.iter().map(|(s, e)| e - s).collect();
        let chunks = section_aligned_chunks(&car).unwrap();

        // Every chunk but the last must be within one section-width of the cap.
        let max_section = *sizes.iter().max().unwrap();
        for chunk in &chunks[..chunks.len() - 1] {
            assert!(chunk.len() <= CHUNK_SIZE);
            assert!(
                chunk.len() + max_section > CHUNK_SIZE,
                "chunk of {} left room for another section",
                chunk.len()
            );
        }
    }

    /// A section wider than the whole budget cannot stay intact; it is split at
    /// byte boundaries so the leaf still fits one extrinsic.
    #[test]
    fn an_oversized_section_is_split_rather_than_overflowing() {
        // One section whose payload alone exceeds the chunk budget.
        let mut car = Vec::new();
        let payload = vec![9u8; CHUNK_SIZE + 5_000];
        let mut len = Vec::new();
        super::varint(payload.len() as u64, &mut len);
        car.extend_from_slice(&len);
        car.extend_from_slice(&payload);

        let chunks = section_aligned_chunks(&car).unwrap();
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= CHUNK_SIZE);
        }
        assert_eq!(chunks.iter().map(|c| c.len()).sum::<usize>(), car.len());
    }

    #[test]
    fn a_truncated_car_is_rejected_rather_than_mis_split() {
        // Section claims far more bytes than the archive holds.
        let mut car = Vec::new();
        super::varint(9_000, &mut car);
        car.extend_from_slice(b"only a few bytes");
        assert!(car_sections(&car).is_err());
    }

    /// Build a CARv1-shaped archive by merkleizing a generated directory, so the
    /// section layout is the one a real deploy produces.
    fn synthetic_car(files: usize, size: usize) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!(
            "dotkit-car-synth-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..files {
            std::fs::write(
                dir.join(format!("asset-{i:04}.bin")),
                vec![(i % 251) as u8; size],
            )
            .unwrap();
        }
        let m = crate::merkle::merkleize_dir(dir.to_str().unwrap()).unwrap();
        let car = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(car_bytes(&m.root, &m.blocks))
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        car
    }

    /// The packaged file's bytes are the archive's bytes: concatenating the
    /// leaves back must reproduce the input exactly, whatever the chunking.
    #[test]
    fn leaves_concatenate_back_to_the_archive() {
        let car = synthetic_car(400, 9_000);
        let packaged = chunked_file(&car).unwrap();
        assert!(packaged.chunks > 1);
        let rebuilt: Vec<u8> = packaged.blocks[..packaged.chunks]
            .iter()
            .flat_map(|b| b.data.clone())
            .collect();
        assert_eq!(rebuilt, car);
    }

    #[test]
    fn empty_archive_is_rejected() {
        assert!(chunked_file(b"").is_err());
    }

    /// Package a real build directory end to end (no chain access), through the
    /// same path `deploy` uses. `DOTKIT_PACKAGE_MANIFEST` exercises the App v2
    /// injection path: the record is added to the DAG as `manifest.json` before
    /// merkleization, and this asserts it is actually present in the archive's
    /// directory listing and that it moved the inner root. Ignored by default:
    ///   DOTKIT_PACKAGE_DIR=../chat-spa/apps/test-bot/dist/app \
    ///   DOTKIT_PACKAGE_MANIFEST='{"$v":2,...}' \
    ///     cargo test -- --ignored --nocapture package_env_dir
    #[tokio::test]
    #[ignore]
    async fn package_env_dir() {
        let Ok(dir) = std::env::var("DOTKIT_PACKAGE_DIR") else {
            eprintln!("set DOTKIT_PACKAGE_DIR to run this packaging check");
            return;
        };
        let manifest = std::env::var("DOTKIT_PACKAGE_MANIFEST").ok();
        let injected: Vec<(String, Vec<u8>)> = manifest
            .as_ref()
            .map(|m| vec![("manifest.json".to_string(), m.clone().into_bytes())])
            .unwrap_or_default();

        let plain = crate::merkle::merkleize_dir(&dir).unwrap();
        let m = crate::merkle::merkleize_dir_with(&dir, &injected).unwrap();
        let car = car_bytes(&m.root, &m.blocks).await.unwrap();
        let packaged = chunked_file(&car).unwrap();

        // Section alignment is the invariant measured on chain; assert it here
        // on real content rather than only on generated fixtures.
        let boundaries: std::collections::HashSet<usize> = car_sections(&car)
            .unwrap()
            .iter()
            .map(|(_, end)| *end)
            .collect();
        let mut at = 0usize;
        for chunk in section_aligned_chunks(&car).unwrap() {
            at += chunk.len();
            assert!(
                boundaries.contains(&at),
                "chunk boundary {at} splits a section"
            );
        }

        let path = std::env::temp_dir().join(format!("dotkit-pkg-{}.car", std::process::id()));
        std::fs::write(&path, &car).unwrap();
        let (parsed_root, parsed_blocks) =
            crate::bulletin::read_car_prepared(path.to_str().unwrap())
                .await
                .unwrap();

        assert_eq!(parsed_root, m.root, "archive must declare the inner root");
        assert_eq!(parsed_blocks.len(), m.blocks.len());

        if let Some(manifest) = &manifest {
            assert_ne!(
                m.root, plain.root,
                "injecting manifest.json must change the inner root"
            );
            let names = root_link_names(&parsed_blocks, &parsed_root);
            assert!(
                names.iter().any(|n| n == "manifest.json"),
                "manifest.json missing from the archive root listing: {names:?}"
            );
            eprintln!("  manifest    injected ({} bytes)", manifest.len());
        }

        eprintln!(
            "{dir}\n  inner root  {}{}\n  file root   {}\n  car         {} bytes in {} chunk(s)\n  \
             blocks      {} inner -> {} uploaded\n  car file    {}",
            m.root,
            if manifest.is_some() {
                format!(" (plain: {})", plain.root)
            } else {
                String::new()
            },
            packaged.root,
            packaged.car_len,
            packaged.chunks,
            m.blocks.len(),
            packaged.blocks.len(),
            path.display()
        );
    }

    /// Chunk an existing CARv1 archive (no chain access), for checking the
    /// packer against a real multi-chunk archive — including one produced by
    /// upstream. Asserts section alignment, the 2 MiB budget, and byte-exact
    /// reassembly, and prints the resulting layout. Ignored by default:
    ///   DOTKIT_CHUNK_CAR=/path/to/app.car \
    ///     cargo test -- --ignored --nocapture chunk_env_car
    #[test]
    #[ignore]
    fn chunk_env_car() {
        let Ok(path) = std::env::var("DOTKIT_CHUNK_CAR") else {
            eprintln!("set DOTKIT_CHUNK_CAR to run this chunking check");
            return;
        };
        let car = std::fs::read(&path).unwrap();
        let sections = car_sections(&car).unwrap();
        let chunks = section_aligned_chunks(&car).unwrap();
        let packaged = chunked_file(&car).unwrap();

        let boundaries: std::collections::HashSet<usize> =
            sections.iter().map(|(_, end)| *end).collect();
        let mut at = 0usize;
        for chunk in &chunks {
            at += chunk.len();
            assert!(
                boundaries.contains(&at),
                "chunk boundary {at} splits a CAR section"
            );
            assert!(chunk.len() <= CHUNK_SIZE, "chunk over the 2 MiB budget");
        }
        assert_eq!(at, car.len());

        let rebuilt: Vec<u8> = packaged.blocks[..packaged.chunks]
            .iter()
            .flat_map(|b| b.data.clone())
            .collect();
        assert_eq!(rebuilt, car, "leaves must reassemble to the archive");

        eprintln!(
            "{path}\n  car         {} bytes · {} sections (max {})\n  chunks      {} \
             (sizes {:?})\n  file root   {}",
            car.len(),
            sections.len(),
            sections.iter().map(|(s, e)| e - s).max().unwrap(),
            packaged.chunks,
            chunks.iter().map(|c| c.len()).collect::<Vec<_>>(),
            packaged.root
        );
    }

    /// Directory link names of the archive's root dag-pb node, so a test can
    /// assert what a consumer importing the CAR would actually see.
    fn root_link_names(blocks: &[PreparedBlock], root: &Cid) -> Vec<String> {
        let node = blocks
            .iter()
            .find(|b| block_cid(b).unwrap() == *root)
            .expect("root block present in archive");
        let mut names = Vec::new();
        let data = &node.data;
        let mut i = 0usize;
        while i < data.len() {
            let (k, used) = read_varint(data, i).unwrap();
            i += used;
            let (field, wire) = (k >> 3, k & 7);
            if wire != 2 {
                let (_, used) = read_varint(data, i).unwrap();
                i += used;
                continue;
            }
            let (len, used) = read_varint(data, i).unwrap();
            i += used;
            let chunk = &data[i..i + len as usize];
            i += len as usize;
            if field != 2 {
                continue;
            }
            let mut j = 0usize;
            while j < chunk.len() {
                let (lk, used) = read_varint(chunk, j).unwrap();
                j += used;
                let (lfield, lwire) = (lk >> 3, lk & 7);
                if lwire != 2 {
                    let (_, used) = read_varint(chunk, j).unwrap();
                    j += used;
                    continue;
                }
                let (llen, used) = read_varint(chunk, j).unwrap();
                j += used;
                if lfield == 2 {
                    names.push(String::from_utf8_lossy(&chunk[j..j + llen as usize]).to_string());
                }
                j += llen as usize;
            }
        }
        names
    }

    /// A CAR round-trip: the archive we write must parse back to the same root
    /// and block set, which is what the Store's importer will do with it.
    #[tokio::test]
    async fn car_bytes_round_trips_through_the_reader() {
        let dir = std::env::temp_dir().join(format!("dotkit-car-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.js"), b"export default 1;\n").unwrap();
        std::fs::write(dir.join("extra.txt"), vec![b'z'; 400_000]).unwrap();

        let m = crate::merkle::merkleize_dir(dir.to_str().unwrap()).unwrap();
        let car = car_bytes(&m.root, &m.blocks).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();

        // CARv1 header, then the declared root first.
        let path = std::env::temp_dir().join(format!("dotkit-car-{}.car", std::process::id()));
        std::fs::write(&path, &car).unwrap();
        let (root, blocks) = crate::bulletin::read_car_prepared(path.to_str().unwrap())
            .await
            .unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(root, m.root);
        assert_eq!(blocks.len(), m.blocks.len());
        let packaged = chunked_file(&car).unwrap();
        assert_eq!(packaged.car_len, car.len());
        assert_eq!(packaged.root.codec(), DAG_PB);
    }
}
