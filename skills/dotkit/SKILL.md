---
name: dotkit
description: "Use when working with the dotkit CLI (a fast single-binary Rust tool for Bulletin storage + DotNS naming on Paseo Asset Hub / pallet_revive) — sharing a file through Dotshare, deploying a static build dir to a DotNS domain (merkleize, Bulletin upload, bind contenthash), registering an open-tier DotNS name, looking up who owns a name or whether it's available, transferring a name you own, creating a subnode/subdomain under a name you own, resolving or setting a name's contenthash/text records, publishing a deployed name to Browse via the Publisher registry, verifying a CID resolves on the gateway, checking or granting Bulletin quota, checking a PAS balance, mapping SS58 to H160, emitting machine-readable --json, or diagnosing a register/bind revert. Trigger phrases: share a file with dotkit, get a dotshare link from the terminal, dotkit share file.pdf, deploy my app to a .paseo or .dot name with dotkit, dotkit deploy ./dist myapp.paseo, register a .paseo name, who owns this name, transfer a name to someone, create a subdomain with dotkit, dotkit subnode create app.myapp.paseo, bind a CID to a name, publish my app to Browse, what TLD does this env use, dotkit deploy --publish, unpublish a .dot from Browse, verify a CID resolves, authorize an account for Bulletin, why did dotkit register revert, set a manifest text record, set a product display name and icon, generate a root manifest for Browse, dotkit deploy --register, what PoP tier does this name need."
---

# dotkit

Fast single-binary Rust CLI for the Polkadot Triangle/Trinity stack: **Bulletin** storage + **DotNS** naming (Asset Hub / `pallet_revive`). No Node/Bun, no `ipfs` daemon (native in-process UnixFS merkleization, byte-exact with Kubo 0.40.1). First-class command is `dotkit deploy`.

- **Binary:** `dotkit` on PATH, or build from source: `cargo build --release` → `./target/release/dotkit`.
- **Default env:** `paseo-next-v2` — TLD **`.paseo`**, resolves at `https://<name>.paseo.li`.
- **TLD is per-env.** DotNS was redeployed on Paseo v2 on 2026-08-11 and Paseo names now end in **`.paseo`**, not `.dot`. `preview` (PreviewNet) moved to **`.test`** after its 2026-08 re-genesis. dotkit appends the selected env's TLD when you omit it, so prefer bare labels (`myapp`) in scripts and let `--env` decide.
- **Envs are config, not code.** Built-in defaults ship in the binary; `~/.dotkit/envs.toml` overlays them **by id** (existing id = patch those fields, new id = add an env). See the Environments section — adding a chain or fixing a post-wipe address needs no rebuild.

## Command surface

| Command | What it does |
|---|---|
| `share <file> [--name <name>] [--mime <type>]` | Wrap one file in Dotshare's v2 envelope, store it on Bulletin, and print browser + host viewer links. Unencrypted; ≤2 MiB including the small envelope. |
| `deploy <dir> <domain>` | Merkleize → Bulletin upload → bind the DotNS contenthash. With a `[product]` `deploy.toml` it also uploads the icon + writes the root `manifest` record. Add `--register` to auto-register an open-tier name; `--publish` to also list it in Browse. |
| `bulletin store <file>` | Store one blob (≤2 MiB) on Bulletin. |
| `bulletin store-car <file.car>` | Store every block of a CARv1 so its root resolves. |
| `bulletin status [--address <ss58>]` | Bulletin authorization / quota for an account. |
| `bulletin verify <cid>` | Check a CID actually resolves on the env's IPFS gateway (live HTTP probe). |
| `bulletin authorize [--address <ss58>] [--transactions N] [--bytes N]` | Grant an account Bulletin storage quota (default `1000` txs / `100 MB`). Signer needs **Authorizer** privileges and defaults to the env's `bulletin_authorizer` (`//Alice` on paseo-next-v2, `//Eve` on preview); override with `--mnemonic`/`--derivation-path`. Not the pool. |
| `asset-hub transfer <dest> <plancks>` | Send native PAS. |
| `asset-hub map` | Ensure the signer has an H160 mapping (`Revive.map_account`). |
| `asset-hub name resolve <name>` | Name → contenthash CID. |
| `asset-hub name owner-of <name>` (alias `oo`) | Whether a name is registered and who owns it (H160). |
| `asset-hub name lookup <name>` | Read-only overview: owner, required tier + status, base price, contenthash. |
| `asset-hub name register <name>` | Register a name (commit/reveal) to the signer — open, or Lite/Full with a personhood-verified signer. |
| `asset-hub name transfer <name> <to>` | Transfer a name you own to `<to>` (0x H160 or SS58); pays the quoted friction fee. |
| `asset-hub name subnode create <child> [to]` | Create/reassign a subnode (subdomain) under a parent name you own, e.g. `app.myapp.paseo`; owner defaults to the signer. |
| `asset-hub name publish <name>` | List a name you own in Browse via the Publisher registry. |
| `asset-hub name unpublish <name>` | Remove a name you own from Browse (no rebuild needed). |
| `asset-hub name content set <name> <cid>` | Bind a CID to a name's contenthash. |
| `asset-hub name content <name>` | Read the raw contenthash record. |
| `asset-hub name text set <name> <key> <value>` | Set a text record (e.g. `manifest`, `executable`). |
| `asset-hub name text get <name> <key>` | Read a text record. |
| `account env` / `account whoami` | Print resolved env / prove signer + chain connectivity (shows SS58 + H160). |
| `account info` | Show the signer's Asset Hub native (PAS) balance. |
| `bulletin pool init [--accounts N] [--force] [--skip-authorize]` / `status` / `authorize [--transactions N] [--bytes N]` | Manage a **private per-machine** Bulletin upload pool (`~/.dotkit/pool.toml`, `0600`; derived `//deploy/N`). `init` generates the keystore **and authorizes** its accounts on-chain via the env's `bulletin_authorizer` in one step — pass `--skip-authorize` for offline-only generation. `status` shows each account's **on-chain** auth + quota with an `N/M authorized` rollup (honors `--pool`, so `--pool shared` inspects the shared pool; an authorization whose expiry block has passed is flagged `✗ EXPIRED` and does **not** count as authorized). `authorize` re-authorizes accounts with **one direct `authorize_account` call each** (never `utility.batch_all`, which loses the Authorizer's feeless exemption): idempotent on still-valid auths, and it **re-authorizes expired ones** (a lingering-but-expired record still exists on-chain but no longer grants free storage, so stores fail "balance too low" until refreshed). `deploy`/`store` use the pool by default (override with `--pool local\|shared`). Testnet-only. |

**Global flags:** `--env <id>` (default `paseo-next-v2`), `--mnemonic`, `--derivation-path //x`, `--pool <local|shared>` (Bulletin upload pool; default: private `~/.dotkit` pool if a keystore exists, else shared), `-q/--quiet`, `--json` (one machine-readable JSON object per command; errors become `{"error": …}` on stderr).
**`deploy` flags:** `--register`, `--publish`, `--fail-on-publish-error`, `--config <deploy.toml>`, `--input-car <file>`, `--kubo`.

## Environments

| `--env` | TLD | Notes |
|---|---|---|
| `paseo-next-v2` *(default)* | `.paseo` | Full support, `<name>.paseo.li`. Verified live. |
| `preview` | `.test` | PreviewNet; `.test` since its 2026-08 re-genesis. Same CREATE3 contracts as Paseo v2. Verified live. |

Those are the only two that ship in the binary — an env is built in only if it can be
defined centrally *and* has been verified against a live chain. Any other network (a
local devnet, or one whose RPCs aren't published upstream) is defined per machine in the
overlay, so treat `dotkit account env --list` as the authoritative list rather than this
table.

Two layers, merged by id:

1. **Built-in** — `assets/envs.toml`, compiled in, so dotkit runs with no config file.
2. **Overlay** — `~/.dotkit/envs.toml`, if present. An existing id patches only the fields it lists; a new id adds an env.

```toml
# ~/.dotkit/envs.toml

[paseo-next-v2]                  # patch one address after a chain wipe
publisher = "0x1875B90A61705917945f9B7C6Ff7819Ad48A198e"

[mynet]                          # or add a whole env
tld = "test"
asset_hub_rpc = "ws://127.0.0.1:9944"
bulletin_rpc  = "ws://127.0.0.1:9945"
```

- `dotkit account env` shows the resolved env plus its `source` (`builtin` / `builtin+user` / `user`); `dotkit account env --list` shows all of them.
- Only **`tld`** is required for a new env — it feeds the namehash and cannot be guessed. Everything else may be omitted; the command that needs a missing endpoint or address says so by name.
- Unknown keys are **rejected**, so a typo like `reslover` fails loudly instead of being silently ignored.

## Signer & account model

- Default signer = a shared **dev account** (base of the standard dev phrase); its base derivation is the dev-mode DotNS owner on testnets. Override with `--mnemonic` (or `$MNEMONIC`, then `$DOTNS_MNEMONIC`) + `--derivation-path`.
- **Bulletin writes** use a random authorized **pool account** `//deploy/{0..N}`. By default this is the **private per-machine pool** (`~/.dotkit/pool.toml`) when a keystore exists (`bulletin pool init`, which now also authorizes), else the **shared** `DEV_PHRASE//deploy/{0..9}` pool; force either with `--pool local|shared`. Pool accounts are Bulletin-authorized but **not funded on Asset Hub** — never use one as the DotNS owner signer (its `map_account`/bind will fail "balance too low").
- Every Revive write auto-runs `Revive.map_account` if the signer isn't mapped.

## DotNS naming rules & PoP tiers (verified on-chain)

The registrar's `classifyName` (on `POP_RULES`) gates a label by shape + base length:

| Label shape | Tier | classifyName status |
|---|---|---|
| Long base (e.g. `mycoolsite`, `dotshare-preview00`) | 0 | "Available to all" (open) |
| Shorter base + **exactly 2** trailing digits (`hostdiag91`) | 1 | "Requires Lite personhood verification" |
| Short base, no digits (`hostdiag`) | 2 | "Requires Full personhood verification" |
| Very short (`ab`) | 3 | Reserved |

- **A label must end in NO digits or EXACTLY 2 digits.** 1 or 3+ trailing digits → the contract reverts: `Name must have no digit suffix or exactly 2 digit suffix`.
- `dotkit name register` and `deploy --register` handle **open (0) and personhood-gated Lite (1) / Full (2)**; **Reserved (3)** is rejected (governance-only). For Lite/Full, dotkit pre-checks the owner's `personhoodStatus(owner, "dotns")` on the AH precompile (`0x…0a010000`) and bails **before committing** if the signer's tier is too low.
- Lite/Full names need a **personhood-verified signer** (Full satisfies Lite). Get testnet personhood at `sudo.personhood.dev/personhood-faucet` (env "Next V2"); the signer must also be funded + H160-mapped on Asset Hub. Note: People-chain personhood is **not** auto-bridged — bind it to the `dotns` context via `sudo.personhood.dev/dotns-bootstrap` first.

## Deploy workflow

```sh
# Deploy to a name you own (redeploy just updates the contenthash)
dotkit deploy ./dist myapp.paseo

# First-time: register the name in the same run (open, or Lite/Full if the signer is verified)
dotkit deploy ./dist myapp.paseo --register
```

`deploy` reads the Registry owner first: proceeds if you own it, errors if someone else does, and (with `--register`) registers an unregistered name (open, or Lite/Full if the signer has the personhood) before uploading. Then it merkleizes, uploads blocks to Bulletin (pool signer), binds the contenthash (owner signer), and prints the CID + `https://<name>.paseo.li`.

**Optional `deploy.toml`** (`--config <path>` or auto-detected `./deploy.toml`; unknown keys rejected):

```toml
[text]
manifest = "https://example.com/manifest.json"
executable = "worker.js"

[product]
display_name = "TV Explorer"
description = "10,000+ free live TV channels"
icon = "icon.png"          # path relative to deploy.toml; PNG or JPEG
```

Each `[text]` entry is written via `setText` after the bind. The build dir is never scanned for the config (its files get uploaded).

**`[product]` (generated root manifest).** When `[product]` is present, `deploy` uploads the `icon` to Bulletin (single blob, ≤2 MiB, **blake2b-256** multihash — the host's Browse/preimage icon resolver requires it; a sha2-256 icon CID resolves on the IPFS gateway but Browse renders the fallback identicon), builds the RFC root manifest `{"$v":1,"displayName","description","icon":{"cid","format"}}`, and writes it as the base name's `manifest` text record — so Browse shows a name + icon, not just a resolvable contenthash. The app's contenthash stays the SPA entry point; no `app.<name>` subname or `executable` records are created (that's for multi-surface widget/worker products). `icon.format` is inferred from the extension (`.png`→`png`, `.jpg`/`.jpeg`→`jpeg`); other extensions are rejected. `[product]` **generates** the `manifest` record, so a config that also sets a manual `[text].manifest` is rejected as a conflicting source of truth. Writing the manifest is automatic on deploy; Browse discovery still needs explicit `--publish` (personhood-gated + rate-limited).

## Browse listing (Publisher)

`--publish` (or the standalone `asset-hub name publish <name>`) calls `publish(<label>)` on the env's Browse **Publisher** registry so the app shows up in Browse without users searching for its name. Take it off later with `asset-hub name unpublish <name>` — no rebuild.

```sh
# Deploy and list in Browse in one run
dotkit deploy ./dist myapp.paseo --publish

# Or list/retract an already-deployed name
dotkit asset-hub name publish myapp.paseo
dotkit asset-hub name unpublish myapp.paseo
```

- **Per-env Publisher.** Each deployment is bound to one TLD, so the address is selected by `--env` (`env.publisher`); dotkit refuses `--publish`/`publish` on an env with none configured.
- **Owner-only, base labels only.** The signer must own the name NFT; dotkit pre-checks ownership and rejects subdomains (`app.`/`widget.`/`worker.`) — only the base `<label>` can be listed.
- **Personhood-gated + rate-limited.** Non-owner-of-contract callers need Lite/Full personhood (`NoPersonhood` revert otherwise) and a per-day publish cap (Lite 1/day, Full 5/day). A freshly registered open-tier name whose owner has no personhood can't publish yet.
- In `deploy`, a publish failure is **non-fatal by default** (warns, exit 0); add `--fail-on-publish-error` to hard-fail after a successful deploy.

## Subnodes (subdomains)

`asset-hub name subnode create <child> [to]` creates (or reassigns) a subnode via the DotNS **Registry** `setSubnodeOwner` — a different contract path from base registration (which goes through the RegistrarController commit/reveal). Give the full child name; the env TLD is appended when omitted:

```sh
# Create app.myapp.paseo, owned by the signer (must own myapp.paseo)
dotkit asset-hub name subnode create app.myapp.paseo

# Assign the new subnode to someone else (0x H160 or SS58)
dotkit asset-hub name subnode create app.myapp.paseo 0xabc… # or an SS58 address
```

- **Owner-only, parent-sovereign.** Only the **parent** name's owner can create subnodes under it (dotkit pre-checks ownership); the call **overwrites** any existing owner of that subnode. No commit/reveal, no PoP tier, no fee beyond gas.
- **No pricing/NFT.** A subnode is not an ERC721 name (no `register`/`transfer`/`publish` NFT semantics) — it's a directly-owned Registry node. Bind its records with the normal `name content set` / `name text set` afterward.
- **Base names only** still applies to registration and Browse: `register` mints top-level `<label>.<tld>` names, and the **Publisher** rejects subnodes — you can bind/resolve a subnode's records but you can't `publish` it to Browse.

## Diagnosing reverts

dotkit surfaces the real EVM revert reason. Map it:

- `requires Lite/Full personhood, but the signer … has NoStatus` → the name is personhood-gated; use a verified signer (`sudo.personhood.dev/personhood-faucet`, env Next V2) or pick an open (long-base) name. dotkit bails here **before** committing.
- `Name must have no digit suffix or exactly 2 digit suffix` → rename (0 or 2 trailing digits).
- `custom error 0x14c417b5 …` echoing your H160 → not authorized (you don't own the node).
- `cannot publish <name>: publishing to Browse needs Lite or Full personhood …` → the Publisher gates non-owner callers; verify at `sudo.personhood.dev` (env Next V2) or publish from a verified signer.
- `cannot publish <name>: daily publish cap reached (Lite 1/day, Full 5/day); next publish allowed in ~N min …` → wait out the rolling 24h window, or use a Full-tier signer for a higher cap.
- `no reason returned (empty revert…)` → often an unmapped account or an address with no code; run `account whoami` / `asset-hub map`.
- `AccountUnmapped` / "balance too low" on map → fund the signer on Asset Hub (`faucet.polkadot.io/?parachain=1500`) then `asset-hub map`.

## Host / content contract

Deployed root must be **CIDv1 / dag-pb (or raw single-file) / sha2-256** with `index.html` at the directory root — the web host fails closed on any other multihash/codec. Native merkleization already produces exactly this (Kubo default).

## Hard rules

- **Open + Lite/Full** registration (Reserved rejected). Lite/Full need a personhood-verified signer; dotkit pre-checks `personhoodStatus` and bails early if the signer's tier is too low.
- **Name digits:** none or exactly two, else the register reverts.
- **`<name>.paseo.li`** is the v2 gateway; `<name>.dot.li` points at the dead Summit chain — never use it for v2.
- **Secrets** via `$MNEMONIC` / `$DOTNS_MNEMONIC`, not `--mnemonic` in shell history.
- **`preview` env** shares Paseo v2's CREATE3 contract set but uses the **`.test`** TLD since PreviewNet's 2026-08 re-genesis. Never pass a `.paseo` name to `--env preview` (or vice versa) — the TLD is part of the namehash, so it silently targets a different node: a register succeeds under the other TLD and the ownership read comes back zero. Pass bare labels and this can't happen.
- **`InvalidTransaction::Stale` ("Transaction is outdated") is a nonce race**, not a config problem. The default signer is the shared public dev phrase, used concurrently by others and CI. Retry.
- **Name transfer** pays the registrar's quoted friction fee (0 for same-tier/upward moves, a fee for downward moves); only the current NFT owner can transfer, and the recipient `<to>` is a `0x` H160 or SS58 address.
- **Publisher (`--publish` / `name publish|unpublish`)** is per-env, owner-only, base-label-only, and personhood-gated + rate-limited. In `deploy` it's non-fatal by default (`--fail-on-publish-error` to hard-fail).
- **`bulletin authorize`** needs a signer that holds Bulletin **Authorizer** privileges; it defaults to the env's `bulletin_authorizer` and the default storage pool cannot authorize (the chain returns `BadOrigin`). The Authorizer is **env-specific** — `//Alice` on paseo-next-v2, `//Eve` on preview, where signing as `//Alice` fails `BadSigner`.
- **`InsufficientAuthorizerBudget`** means the grant exceeds what the Authorizer has left in `AllowedAuthorizers` (its own `quota` is debited per grant), not that your account is at fault. Lower `--transactions`/`--bytes`; the defaults (`1000` / `100 MB`) are sized to fit.
- **`--json`** makes every command print one JSON object to stdout (read commands like `name owner-of`/`lookup`, `bulletin verify`, `account info` are read-only and script-friendly); on failure it prints `{"error": …}` to stderr.
- **Single blob > 2 MiB** is not yet supported (`bulletin store` bails; Kubo/native chunking keeps deploy blocks ≤256 KiB).
- **`--env` carries a matched set** — the Bulletin RPC, the DotNS **TLD** and the Asset Hub contract addresses go together; select an env, don't mix them. After a chain wipe, re-verify against `paritytech/dotns-releases` before trusting a deploy.
