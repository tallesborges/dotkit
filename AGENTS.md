# AGENTS.md

This file gives coding agents the context they need to work in this repository.

## Project

`dotkit` — a single-binary Rust CLI for the Polkadot Triangle ecosystem: Bulletin
storage + DotNS naming on Asset Hub (`pallet_revive`). The first-class command is
`dotkit deploy` (merkleize a build dir → upload to Bulletin → bind the DotNS contenthash).

## Build & verify

- `just check` is the pre-commit gate: `cargo fmt` + `cargo clippy --all-targets` + `cargo test`. Keep it warning-clean.
- dotkit talks to **live testnets** — verify behavior against the live chain + IPFS
  gateway, not assumptions. Prefer read-only `ReviveApi.call` dry-runs and gateway
  round-trips over guessing.

## Chain gotchas

- **Pinned metadata.** `chain/config.rs` static-codegens from `artifacts/paseo_next_v2_{asset_hub,bulletin}.scale`.
  If a call breaks after a runtime upgrade (subxt reports stale metadata), regenerate
  with `just metadata` — don't hand-edit the `.scale` files.
- **Metadata is pruned to the pallets we call** (Asset Hub: `Revive`, `System`, `Balances`,
  `Utility` + the `ReviveApi` runtime API; Bulletin: `TransactionStorage`, `System`,
  `Balances`, `Utility`). Asset Hub ships 94 pallets and `#[subxt::subxt]` expands every
  one into code that gets type-checked on every build, so the subset keeps a one-file
  edit at ~91s instead of ~190s. **Calling a new pallet means adding it to the `metadata`
  recipe and re-running it first** — otherwise codegen never emits that pallet and the
  call fails with a misleading `no method named ...` error.
- **`--env` is a matched set** — it selects the RPCs, the DotNS **TLD** and the Asset Hub
  contract addresses together; never mix envs. DotNS is deployed via CREATE3, so
  `paseo-next-v2` and `preview` share one address set and differ by TLD (`paseo` vs
  `testnet`) and Publisher. Every name normalization / namehash / label
  strip goes through `env.tld` (`dotns::normalize_name`, `dotns::strip_tld`) — a
  hardcoded `.dot` silently reads and writes the wrong namespace. Re-verify addresses
  **and the TLD** against `paritytech/dotns-releases` after any chain wipe: a stale TLD
  fails silently, the register lands under the other base node and the ownership read
  returns the zero address (PreviewNet, 2026-08-20). PreviewNet has since re-rooted twice
  (`.dot` → `.test` → `.testnet`, the last verified live 2026-09-10 via
  `DotnsProtocolRegistry.tld()` at `0xD19e3D0C97CF501125a04A97405e3e6592fa846E`), so treat
  a TLD as a measured value with an expiry date, not a constant.
  `env::tests::builtin_envs_pin_their_dotns_tld` is the guard.
- **Envs are data, not code** — the table lives in `assets/envs.toml`, compiled in with
  `include_str!`, and is overlaid by `~/.dotkit/envs.toml` merged **by id**: an existing
  id patches only the fields it lists, a new id adds an env. Add or fix an environment
  there rather than editing `src/env.rs`. Only ship an env in `assets/envs.toml` if it can
  be defined centrally *and* has been verified against a live chain — machine-local
  networks and ones with unpublished endpoints stay in the overlay, never as commented-out
  examples in the repo. `dotkit account env --list` shows every env and
  whether it is `builtin`, `builtin+user` or `user`. Unknown TOML keys are rejected, and
  an env only *requires* `tld` — missing endpoints error in context when a command needs
  them, so a partially-known env is still useful.
- **DotNS registration rules** — the label digit-suffix rule, the commit/reveal
  `CommitmentTooNew` timing, and the personhood (PoP) tiers — live in the
  `src/dotns/registrar_abi.rs` module docs (the register/deploy flow itself is in
  `src/dotns/names.rs`). Read them before touching the register / deploy flow.
- **`InvalidTransaction::Stale` is a nonce race, not a bug.** The default signer is the
  *shared* public dev phrase, used concurrently by other people and CI, so a signed call
  can intermittently fail `Transaction is outdated`. Retry before investigating, and never
  "fix" it by editing the transaction-extension tuples in `chain/config.rs` — measured
  2026-08-12, that tuple signs correctly on both paseo-next-v2 (17 declared extensions)
  and PreviewNet (18), because encoding is driven by the chain's metadata, not the tuple.
- **Bulletin authorization is env-specific and must never be batched.** The Authorizer
  lives in `TransactionStorage.AllowedAuthorizers` and differs per env — `//Alice` on
  paseo-next-v2, `//Eve` on PreviewNet (where `//Alice` is rejected `BadSigner`) — so it
  ships as `bulletin_authorizer` in `assets/envs.toml`, not hardcoded. Grant it with one
  **direct** `authorize_account` extrinsic per account: the feeless exemption is derived
  from the top-level call by the chain's custom `AuthorizeCall`/`ValidateAuthorizedCalls`
  extensions, so a `utility.batch_all` wrapper drops it and validation fails
  `Inability to pay some fees` (Authorizers normally hold zero balance). Grants are also
  debited from the Authorizer's own budget — oversized ones fail
  `InsufficientAuthorizerBudget`, so defaults stay at 1000 txs / 100 MB, matching
  `paritytech/bulletin-deploy`.
- **Surface real reverts.** All `Revive.call` reverts decode returndata via
  `chain::revive::revert_reason`; show the actual on-chain error, don't hardcode "probably X" hints.
- **`pallet_revive` writes** need an SS58↔H160 mapping and a successful dry-run first;
  derive weight / storage-deposit limits from the dry-run, never magic constants.
- **Websites and executables are different content models** behind one resolver, and
  their CIDs look identical (CIDv1 / dag-pb / sha2-256). A website binds the UnixFS
  *directory root* (`merkle.rs`). An executable binds a CARv1 archive of that directory
  stored as a **2 MiB-chunked UnixFS file** (`car.rs`) — so fetching a path inside an
  executable CID correctly fails with `no link named`, and only the chunks plus the
  dag-pb file root are uploaded, never the inner directory blocks. Don't "fix" one model
  by making it look like the other.
- **The executable file-root encoding is pinned by a live golden vector**, not by
  upstream source: `car.rs` tests reproduce `worker.jollity.paseo`'s on-chain contenthash
  exactly. Two details are load-bearing and silently change the CID if touched — the
  dag-pb link `Name` is **present but empty**, and a single chunk still gets a file root
  instead of collapsing to its raw leaf (which is why a small executable is `bafybei…`,
  not `bafkrei…`). Chunks are **CAR-section aligned** (never split a section; pack
  greedily to 2 MiB) — the invariant measured on `app.jollity.paseo`, whose 25 chunk
  boundaries all land on section boundaries. Upstream's *exact* boundaries are
  stream-flush dependent and not reproducible even upstream-to-upstream, so don't chase
  them; assert alignment and the budget instead. Verify a change here against the vector,
  never by reasoning about it.
- **A subnode needs its Registry resolver pointer set.** `setSubnodeOwner` mints a node
  whose resolver is the zero address; records written straight to the content resolver
  are then unreachable, because consumers ask the Registry which resolver serves a node.
  A registered base name already has the pointer (the registrar sets it), subnodes do not.
- **Executable writes go out as two atomic `Utility.batch_all` groups** —
  `setSubnodeOwner` + `setResolver`, then `setText("executable")` + `setContenthash` — so
  no consumer can observe a subnode without a resolver, or content without the record
  describing how to run it. Use `batch_all`, never `batch`: `batch` swallows an inner
  failure into an event instead of failing the extrinsic.
  `ReviveApi.call` dry-runs a single *contract call*, not an extrinsic, so a batch cannot
  be dry-run as a whole and each inner call carries its own limits. `setResolver` on a
  subnode that doesn't exist yet reverts in a dry-run (no owner ⇒ unauthorized), so its
  limits are measured against the **parent** node and widened by the `setSubnodeOwner`
  measurement to cover writing a fresh storage slot; a fresh resolver slot is one address
  word, strictly smaller than the subnode record. Limits are caps pallet_revive refunds
  down to actual usage, so widening is free — never substitute a magic constant.
- **Both groups are skipped when the chain already matches.** A rerun after a partial
  failure, or with only one executable changed, writes nothing for the rest.

## Live-write commands (don't run to "test")

The signed `just` recipes (`deploy`, `register`, `set`, `store`) and their `dotkit`
subcommands submit **real transactions to paseo-next-v2** — they register actual `.paseo`
names, spend testnet funds, and write to Bulletin. Don't run them just to check the build;
use `cargo test` or the read-only recipes (`just whoami | env | resolve | status`). The
default signer is the public dev phrase (its `//Alice` / `//deploy/N` derivations are funded on paseo-next-v2).

## Skill (keep in sync)

- The agent-facing usage doc is `skills/dotkit/SKILL.md` (single source of truth).
- Update it in the **same change** when the command/flag surface, `--env` set, signer
  model, naming/PoP rules, or revert wording changes. Match `dotkit --help`.
