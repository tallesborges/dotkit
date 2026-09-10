//! DotNS naming on Asset Hub: the resolver/registrar ABI encoders
//! ([`resolver`], [`registrar_abi`]), the high-level naming operations
//! ([`names`]) that drive registration, records, ownership, and transfers, and
//! the contract-deployment preflight ([`deployment`]).

pub mod deployment;
pub mod names;
pub mod registrar_abi;
pub mod resolver;

pub use deployment::{ensure_deployed, probe, Contract, State};
pub use names::{
    classify_name, create_subnode, ensure_domain, ensure_subnode_with_resolver, name_owner,
    name_price_native, register_name, resolve_contenthash, resolve_text, set_contenthash,
    set_executable_records, set_text, tier_name, transfer_name,
};
pub use resolver::{contenthash_to_cid, normalize_name, strip_tld};
