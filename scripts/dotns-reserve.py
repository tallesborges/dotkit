#!/usr/bin/env python3
"""Build (and optionally dry-run) a DotNS base-label reservation.

Reserves a base label such as `dotpulse` for a candidate account by calling
`DotnsGateway.reserve_name` through the DUB attester's proxy. The attester is
the only account on paseo-next-v2 holding `AttestationAllowance`, so this is the
only path that does not require Full personhood.

WHAT A RESERVATION DOES
  Puts the candidate at the head of the per-label reservation queue in
  `DotnsPopController`. While that head entry is live, any other account calling
  `registerBaseName(<label>)` reverts `NotHolder` — including through the
  gateway, which is the path a real verified person uses. It does NOT grant
  ownership: claiming still needs Full personhood and a People ring proof.

  Head entries expire after `reservationDuration` (604800s = 7 days on
  paseo-next-v2), so this must be re-run until personhood lands.

  RENEWAL HAZARD: `reserveBaseName` removes the caller from the queue and
  re-appends at the TAIL. Renewing while another account is queued behind you
  hands them the head. Always check `queue depth` in the preflight output and
  only renew when you are the sole entry.

  The candidate MUST be the account that later completes personhood and calls
  `register_name` — `registerBaseName` compares the claimant's H160 against the
  queue head owner. Reserving to an account that can never obtain personhood
  parks the label behind a head you cannot use.

SECRETS
  The candidate key is ours (`~/.dotkit/dotpulse-owner.env`). The submitting key
  is DUB's production chain-writer SURI, which lives only in
  `/home/ubuntu/dub-paseo-next-v2/.env` on the `ibv2` host. This script never
  takes a SURI on the command line and never prints one: it takes the *name* of
  an environment variable and hands that to `dot`, which keeps the key off disk.

Usage:
  scripts/dotns-reserve.py preflight --label dotpulse
  scripts/dotns-reserve.py build --label dotpulse --candidate-env DOTNS_MNEMONIC
  scripts/dotns-reserve.py build --label dotpulse --candidate-env DOTNS_MNEMONIC \\
      --writer-env CHAIN_WRITER_SIGNER_SURI --dry-run
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time

CHAIN = "paseo-next-v2-asset-hub"
ETH_RPC = "https://eth-rpc-paseo-next.polkadot.io"

# indiv_pallet_dotns_gateway::RESERVE_MSG_PREFIX
RESERVE_MSG_PREFIX = b"pop:dotns-gateway:reserve"

# DUB attester authority: the sole holder of DotnsGateway::AttestationAllowance.
ATTESTER = "5GCF223UbXNZiH78V6DhsWeZ6hrQ6xRfGTfdn6MraprJKbWj"
# Any/delay-0 proxy of the attester; DUB's live chain-writer signs as this.
WRITER = "5Fxu2qzdqo1P1E3GGdiyVd8rtqGg185zYN6ssNdPvNRsksv1"

POP_CONTROLLER = "0xCC932348606cc1f3318cADeC5A5Cd2CA447f8a4b"
POP_RULES = "0x747B456bE03aec0b42bd85C51513730FBD45DA31"


def run(cmd: list[str], **kw) -> str:
    out = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if out.returncode != 0:
        sys.exit(f"command failed: {' '.join(cmd[:3])}…\n{out.stderr.strip()}")
    return out.stdout.strip()


# ---------------------------------------------------------------- SCALE codec


def compact(n: int) -> bytes:
    """SCALE compact integer."""
    if n < 0:
        raise ValueError("compact is unsigned")
    if n < 0x40:
        return bytes([n << 2])
    if n < 0x4000:
        return ((n << 2) | 0b01).to_bytes(2, "little")
    if n < 0x40000000:
        return ((n << 2) | 0b10).to_bytes(4, "little")
    body = n.to_bytes((n.bit_length() + 7) // 8, "little")
    return bytes([((len(body) - 4) << 2) | 0b11]) + body


def scale_bytes(b: bytes) -> bytes:
    """A `&[u8]` / `Vec<u8>` field: compact length then the raw bytes."""
    return compact(len(b)) + b


def scale_option_bytes(b: bytes | None) -> bytes:
    return b"\x00" if b is None else b"\x01" + scale_bytes(b)


def account_pubkey(ss58: str) -> bytes:
    info = json.loads(run(["dot", "account", "inspect", ss58, "--json"]))
    pk = info.get("publicKey") or info.get("public_key")
    if not pk:
        sys.exit(f"could not resolve public key for {ss58}")
    return bytes.fromhex(pk[2:])


def reservation_message(
    candidate: str, attester: str, lite_base: bytes, chat_key: bytes,
    reserved_base: bytes | None, signed_at: int,
) -> bytes:
    """Mirror of `Pallet::construct_reservation_message`.

    SCALE tuple: (prefix, candidate, attester, lite_base, chat_key, Option<base>, signed_at).
    AccountId32 encodes as 32 raw bytes; every byte-slice field is length-prefixed;
    `chat_key` is passed as `.as_slice()` in the pallet, so it is length-prefixed too.
    """
    if len(chat_key) != 65:
        sys.exit(f"chat_key must be 65 bytes, got {len(chat_key)}")
    return (
        scale_bytes(RESERVE_MSG_PREFIX)
        + account_pubkey(candidate)
        + account_pubkey(attester)
        + scale_bytes(lite_base)
        + scale_bytes(chat_key)
        + scale_option_bytes(reserved_base)
        + signed_at.to_bytes(8, "little")
    )


# ------------------------------------------------------------------ preflight


def eth_call(to: str, data: str) -> dict:
    payload = json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": "eth_call",
        "params": [{"to": to, "data": data}, "latest"],
    })
    return json.loads(run(["curl", "-sS", "-X", "POST", ETH_RPC,
                           "-H", "Content-Type: application/json", "-d", payload]))


def abi_string(s: str) -> str:
    b = s.encode()
    return (len(b).to_bytes(32, "big").hex() + b.hex()
            + "00" * ((32 - len(b) % 32) % 32))


def preflight(label: str) -> None:
    print(f"== preflight: {label} ==")

    allowance = json.loads(run([
        "dot", f"{CHAIN}.query.DotnsGateway.AttestationAllowance", "--dump", "--json"]))
    holder = allowance[0]["keys"][0] if allowance else None
    print(f"  attestation allowance : {allowance[0]['value'] if allowance else 0} "
          f"(holder {holder})")

    dur = eth_call(POP_CONTROLLER, "0xbfe2c974")
    if "result" in dur:
        secs = int(dur["result"], 16)
        print(f"  reservation duration  : {secs}s ({secs / 86400:.0f} days)")

    # PopRules.isBaseNameReserved(string) -> (bool live, address owner, uint)
    off = (32).to_bytes(32, "big").hex()
    res = eth_call(POP_RULES, "0x91809994" + off + abi_string(label))
    if "result" in res:
        raw = res["result"][2:]
        live = int(raw[0:64], 16) != 0
        owner = "0x" + raw[64 + 24:128]
        print(f"  base name reserved    : {live}  head owner {owner}")
        if live:
            print("  !! label already has a live reservation head — do NOT renew blindly;")
            print("     re-appending puts you behind the current head.")
    else:
        print(f"  base name reserved    : query failed {res}")

    for who, name in ((ATTESTER, "attester"), (WRITER, "writer")):
        acct = json.loads(run([
            "dot", f"{CHAIN}.query.System.Account", who, "--json"]))
        print(f"  {name:<21} : nonce {acct['nonce']}, "
              f"free {int(acct['data']['free']) / 1e10:.4f} PAS")
    print("  NOTE: the writer is DUB's live chain-writer. It caches its nonce "
          "(`next_nonce_ah`),\n        so an out-of-band submit from that account can "
          "fail one in-flight\n        attestation write. Prefer a quiet window.")


# ---------------------------------------------------------------------- build


def build(args: argparse.Namespace) -> None:
    label = args.label
    lite_label = args.lite_label or f"{label}.01"
    lite_base = lite_label.split(".")[0].encode()
    chat_key = bytes.fromhex(args.chat_key[2:]) if args.chat_key else (b"\x04" + b"\x00" * 64)
    signed_at = args.signed_at or int(time.time())

    candidate_suri = os.environ.get(args.candidate_env)
    if not candidate_suri:
        sys.exit(f"${args.candidate_env} is not set (candidate key)")

    # Load the candidate into `dot` from the environment so the key stays off disk.
    acct = "dotns-reserve-candidate"
    subprocess.run(["dot", "account", "rm", acct], capture_output=True)
    run(["dot", "account", "add", acct, "--env", args.candidate_env])
    candidate = json.loads(run(["dot", "account", "inspect", acct, "--json"]))["ss58"]

    print(f"== build ==")
    print(f"  candidate       : {candidate}")
    print(f"  attester        : {ATTESTER}")
    print(f"  lite_label      : {lite_label}  (stem {lite_base.decode()!r})")
    print(f"  reserved_base   : {label}")
    print(f"  chat_key        : 0x{chat_key.hex()[:16]}… ({len(chat_key)} bytes)")
    print(f"  signed_at       : {signed_at} ({time.strftime('%FT%TZ', time.gmtime(signed_at))})")

    msg = reservation_message(candidate, ATTESTER, lite_base, chat_key,
                              label.encode(), signed_at)
    print(f"  message         : 0x{msg.hex()}")

    sig = json.loads(run(["dot", "sign", "0x" + msg.hex(), "--from", acct,
                          "--output", "json"]))
    sig_hex = sig.get("signature") or sig.get("sig")
    print(f"  candidate_sig   : {sig_hex}")

    inner = (f"tx.DotnsGateway.reserve_name '{candidate}' "
             f"'{{\"type\":\"Sr25519\",\"value\":\"{sig_hex}\"}}' "
             f"'{lite_label}' '0x{chat_key.hex()}' '{label}' {signed_at}")

    print()
    print("== dry-run (no state change; requires the writer SURI in the env) ==")
    print(f"  ${args.writer_env} must be exported on the host that signs.")
    print(f"  dot account add dub-writer --env {args.writer_env}")
    print(f"  dot tx.Proxy.proxy '{ATTESTER}' null \\")
    print(f"      '{inner}' \\")
    print(f"      --from dub-writer --chain {CHAIN} --dry-run")
    print()
    print("== submit (ONLY after an explicit go) ==")
    print("  … same command with --dry-run replaced by nothing")
    print()
    print("== verify it landed ==")
    print(f"  scripts/dotns-reserve.py preflight --label {label}")
    print(f"  # expect: base name reserved : True  head owner <candidate H160>")

    subprocess.run(["dot", "account", "rm", acct], capture_output=True)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    pf = sub.add_parser("preflight", help="read-only queue/allowance/nonce state")
    pf.add_argument("--label", required=True)

    b = sub.add_parser("build", help="construct the signed candidate message + commands")
    b.add_argument("--label", required=True)
    b.add_argument("--lite-label", help="defaults to <label>.01")
    b.add_argument("--chat-key", help="0x-prefixed 65 bytes; defaults to 0x04||zeros")
    b.add_argument("--signed-at", type=int, help="unix seconds; defaults to now")
    b.add_argument("--candidate-env", default="DOTNS_MNEMONIC")
    b.add_argument("--writer-env", default="CHAIN_WRITER_SIGNER_SURI")

    a = p.parse_args()
    if a.cmd == "preflight":
        preflight(a.label)
    else:
        build(a)


if __name__ == "__main__":
    main()
