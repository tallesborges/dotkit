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
- **`--env` is a matched set** — it selects the RPCs, the DotNS **TLD** and the Asset Hub
  contract addresses together; never mix envs. DotNS is deployed via CREATE3, so
  `paseo-next-v2` and `preview` share one address set and differ by TLD (`paseo` vs
  `test`) and Publisher. Every name normalization / namehash / label
  strip goes through `env.tld` (`dotns::normalize_name`, `dotns::strip_tld`) — a
  hardcoded `.dot` silently reads and writes the wrong namespace. Re-verify addresses
  **and the TLD** against `paritytech/dotns-releases` after any chain wipe: a stale TLD
  fails silently, the register lands under the other base node and the ownership read
  returns the zero address (PreviewNet, 2026-08-20).
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
- **Surface real reverts.** All `Revive.call` reverts decode returndata via
  `chain::revive::revert_reason`; show the actual on-chain error, don't hardcode "probably X" hints.
- **`pallet_revive` writes** need an SS58↔H160 mapping and a successful dry-run first;
  derive weight / storage-deposit limits from the dry-run, never magic constants.

## Live-write commands (don't run to "test")

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
The signed `just` recipes (`deploy`, `register`, `set`, `store`) and their `dotkit`
subcommands submit **real transactions to paseo-next-v2** — they register actual `.paseo`
names, spend testnet funds, and write to Bulletin. Don't run them just to check the build;
use `cargo test` or the read-only recipes (`just whoami | env | resolve | status`). The
default signer is the public dev phrase (its `//Alice` / `//deploy/N` derivations are funded on paseo-next-v2).

## Skill (keep in sync)

- The agent-facing usage doc is `skills/dotkit/SKILL.md` (single source of truth).
- Update it in the **same change** when the command/flag surface, `--env` set, signer
  model, naming/PoP rules, or revert wording changes. Match `dotkit --help`.
