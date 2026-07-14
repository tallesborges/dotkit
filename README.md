# dotkit

A fast single-binary Rust CLI for the Polkadot **Triangle/Trinity** ecosystem — **Bulletin** storage and **DotNS** naming (Asset Hub / `pallet_revive`).

The first-class command is `dotkit deploy`, a native replacement for the existing Node-based deploy + `.dot` naming CLIs: it merkleizes a build directory, uploads the DAG to the Bulletin chain, and binds the content CID to a `.dot` domain — all from one static binary with no Node/Bun runtime and no `ipfs` daemon.

## Why

- **One static binary** — no `node_modules`, no runtime install, fast cold-start.
- **Native merkleization** — an in-process UnixFS encoder that reproduces Kubo's CIDv1 layout (CIDv1, raw leaves, sha2-256, 256 KiB balanced chunks, wrap-with-directory). No `ipfs` binary required. Golden-tested for byte-exact CID parity against Kubo 0.40.1.
- **Env-matched** — `--env` selects the Bulletin RPC *and* the Asset Hub contract addresses as one set, so the v1/v2 address split can't drift.

## Install

The quickest way — download the prebuilt binary for your platform (no Rust toolchain needed):

```sh
curl -fsSL https://raw.githubusercontent.com/tallesborges/dotkit/master/install.sh | bash
```

This grabs the latest release, verifies its checksum, and installs `dotkit` into a directory on your `PATH` (defaults to `~/.local/bin`). Then `dotkit --help` should work from anywhere. If the installer warns that the bin dir isn't on your `PATH`, add the printed `export PATH=...` line to your shell profile.

Prebuilt binaries are published for **macOS (aarch64)** and **Linux (x86_64)**.

Installer options (run it locally to pass flags):

```sh
curl -fsSL https://raw.githubusercontent.com/tallesborges/dotkit/master/install.sh -o install.sh
bash install.sh --version v0.1.0     # pin a specific release tag
bash install.sh --bin-dir ~/bin      # install into a specific dir (or BIN_DIR=~/bin)
bash install.sh --from-source        # build from a local checkout with cargo instead
```

### Build from source

Requires a recent stable Rust toolchain ([rustup.rs](https://rustup.rs)):

```sh
git clone git@github.com:tallesborges/dotkit.git
cd dotkit
./install.sh --from-source   # builds release + installs to ~/.local/bin
```

Or use Cargo directly: `cargo install --path .` (or `just install`) puts `dotkit` in `~/.cargo/bin`. Or just `cargo build --release` and run `./target/release/dotkit`.

## Quickstart

Deploy a built site to a `.dot` domain you own:

```sh
dotkit deploy ./dist myapp.dot
```

This merkleizes `./dist`, uploads every block to Bulletin, sets the contenthash on `myapp.dot`, and prints the gateway + `https://myapp.paseo.li` URL.

Register the name first if you need one — or add `--register` to the `deploy` command above to do it in one step:

```sh
dotkit asset-hub name register myapp.dot
```

## Command surface

| Command | What it does |
|---|---|
| `deploy <dir> <domain.dot>` | Merkleize → Bulletin upload → bind `.dot` contenthash (the MVP flow). |
| `bulletin store <file>` | Store a single blob (≤2 MiB) on Bulletin. |
| `bulletin store-car <file.car>` | Store every block of a CARv1 so its root resolves. |
| `bulletin status [--address <ss58>]` | Show authorization / quota for an account. |
| `bulletin verify <cid>` | Check a CID actually resolves on the env's IPFS gateway (live HTTP probe). |
| `bulletin authorize [--address <ss58>] [--transactions N] [--bytes N]` | Grant an account Bulletin storage quota (signer needs Authorizer privileges). |
| `asset-hub transfer <dest> <plancks>` | Send native PAS on Asset Hub. |
| `asset-hub map` | Ensure the signer has an H160 mapping (`Revive.map_account`). |
| `asset-hub name resolve <name.dot>` | Resolve a name to its contenthash CID. |
| `asset-hub name owner-of <name.dot>` | Show whether a name is registered and who owns it. |
| `asset-hub name lookup <name.dot>` | Read-only overview: owner, required tier + status, base price, contenthash. |
| `asset-hub name register <name.dot>` | Register a name (commit/reveal) — open, or Lite/Full with a personhood-verified signer. |
| `asset-hub name transfer <name.dot> <to>` | Transfer a name you own to `<to>` (0x H160 or SS58); pays the quoted friction fee. |
| `asset-hub name content set <name.dot> <cid>` | Bind a CID to a name's contenthash. |
| `asset-hub name content <name.dot>` | Read a name's raw contenthash record. |
| `asset-hub name text set <name.dot> <key> <value>` | Set a text record (e.g. `manifest`, `executable`). |
| `asset-hub name text get <name.dot> <key>` | Read a text record. |
| `account env` | Print the resolved environment config. |
| `account whoami` | Derive the signer and prove Asset Hub + Bulletin connectivity. |
| `account info` | Show the signer's Asset Hub native (PAS) balance. |
| `bulletin pool init [--accounts N] [--force] [--skip-authorize]` | Generate a private per-machine Bulletin upload pool (`~/.dotkit/pool.toml`, `0600`), print its `//deploy/N` accounts, and authorize them on-chain (via `//Alice`) in one step. `--skip-authorize` generates the keystore only. Testnet-only. |
| `bulletin pool status` | Show each pool account's on-chain authorization + quota (txs/bytes used vs allowance) with an `N/M authorized` rollup. `--pool shared` inspects the shared pool instead. |
| `bulletin pool authorize [--transactions N] [--bytes N]` | Authorize all pool accounts for Bulletin storage in one `utility.batch_all` (signer defaults to `//Alice`). Idempotent — skips already-authorized. |

- `--register` — register the domain first (open, or Lite/Full if the signer is verified) when it isn't already owned.
- `--config <deploy.toml>` — write text records from a config after the bind (auto-detected as `./deploy.toml`).
- `--input-car <file>` — deploy a pre-built CAR instead of merkleizing.
- `--kubo` — merkleize with the external `ipfs` binary instead of the native encoder (fallback).

### Global flags

- `--env <id>` — target environment (default `paseo-next-v2`).
- `--mnemonic <phrase>` — signer mnemonic. Falls back to `$MNEMONIC`, then `$DOTNS_MNEMONIC`; defaults to a shared dev account on testnets.
- `--derivation-path <path>` — Substrate derivation path (e.g. `//Alice`).
- `--pool <local|shared>` — which Bulletin upload pool to sign with. Default: the private `~/.dotkit` pool if a keystore exists, else the shared dev pool. See [Bulletin upload pools](#bulletin-upload-pools).
- `-q`, `--quiet` — suppress step/detail output; only errors are printed (useful in CI/scripts).
- `--json` — emit one machine-readable JSON object per command instead of human output; on failure prints `{"error": …}` to stderr.

## Environments

| `--env` | Bulletin + Asset Hub | Notes |
|---|---|---|
| `paseo-next-v2` (default) | Paseo Next v2 | Full support; resolves at `<name>.paseo.li`. |
| `preview` | PreviewNet | Partial — `asset-hub name register` is not yet wired. |

## Bulletin upload pools

Uploading blocks to Bulletin is signed by an account from an **upload pool** — a set of `//deploy/N` accounts used round-robin so parallel uploads don't collide on nonces or quota. There are two pools:

- **Shared dev pool** — derived from the well-known public dev phrase (`DEV_PHRASE//deploy/{0..9}`). Pre-funded and Bulletin-authorized on testnets. Zero setup, but **everyone shares it**, so you contend with other users for nonces and quota.
- **Private pool** — a per-machine keystore at `~/.dotkit/pool.toml` (`0600`) holding a locally-generated 12-word mnemonic and its own `//deploy/N` accounts. Isolated from other users. **Testnet-only** — the mnemonic is plaintext and holds no mainnet value.

### Which pool a command uses

Selection is controlled by the global `--pool` flag:

| `--pool` value | Behavior |
|---|---|
| *(omitted — default)* | **Auto:** use the private pool **if** `~/.dotkit/pool.toml` exists, otherwise fall back to the shared dev pool. |
| `--pool local` | Force the private pool (errors if no keystore — run `pool init` first). |
| `--pool shared` | Force the shared dev pool, even if a private keystore exists. |

> A private pool is **never created automatically.** Until you run `pool init`, every command signs with the shared dev pool.

### Create and use your own pool

```sh
# Generate a private per-machine pool AND authorize its accounts for Bulletin storage.
# Writes ~/.dotkit/pool.toml (0600) and prints the //deploy/N accounts.
dotkit bulletin pool init
#   --accounts N        change the account count (default 10)
#   --force             regenerate (new mnemonic) over an existing keystore
#   --skip-authorize    only generate the keystore; authorize later

# From now on, deploys auto-use your private pool (a keystore now exists):
dotkit deploy ./dist myapp.dot
# ...or force one explicitly:
dotkit deploy ./dist myapp.dot --pool local     # your private pool
dotkit deploy ./dist myapp.dot --pool shared    # the shared dev pool
```

`pool init` authorizes the accounts on-chain in the same step, signed by the testnet Authorizer `//Alice` by default (override with `--mnemonic`/`--derivation-path`). Two more commands help you inspect/repair the pool:

```sh
dotkit bulletin pool status      # each account's on-chain authorization + quota (--pool shared for the shared pool)
dotkit bulletin pool authorize   # (re)authorize accounts — idempotent; only needed after --skip-authorize or to raise allowances
```

Every signed command prints a one-line note of which pool + account it picked (e.g. `pool: private //deploy/3 (…)` or `pool: shared (…)`), suppressed under `--quiet`/`--json`.

## How merkleization stays Kubo-compatible

`dotkit deploy` builds the same content DAG that `ipfs add -r --cid-version=1 --raw-leaves --hidden` would, using [`rust-unixfs`](https://crates.io/crates/rust-unixfs)'s `FileAdder` + `BufferingTreeBuilder`. Files are added in lexicographic path order (hidden files included), chunked at 256 KiB, hashed with sha2-256, and wrapped in a directory root — the exact defaults Kubo uses for CIDv1. The produced blocks are stored on Bulletin keyed by their own content hash, so the root CID resolves on any IPFS gateway.

Parity is enforced by golden tests, including live cross-checks against `ipfs` when present:

```sh
# unit golden vectors (no ipfs needed)
cargo test merkle

# compare our root vs kubo for any directory (needs ipfs on PATH)
DOTKIT_COMPARE_DIR=./dist cargo test -- --ignored compare_env
```

## Status

The `deploy` MVP is built and live-verified end-to-end on `paseo-next-v2`, including auto-register (`--register`), Lite/Full personhood-gated registration (with a pre-commit personhood check), text records via `deploy.toml`, native merkleization (golden-tested for byte-exact Kubo parity), reliable commit/reveal, and decoded on-chain revert reasons. Remaining work: a chunked path for single blobs larger than 2 MiB, and full `preview`-env contract addresses.
