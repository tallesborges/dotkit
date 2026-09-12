---
name: dotkit
description: "Use when working with the dotkit CLI, a single-binary Rust tool for Bulletin storage and DotNS naming on Paseo Asset Hub (pallet_revive). Covers deploying a static build dir to a DotNS name (merkleize, Bulletin upload, bind contenthash); publishing App and Worker executables to app./worker. subdomains; registering, looking up or transferring names and subnodes; contenthash and text records; Dotshare file links; Bulletin quota and authorization; per-env TLDs; and diagnosing register/bind reverts. Trigger phrases: deploy my app with dotkit, dotkit deploy ./dist myapp.paseo, register a .paseo name, who owns this name, deploy an app and worker executable, share a file with dotkit, why did dotkit register revert, what TLD does this env use, authorize an account for Bulletin."
---

# dotkit

Fast single-binary Rust CLI for the Polkadot Triangle/Trinity stack: **Bulletin** storage + **DotNS** naming (Asset Hub / `pallet_revive`). No Node/Bun, no `ipfs` daemon (native in-process UnixFS merkleization, byte-exact with Kubo 0.40.1). First-class command is `dotkit deploy`.

- **Binary:** `dotkit` on PATH, or build from source: `cargo build --release` → `./target/release/dotkit`.
- **Default env:** `paseo-next-v2` — TLD **`.paseo`**, resolves at `https://<name>.paseo.li`.
- **TLD is per-env.** DotNS was redeployed on Paseo v2 on 2026-08-11 and Paseo names now end in **`.paseo`**, not `.dot`. `preview` (PreviewNet) re-rooted again and is now **`.testnet`** (it was `.test` in 2026-08). dotkit appends the selected env's TLD when you omit it, so prefer bare labels (`myapp`) in scripts and let `--env` decide.
- **Envs are config, not code.** Built-in defaults ship in the binary; `~/.dotkit/envs.toml` overlays them **by id** (existing id = patch those fields, new id = add an env). See the Environments section — adding a chain or fixing a post-wipe address needs no rebuild.

## Command surface

| Command | What it does |
|---|---|
| `share <file> [--name <name>] [--mime <type>]` | Wrap one file in Dotshare's v2 envelope, store it on Bulletin, and print browser + host viewer links. Unencrypted; ≤2 MiB including the small envelope. |
| `deploy <dir> <domain>` | Merkleize → Bulletin upload → bind the DotNS contenthash. With a `[product]` `deploy.toml` it also uploads the icon + writes the root `manifest` record, and with `[[executables]]` it publishes App/Worker executables to `app.<domain>` / `worker.<domain>`. Add `--register` to auto-register an open-tier name; `--publish` to also list it in Browse. |
| `bulletin store <file>` | Store one blob (≤2 MiB) on Bulletin. |
| `bulletin store-car <file.car>` | Store every block of a CARv1 so its root resolves. |
| `bulletin status [--address <ss58>]` | Bulletin authorization / quota for an account. |
| `bulletin verify <cid>` | Check a CID actually resolves on the env's IPFS gateway (live HTTP probe). |
| `bulletin authorize [--address <ss58>] [--transactions N] [--bytes N]` | Grant an account Bulletin storage quota (default `1000` txs / `100 MB`). Signer needs **Authorizer** privileges and defaults to the env's `bulletin_authorizer` (`//Alice` on paseo-next-v2, `//Eve` on preview); override with `--mnemonic`/`--derivation-path`. Not the pool. |
| `asset-hub transfer <dest> <plancks>` | Send native PAS. |
| `asset-hub map` | Ensure the signer has an H160 mapping (`Revive.map_account`). |
| `asset-hub status` | Per-address deployment state of the env's six DotNS contracts (`✓ deployed` / `✗ absent` / `– not configured`), via code-at-address. Read-only, no signer. This is how you check whether a post-wipe redeployment has landed. |
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
| `account login --app-id <id> --metadata-url <https-url>` | Pair with a mobile wallet via a terminal QR and persist its account under `~/.dotkit/papp/sessions`. Read-only for DotNS/Bulletin: it does not request allowances, sign, upload, query balances, or submit a chain transaction. The pairing protocol does use the selected env's People-chain Statement Store and required attestation. |
| `bulletin pool init [--accounts N] [--force] [--skip-authorize]` / `status` / `authorize [--transactions N] [--bytes N]` | Manage a **private per-machine** Bulletin upload pool (`~/.dotkit/pool.toml`, `0600`; derived `//deploy/N`). `init` generates the keystore **and authorizes** its accounts on-chain via the env's `bulletin_authorizer` in one step — pass `--skip-authorize` for offline-only generation. `status` shows each account's **on-chain** auth + quota with an `N/M authorized` rollup (honors `--pool`, so `--pool shared` inspects the shared pool; an authorization whose expiry block has passed is flagged `✗ EXPIRED` and does **not** count as authorized). `authorize` re-authorizes accounts with **one direct `authorize_account` call each** (never `utility.batch_all`, which loses the Authorizer's feeless exemption): idempotent on still-valid auths, and it **re-authorizes expired ones** (a lingering-but-expired record still exists on-chain but no longer grants free storage, so stores fail "balance too low" until refreshed). `deploy`/`store` use the pool by default (override with `--pool local\|shared`). Testnet-only. |

**Global flags:** `--env <id>` (default `paseo-next-v2`), `--mnemonic`, `--derivation-path //x`, `--pool <local|shared>` (Bulletin upload pool; default: private `~/.dotkit` pool if a keystore exists, else shared), `-q/--quiet`, `--json` (one machine-readable JSON object per command; errors become `{"error": …}` on stderr).
`account login` requires an explicit product identity (`--app-id`) and HTTPS metadata URL (`--metadata-url`); it rejects `--json`, `--quiet`, `--mnemonic`, `--derivation-path`, and `--pool` because a QR must render in the terminal and those signer/pool inputs are not part of pairing.
**`deploy` flags:** `--register`, `--publish`, `--fail-on-publish-error`, `--config <deploy.toml>`, `--input-car <file>`, `--kubo`.

## Environments

| `--env` | TLD | Notes |
|---|---|---|
| `paseo-next-v2` *(default)* | `.paseo` | Full support, `<name>.paseo.li`. Verified live. |
| `preview` | `.testnet` | PreviewNet; re-rooted from `.test` to **`.testnet`** around 2026-09-01 (verified live 2026-09-10 via `DotnsProtocolRegistry.tld()`). Same CREATE3 contracts as Paseo v2. |

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
people_rpc = "ws://127.0.0.1:9946"
```

- `dotkit account env` shows the resolved env plus its `source` (`builtin` / `builtin+user` / `user`); `dotkit account env --list` shows all of them.
- Only **`tld`** is required for a new env — it feeds the namehash and cannot be guessed. Everything else may be omitted; the command that needs a missing endpoint or address says so by name.
- `account login` uses `people_rpc` for the People-chain Statement Store. Both built-in envs provide it; custom envs may omit it until pairing is needed.
- Unknown keys are **rejected**, so a typo like `reslover` fails loudly instead of being silently ignored.

## Signer & account model

- Default signer = a shared **dev account** (base of the standard dev phrase); its base derivation is the dev-mode DotNS owner on testnets. Override with `--mnemonic` (or `$MNEMONIC`, then `$DOTNS_MNEMONIC`) + `--derivation-path`.
- **Bulletin writes** use a random authorized **pool account** `//deploy/{0..N}`. By default this is the **private per-machine pool** (`~/.dotkit/pool.toml`) when a keystore exists (`bulletin pool init`, which now also authorizes), else the **shared** `DEV_PHRASE//deploy/{0..9}` pool; force either with `--pool local|shared`. Pool accounts are Bulletin-authorized but **not funded on Asset Hub** — never use one as the DotNS owner signer (its `map_account`/bind will fail "balance too low").
- Every Revive write auto-runs `Revive.map_account` if the signer isn't mapped.

## DotNS naming rules & PoP tiers (verified on-chain)

**Label rules differ per DotNS generation, and dotkit reads them from the chain** (`classifyName` + `priceWithoutCheck` on `POP_RULES`) instead of replicating them, so it follows a redeploy with no client change. When a shape matters, measure it — `dotkit --env <id> asset-hub name lookup <label>` — rather than reasoning from either rule set below.

### `.paseo` (paseo-next-v2) — DotNS v0.6.0

Verified live 2026-09-10. Labels are measured **as written** and the old digit rules are **gone**: there is no digit-suffix constraint (`dotkitprobe7` and `dotkitprobe1234` both classify open), and digits count toward base length (`myapp-pr07` measures 10, not 8).

| Label shape | Tier | classifyName status |
|---|---|---|
| 9+ chars (`chatspaapp`, `myapp-pr07`) | 0 | "Available to all" (open) · ~10 PAS |
| 6–8 chars (`chat-spa`, `mypr07`) | 1 / 2 | Lite/Full — **not for sale** on the public registrar |
| Very short (`ab`) | 3 | Reserved (governance) |

The 6–8 char band is closed by `PopRules.shortNamesEnabled = false`: `priceWithoutCheck` reverts `Short names are not for sale`, so the public `RegistrarController` cannot price it at **any** personhood tier. **Verification alone does not open it** — upstream's preflight gets this wrong and tells users to verify (their issue #1415); dotkit reports both the required tier and the not-for-sale fact.

### `preview` (PreviewNet) — still the older profile

The pre-v0.6.0 rules remain in force there:

- A label must end in **no digits or exactly 2**, else `classifyName` reverts `Name must have no digit suffix or exactly 2 digit suffix` (`0x2dfc7d98`).
- Tier is computed on the base label **excluding** a trailing 2-digit suffix, so `dotpulse00` classifies like `dotpulse` — padding a short name with digits does not make it buyable.

A label shape that registers on `.paseo` today may still be refused on `preview`.

### Public registrar vs PoP gateway

Two ways to mint a name; **dotkit implements only the first**.

- **Public `RegistrarController`** (commit/reveal, what dotkit does): handles **open-tier** names. **Reserved (3)** is rejected as governance-only.
- **PoP gateway** (`dotnsGateway.register_name`): mints Lite/Full and short names against a People-chain ring-membership proof, bypassing the registrar controller entirely. dotkit does **not** implement it, and whether it accepts a given short label has not been tested here.

So "not for sale" in dotkit's output means *not purchasable through the public registrar* — it is not proof the name is unobtainable by any route.

dotkit still pre-checks the owner's `personhoodStatus(owner, "dotns")` on the AH precompile (`0x…0a010000`) and bails **before committing** when the signer's tier is too low, so an unverified signer stops there first. Get testnet personhood at `sudo.personhood.dev/personhood-faucet` (env "Next V2"); the signer must also be funded + H160-mapped on Asset Hub. People-chain personhood is **not** auto-bridged — bind it to the `dotns` context via `sudo.personhood.dev/dotns-bootstrap` first.

### Registration ABI (commit/reveal)

The 2026-09-01 redeploy widened the registrar's `Registration` tuple, which moved both commit/reveal selectors:

```solidity
Registration(string label, address owner, bytes32 secret, bool reserved, uint256 maxPrice, uint256 pricingVersion)
```

| Call | Selector | Notes |
|---|---|---|
| `makeCommitment(Registration)` | `0x7d0450f0` | was `0x7a23df1d` for the 4-field tuple |
| `register(Registration)` | `0x4e47e64b` | payable; was `0xb26675d5` |
| `commit(bytes32)` | `0xf14fcbc8` | unchanged |
| `priceWithoutCheck(string,address)` | `0xdcd62573` | unchanged selector, returns `PriceWithMeta(price,status,userStatus,message)` |
| `pricingVersion()` | `0xe44ce5a7` | on `POP_RULES` |

- Sending the **old 4-field tuple hits no function at all**: the dry-run comes back `flags: 1, data: ""` (empty revert) while `minCommitmentAge()` still answers, which reads like a dead contract but is purely a selector mismatch. That was dotkit ≤0.2.4's bug.
- `maxPrice` is a **slippage ceiling**, price + 10% (matching `@parity/dotns-cli`'s `MAX_PRICE_SLIPPAGE_PERCENT`). `pricingVersion` is the cost model's content hash — **not** a counter — read from `POP_RULES.pricingVersion()`, which returns the same value as `DotnsCostModelRegistry.currentVersion()` (verified live 2026-09-04).
- Both fields are part of the **commitment preimage**, so `makeCommitment` and `register` must be given one identical tuple or the reveal never matches.
- The payable `register` is charged the **quoted price exactly** (10 PAS open tier), not the ceiling. There is no +10% payment margin — that margin moved into `maxPrice`. Paying 0 reverts `0x11011294`.
- Commitment window is `minCommitmentAge` 6s to `maxCommitmentAge` **86400s**, so a slow reveal is never the problem.

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

[[executables]]            # optional; publishes to app.<domain>
kind = "app"
path = "dist/app"
app_version = [0, 0, 1]
runtime = "web"            # presence of `runtime` selects the v2 app manifest
entrypoint = "index.html"

[[executables]]            # optional; publishes to worker.<domain>
kind = "worker"
path = "dist/worker"
app_version = [0, 0, 1]
entrypoint = "index.js"
includes = { chat = true, pocket = false }
```

Each `[text]` entry is written via `setText` after the bind. The build dir is never scanned for the config (its files get uploaded).

**`[product]` (generated root manifest).** When `[product]` is present, `deploy` uploads the `icon` to Bulletin (single blob, ≤2 MiB, **blake2b-256** multihash — the host's Browse/preimage icon resolver requires it; a sha2-256 icon CID resolves on the IPFS gateway but Browse renders the fallback identicon), builds the RFC root manifest `{"$v":1,"displayName","description","icon":{"cid","format"}}`, and writes it as the base name's `manifest` text record — so Browse shows a name + icon, not just a resolvable contenthash. The app's contenthash stays the SPA entry point; `[product]` alone creates no `app.<name>` subname or `executable` records — add `[[executables]]` for multi-surface app/worker products (see Executables below). `icon.format` is inferred from the extension (`.png`→`png`, `.jpg`/`.jpeg`→`jpeg`); other extensions are rejected. `[product]` **generates** the `manifest` record, so a config that also sets a manual `[text].manifest` is rejected as a conflicting source of truth. Writing the manifest is automatic on deploy; Browse discovery still needs explicit `--publish` (personhood-gated + rate-limited).

## Executables (App + Worker)

`[[executables]]` publishes multi-surface products: each entry gets its own subdomain (`app.<domain>`, `worker.<domain>`) carrying an `executable` text record plus a contenthash. The base name keeps the website contenthash and the `manifest` record, so a site and its executables coexist.

**Two content models, one resolver.** They are indistinguishable by CID — both CIDv1/dag-pb/sha2-256 — but they are not the same kind of object:

- **Website** (the `<dir> <domain>` argument): the bound CID *is* the UnixFS directory root, so a gateway serves `index.html` from it.
- **Executable** (`[[executables]]`): the directory DAG is serialized to a **CARv1 archive**, and the bound CID is that archive stored **as a chunked file**. Fetching `index.js` under it fails (`no link named "index.js"`) — that is expected. The consumer is the Store, which downloads the whole archive and imports it; the archive's inner root is the real directory.

```sh
# Website + both executables in one run (needs a deploy.toml with [[executables]])
dotkit deploy ./dist myapp.paseo
```

Per executable, `deploy` merkleizes the build dir, wraps it in a CAR, chunks the archive into raw (`0x55`, sha2-256) leaves under a dag-pb UnixFS file root, and uploads **only the chunks + that root** (the inner directory blocks ride inside the archive bytes). Chain writes then go out as **two atomic `Utility.batch_all` groups**:

1. `setSubnodeOwner` + `setResolver` — so the subnode is never left owned-but-unresolvable.
2. `setText("executable")` + `setContenthash` — so a consumer never sees content with no record describing how to run it, or vice versa.

Both groups are read-back verified, and both are **skipped when the chain already holds the wanted state**, so re-running a deploy where only one executable changed writes nothing for the other.

**Record shapes** (key order is part of the record):

| Entry | Generated `executable` record |
|---|---|
| `kind = "app"` with `runtime` | `{"$v":2,"kind":"app","appVersion":[…],"runtime":{"kind":"web","entrypoint":"index.html"}}` |
| `kind = "app"` without `runtime` | `{"$v":1,"kind":"app","appVersion":[…]}` |
| `kind = "worker"` | `{"$v":1,"kind":"worker","appVersion":[…],"entrypoint":"index.js","includes":{…}}` |

- **App v2 embeds its manifest.** When `runtime` is set, the same JSON is added to the executable's DAG as `manifest.json` **before** merkleization (it changes the CID). dotkit injects it in memory, so your `dist/` is never written to — the resulting CID is identical to having the file on disk, and an existing `manifest.json` at that path is replaced.
- **`kind` is the subdomain label**, so two entries of the same kind are rejected.
- `includes` is worker-only; `runtime` is app-only; workers require `entrypoint`.
- **The subnode resolver pointer matters.** `setSubnodeOwner` mints a node with a zero resolver, and records written straight to the content resolver are unreachable until the Registry pointer is set. dotkit sets it (and repoints a wrong one) as part of publishing.
- **Chunks are CAR-section aligned.** A chunk never splits a CAR section (`varint(len) ++ cid ++ block`), and sections are packed greedily up to 2 MiB — the invariant measured on `app.jollity.paseo`, whose 25 chunk boundaries all land on section boundaries. A section wider than the budget (only reachable via `--input-car` with a near-2 MiB block) is split at byte boundaries so the leaf still fits one extrinsic.
- **Exact upstream boundaries are not reproducible.** Upstream emits whatever its CAR stream has buffered per flush — that same 19.9 MB archive gives 25 unevenly sized chunks including a lone 396-byte one — so two upstream deploys of identical content disagree with each other. dotkit packs deterministically (11 chunks for that archive). The archive bytes, inner root, leaf codec and node encoding are identical; only the split points differ, and the consumer reassembles the stream.
- **The leaf/root encoding is byte-exact with chain.** Pinned by a golden vector in `src/car.rs` that reproduces `worker.jollity.paseo`'s live contenthash, including the empty-but-present dag-pb link `Name` and the fact that a single chunk still gets a file root rather than collapsing to a raw CID.

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
- **The `setSubnodeOwner` ABI generation is detected per chain.** DotNS v0.7 added `persist` to the record, moving the selector; environments upgrade separately (2026-09-12: paseo-next-v2 on v0.7, PreviewNet on v0.6). dotkit dry-runs both shapes and uses the one the Registry implements, so subnode creation and executable publishing work on either generation with no flag. A "reverted with no reason returned" on a subnode write means neither known shape matched — that needs a dotkit update, not a retry.
- **No pricing/NFT.** A subnode is not an ERC721 name (no `register`/`transfer`/`publish` NFT semantics) — it's a directly-owned Registry node. Bind its records with the normal `name content set` / `name text set` afterward.
- **Base names only** still applies to registration and Browse: `register` mints top-level `<label>.<tld>` names, and the **Publisher** rejects subnodes — you can bind/resolve a subnode's records but you can't `publish` it to Browse.

## Diagnosing reverts

dotkit surfaces the real EVM revert reason. Map it:

- `no DotNS contracts are deployed on <env>; awaiting the post-wipe redeployment` (or `N of 6 DotNS contracts are not deployed on <env>`) → that env's DotNS suite is absent, not misconfigured. dotkit probes code-at-address **before** any ABI decoding, lists every missing contract by name and address, and refuses signed calls before spending fees. The addresses are CREATE3-deterministic so they come back unchanged and **no dotkit change is needed** — run `dotkit --env <id> asset-hub status` to watch for the redeploy. (Paseo Next v2's DotNS suite was redeployed 2026-09-01 ~16:15 UTC to the same CREATE3 addresses and is live again; the Browse **publisher** remains absent on *both* built-in envs, at both its v2 and current v3 addresses, so `--publish` is still broken everywhere.)
- `not for sale (Short names are not for sale)` in `name lookup`, or that revert from `register` → the label falls in the closed short band (6–8 chars on `.paseo`) and cannot be bought through the public registrar **at any personhood tier**; verifying does not help. Lengthen the label to 9+ chars, or obtain it through the PoP gateway (not implemented by dotkit). Not a dotkit or funding problem.
- `requires Lite/Full personhood, but the signer … has NoStatus` → the name is personhood-gated; use a verified signer (`sudo.personhood.dev/personhood-faucet`, env Next V2) or pick an open (9+ char) name. dotkit bails here **before** committing.
- `Name must have no digit suffix or exactly 2 digit suffix` → **older-profile envs only** (`preview`): rename to 0 or 2 trailing digits. DotNS v0.6.0 dropped this rule, so it no longer fires on `.paseo`.
- `custom error 0x14c417b5 …` echoing your H160 → not authorized (you don't own the node).
- `cannot publish <name>: publishing to Browse needs Lite or Full personhood …` → the Publisher gates non-owner callers; verify at `sudo.personhood.dev` (env Next V2) or publish from a verified signer.
- `cannot publish <name>: daily publish cap reached (Lite 1/day, Full 5/day); next publish allowed in ~N min …` → wait out the rolling 24h window, or use a Full-tier signer for a higher cap.
- `Revive::ContractReverted` on a **submitted** write whose dry-run passed → almost always the storage-deposit limit, not the contract's own logic. `ReviveApi.call` reports `storage_deposit` (net) and `max_storage_deposit` (peak); a call that writes then refunds peaks above its net, and a limit derived from the net is exhausted mid-execution, which the pallet reports as a revert. DotNS `register` nets 1.032 PAS of deposit but peaks at 1.4448 PAS. dotkit now limits from the peak; if you hand-build a `Revive.call`, do the same.
- `no reason returned (empty revert…)` on `register` while other reads on the same contract answer fine → a **selector mismatch**, not a dead contract. Check the `Registration` tuple against the deployed ABI (see "Registration ABI" above) and `@parity/dotns-cli`'s bundled ABI in `dist/core/index.js`.
- `custom error 0x11011294` on `register` → insufficient payment; the call value must cover the quoted price.
- `no reason returned (empty revert…)` elsewhere → often an unmapped account or an address with no code; run `account whoami` / `asset-hub map`.
- `AccountUnmapped` / "balance too low" on map → fund the signer on Asset Hub (`faucet.polkadot.io/?parachain=1500`) then `asset-hub map`.

## Host / content contract

Deployed root must be **CIDv1 / dag-pb (or raw single-file) / sha2-256** with `index.html` at the directory root — the web host fails closed on any other multihash/codec. Native merkleization already produces exactly this (Kubo default).

## Hard rules

- **Open-tier registration only** through the public registrar (Reserved rejected; the short band is unbuyable there at any tier — `Short names are not for sale`). Open tier is a flat **10 PAS** on paseo-next-v2, charged exactly (no margin). dotkit still pre-checks `personhoodStatus` and bails early if the signer's tier is too low. Short/Lite/Full names exist only via the PoP gateway, which dotkit does not implement.
- **Signed `Revive.call` limits come from the dry-run's peak, not its net.** `max_storage_deposit` ≥ `storage_deposit`; limiting to the net makes a refunding call fail as `Revive::ContractReverted` after the dry-run passed.
- **Label rules are per-generation — never hardcode them.** `.paseo` (v0.6.0) measures labels as written with no digit-suffix rule; `preview` still enforces 0-or-2 trailing digits and strips a 2-digit suffix before measuring. dotkit reads both from chain, so ask the chain instead of assuming.
- **`<name>.paseo.li`** is the v2 gateway; `<name>.dot.li` points at the dead Summit chain — never use it for v2.
- **Secrets** via `$MNEMONIC` / `$DOTNS_MNEMONIC`, not `--mnemonic` in shell history.
- **`preview` env** shares Paseo v2's CREATE3 contract set but uses the **`.testnet`** TLD since PreviewNet re-rooted its registry (`.test` → `.testnet`, around 2026-09-01). Never pass a `.paseo` name to `--env preview` (or vice versa) — the TLD is part of the namehash, so it silently targets a different node: a register succeeds under the other TLD and the ownership read comes back zero. Pass bare labels and this can't happen.
- **`InvalidTransaction::Stale` ("Transaction is outdated") is a nonce race**, not a config problem. The default signer is the shared public dev phrase, used concurrently by others and CI. Retry.
- **Name transfer** pays the registrar's quoted friction fee (0 for same-tier/upward moves, a fee for downward moves); only the current NFT owner can transfer, and the recipient `<to>` is a `0x` H160 or SS58 address.
- **Publisher (`--publish` / `name publish|unpublish`)** is per-env, owner-only, base-label-only, and personhood-gated + rate-limited. In `deploy` it's non-fatal by default (`--fail-on-publish-error` to hard-fail).
- **`bulletin authorize`** needs a signer that holds Bulletin **Authorizer** privileges; it defaults to the env's `bulletin_authorizer` and the default storage pool cannot authorize (the chain returns `BadOrigin`). The Authorizer is **env-specific** — `//Alice` on paseo-next-v2, `//Eve` on preview, where signing as `//Alice` fails `BadSigner`.
- **`InsufficientAuthorizerBudget`** means the grant exceeds what the Authorizer has left in `AllowedAuthorizers` (its own `quota` is debited per grant), not that your account is at fault. Lower `--transactions`/`--bytes`; the defaults (`1000` / `100 MB`) are sized to fit.
- **`--json`** makes every command print one JSON object to stdout (read commands like `name owner-of`/`lookup`, `bulletin verify`, `account info` are read-only and script-friendly); on failure it prints `{"error": …}` to stderr.
- **Single blob > 2 MiB** is not yet supported (`bulletin store` bails; Kubo/native chunking keeps deploy blocks ≤256 KiB).
- **Every DotNS command preflights the contracts it needs.** dotkit probes code-at-address before ABI-decoding a read or signing a write (`deploy` checks registry + controller + resolver in one burst, before uploading anything). Probes are memoized per process and never added to commands that don't touch DotNS. `asset-hub status` shows the whole picture.
- **The Browse Publisher is currently absent on both built-in envs**, so `--publish` / `name publish|unpublish` cannot work anywhere right now. dotkit pins the superseded v2 addresses (`0x1875B90A…` paseo, `0x5a3c1112…` preview); upstream `paritytech/browse` has since moved to v3.0.0 at `0x01167f228A729f8e50f18aa7189f59b659155D09` on both, which is *also* not deployed. Re-check `evm/deployments.json` before trusting `publisher` again.
- **`asset-hub name content <name>` takes the name and nothing else.** It is a clap `external_subcommand`, so any global flag written *after* it is swallowed instead of applied — put them first: `dotkit --env preview asset-hub name content myapp`. Trailing flags are now rejected loudly; before that they silently read the default env's namespace.
- **`--env` carries a matched set** — the Bulletin RPC, the DotNS **TLD** and the Asset Hub contract addresses go together; select an env, don't mix them. After a chain wipe, re-verify against `paritytech/dotns-releases` before trusting a deploy.
