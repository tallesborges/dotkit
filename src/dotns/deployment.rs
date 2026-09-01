//! Whether an environment's DotNS contracts are actually deployed.
//!
//! DotNS lives in `pallet_revive` contracts, and `pallet_revive` treats a call
//! to an address with no code as a successful no-op returning zero bytes. Every
//! read then fails while ABI-decoding the absent return value ("buffer overrun")
//! and every write silently submits nothing, so an environment whose contracts
//! were never (re)deployed looks like a broken CLI rather than an empty chain.
//! Paseo Next v2 has been in exactly that state since the 2026-09-01 wipe.
//!
//! The addresses are CREATE3-deterministic, so a redeploy restores the same
//! values dotkit already ships — an absent suite is something to wait out, not
//! something to reconfigure, and the error says so.
//!
//! Probes are memoized per address for the life of the process: dotkit runs one
//! command per process, so a command that touches several contracts pays at most
//! one `ReviveApi.code` round trip per distinct address, and the batched
//! [`ensure_deployed`] collapses even that into a single concurrent burst.

use crate::chain::config::AssetHubConfig;
use crate::chain::revive::{code_len, parse_h160};
use crate::env::Env;
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use subxt::OnlineClient;

/// A DotNS contract dotkit talks to, as configured per environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Contract {
    Registry,
    Registrar,
    RegistrarController,
    ContentResolver,
    PopRules,
    Publisher,
}

impl Contract {
    /// Every contract dotkit knows about, in report order.
    pub const ALL: [Contract; 6] = [
        Contract::Registry,
        Contract::Registrar,
        Contract::RegistrarController,
        Contract::ContentResolver,
        Contract::PopRules,
        Contract::Publisher,
    ];

    /// Human-facing name, matching how the env table and docs refer to it.
    pub fn label(self) -> &'static str {
        match self {
            Contract::Registry => "registry",
            Contract::Registrar => "registrar",
            Contract::RegistrarController => "registrar controller",
            Contract::ContentResolver => "content resolver",
            Contract::PopRules => "pop rules",
            Contract::Publisher => "publisher",
        }
    }

    /// Short column key for tabular output, within `ui`'s key width.
    pub fn key(self) -> &'static str {
        match self {
            Contract::Registry => "registry",
            Contract::Registrar => "registrar",
            Contract::RegistrarController => "controller",
            Contract::ContentResolver => "resolver",
            Contract::PopRules => "pop_rules",
            Contract::Publisher => "publisher",
        }
    }

    /// The env's configured address, or `""` when this env doesn't define one.
    pub fn address(self, env: &Env) -> &str {
        match self {
            Contract::Registry => &env.registry,
            Contract::Registrar => &env.registrar,
            Contract::RegistrarController => &env.registrar_controller,
            Contract::ContentResolver => &env.dotns_content_resolver,
            Contract::PopRules => &env.pop_rules,
            Contract::Publisher => &env.publisher,
        }
    }
}

/// What a probe found for one contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Code is present at the configured address.
    Deployed,
    /// The address is configured but holds no code.
    Absent,
    /// This env defines no address for the contract.
    Unconfigured,
}

/// One contract's configured address and live deployment state.
#[derive(Debug, Clone)]
pub struct Status {
    pub contract: Contract,
    pub address: String,
    pub state: State,
}

fn cache() -> &'static Mutex<HashMap<[u8; 20], bool>> {
    static CACHE: OnceLock<Mutex<HashMap<[u8; 20], bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Memoized `ReviveApi.code` presence check. The lock is never held across an
/// await, so a concurrent burst may probe the same address twice at most.
async fn has_code(client: &OnlineClient<AssetHubConfig>, address: &str) -> Result<bool> {
    let addr = parse_h160(address)?;
    if let Some(hit) = cache().lock().expect("code cache poisoned").get(&addr.0) {
        return Ok(*hit);
    }
    let deployed = code_len(client, addr).await? > 0;
    cache()
        .lock()
        .expect("code cache poisoned")
        .insert(addr.0, deployed);
    Ok(deployed)
}

/// Probe `contracts` concurrently, so a multi-contract command costs roughly one
/// round trip rather than one per address.
pub async fn probe(
    client: &OnlineClient<AssetHubConfig>,
    env: &Env,
    contracts: &[Contract],
) -> Result<Vec<Status>> {
    let checks = contracts.iter().map(|&contract| async move {
        let address = contract.address(env);
        if address.is_empty() {
            return Ok::<_, anyhow::Error>(Status {
                contract,
                address: String::new(),
                state: State::Unconfigured,
            });
        }
        let state = if has_code(client, address).await? {
            State::Deployed
        } else {
            State::Absent
        };
        Ok(Status {
            contract,
            address: address.to_string(),
            state,
        })
    });
    futures::future::try_join_all(checks).await
}

/// Render the actionable error for a probe result, or `None` when nothing that
/// was asked for is missing.
///
/// Kept free of chain access so both the present and absent paths are unit
/// testable. An `Unconfigured` contract is not reported here — the command that
/// needs it already fails by name, and reporting it as "not deployed" would
/// blame the chain for a gap in the env table.
pub fn absent_report(env_id: &str, statuses: &[Status]) -> Option<String> {
    let absent: Vec<&Status> = statuses
        .iter()
        .filter(|s| s.state == State::Absent)
        .collect();
    if absent.is_empty() {
        return None;
    }
    let probed = statuses
        .iter()
        .filter(|s| s.state != State::Unconfigured)
        .count();

    let headline = if absent.len() == probed {
        format!("no DotNS contracts are deployed on {env_id}; awaiting the post-wipe redeployment")
    } else {
        format!(
            "{} of {probed} DotNS contracts are not deployed on {env_id}",
            absent.len()
        )
    };
    let missing = absent
        .iter()
        .map(|s| format!("\n  missing: {} {}", s.contract.label(), s.address))
        .collect::<String>();
    Some(format!(
        "{headline}{missing}\n  These addresses are CREATE3-deterministic, so they come back \
         unchanged once they are redeployed — no dotkit change is needed.\n  Check with \
         `dotkit --env {env_id} asset-hub status`."
    ))
}

/// Fail fast unless every contract in `contracts` holds code on `env`.
///
/// On a miss the probe widens to [`Contract::ALL`] before reporting, so the
/// message describes the environment rather than just the one contract this
/// command happened to need first — otherwise a command that only reads the
/// registry would claim the whole suite was missing. Widening is memoized and
/// only happens on the failure path, so the success path still costs one burst.
pub async fn ensure_deployed(
    client: &OnlineClient<AssetHubConfig>,
    env: &Env,
    contracts: &[Contract],
) -> Result<()> {
    let statuses = probe(client, env, contracts).await?;
    if statuses.iter().all(|s| s.state != State::Absent) {
        return Ok(());
    }
    let full = probe(client, env, &Contract::ALL).await?;
    match absent_report(&env.id, &full) {
        Some(report) => bail!("{report}"),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(contract: Contract, state: State) -> Status {
        Status {
            contract,
            address: match state {
                State::Unconfigured => String::new(),
                _ => "0xf34054fd76bbf85f216cf9908226d5f0a72e50ca".to_string(),
            },
            state,
        }
    }

    #[test]
    fn contracts_present_report_nothing() {
        let statuses = vec![
            status(Contract::Registry, State::Deployed),
            status(Contract::ContentResolver, State::Deployed),
        ];
        assert!(absent_report("preview", &statuses).is_none());
    }

    #[test]
    fn all_absent_reads_as_a_missing_deployment() {
        let statuses = vec![
            status(Contract::Registry, State::Absent),
            status(Contract::ContentResolver, State::Absent),
        ];
        let report = absent_report("paseo-next-v2", &statuses).expect("should report");
        assert!(
            report.starts_with("no DotNS contracts are deployed on paseo-next-v2"),
            "{report}"
        );
        assert!(
            report.contains("awaiting the post-wipe redeployment"),
            "{report}"
        );
        assert!(report.contains("missing: registry 0xf34054fd"), "{report}");
        assert!(report.contains("missing: content resolver"), "{report}");
        assert!(report.contains("asset-hub status"), "{report}");
    }

    #[test]
    fn a_partial_outage_counts_rather_than_claiming_none_exist() {
        let statuses = vec![
            status(Contract::Registry, State::Deployed),
            status(Contract::Publisher, State::Absent),
        ];
        let report = absent_report("preview", &statuses).expect("should report");
        assert!(
            report.starts_with("1 of 2 DotNS contracts are not deployed on preview"),
            "{report}"
        );
        assert!(report.contains("missing: publisher"), "{report}");
    }

    #[test]
    fn unconfigured_contracts_are_not_blamed_on_the_chain() {
        let statuses = vec![
            status(Contract::Registry, State::Deployed),
            status(Contract::Publisher, State::Unconfigured),
        ];
        assert!(absent_report("mynet", &statuses).is_none());
    }

    #[test]
    fn unconfigured_entries_are_excluded_from_the_probed_total() {
        let statuses = vec![
            status(Contract::Registry, State::Absent),
            status(Contract::Publisher, State::Unconfigured),
        ];
        let report = absent_report("mynet", &statuses).expect("should report");
        // One configured contract, one absent: that is a total outage, not "1 of 2".
        assert!(
            report.starts_with("no DotNS contracts are deployed on mynet"),
            "{report}"
        );
    }

    #[test]
    fn every_contract_has_a_label_and_a_column_key_that_fits() {
        for contract in Contract::ALL {
            assert!(!contract.label().is_empty());
            // `ui::kv` pads keys to 10 columns; a longer key breaks the table.
            assert!(
                contract.key().len() <= 10,
                "{} key too wide: {}",
                contract.label(),
                contract.key()
            );
        }
    }
}
