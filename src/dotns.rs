use alloy_primitives::keccak256;
use alloy_sol_types::{sol, SolCall};
use anyhow::{Context, Result};
use cid::Cid;

sol! {
    function contenthash(bytes32 node) external view returns (bytes);
}

/// EIP-137 ENS namehash of a (already normalized) dotted name.
pub fn namehash(name: &str) -> [u8; 32] {
    let mut node = [0u8; 32];
    if name.is_empty() {
        return node;
    }
    for label in name.rsplit('.') {
        let label_hash = keccak256(label.as_bytes());
        let mut buf = [0u8; 64];
        buf[..32].copy_from_slice(&node);
        buf[32..].copy_from_slice(label_hash.as_slice());
        node = keccak256(buf).0;
    }
    node
}

/// ABI-encode a `contenthash(bytes32 node)` resolver call.
pub fn encode_contenthash_call(node: [u8; 32]) -> Vec<u8> {
    contenthashCall { node: node.into() }.abi_encode()
}

/// ABI-decode the `bytes` returned by a `contenthash` call.
pub fn decode_contenthash_return(data: &[u8]) -> Result<Vec<u8>> {
    let bytes = contenthashCall::abi_decode_returns(data)
        .context("ABI-decoding contenthash return failed")?;
    Ok(bytes.to_vec())
}

/// Decode an EIP-1577 IPFS contenthash into its CIDv1 (base32) string.
pub fn contenthash_to_cid(contenthash: &[u8]) -> Result<String> {
    let cid_bytes = contenthash
        .strip_prefix(&[0xe3, 0x01])
        .context("contenthash is not an EIP-1577 IPFS record (expected 0xe301 prefix)")?;
    let cid = Cid::try_from(cid_bytes).context("failed to parse CID from contenthash")?;
    Ok(cid.to_string())
}

/// Normalize a user-supplied name: lowercase and ensure a trailing `.dot`.
pub fn normalize_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    if lower.ends_with(".dot") {
        lower
    } else {
        format!("{lower}.dot")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(s: &str) -> [u8; 32] {
        let bytes = hex::decode(s).unwrap();
        bytes.as_slice().try_into().unwrap()
    }

    #[test]
    fn namehash_empty_is_zero() {
        assert_eq!(namehash(""), [0u8; 32]);
    }

    #[test]
    fn namehash_eth() {
        assert_eq!(
            namehash("eth"),
            hex32("93cdeb708b7545dc668eb9280176169d1c33cfd8ed6f04690a0bcc88a93fc4ae")
        );
    }

    #[test]
    fn namehash_host_playground_dot() {
        assert_eq!(
            namehash("host-playground.dot"),
            hex32("99bc92db900deaafbbe9bcc98e6fca316302515220c2729a861d590cc6b1926a")
        );
    }

    #[test]
    fn normalize_appends_dot() {
        assert_eq!(normalize_name("host-playground"), "host-playground.dot");
        assert_eq!(normalize_name("HOST-Playground.DOT"), "host-playground.dot");
    }
}
