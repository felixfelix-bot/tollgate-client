//! TollGate client core.
//!
//! Shells (Android/Kotlin, Debian daemon) wrap this crate; nothing in here may
//! depend on a platform API. The scoring and policy modules are protocol-free:
//! they take an offer plus that gateway's measured history and answer "buy or
//! not, from whom, how much".
//!
//! Protocol (discovery/purchase) lands in later modules; see
//! docs/client/v1-client-contract.md.
pub mod agent;
#[cfg(feature = "persistence")]
pub mod budget_store;
#[cfg(feature = "cdk")]
pub mod cdk_wallet;
pub mod discovery;
pub mod history;
pub mod http;
#[cfg(feature = "android")]
pub mod jni;
pub mod json;
pub mod lightning;
mod mint_identity;
pub mod netcfg;
#[cfg(all(
    feature = "cdk",
    feature = "persistence",
    any(target_os = "linux", target_os = "android")
))]
pub mod offline_policy;
pub mod policy;
pub mod purchase;
pub mod runner;
pub mod scoring;
#[cfg(feature = "persistence")]
pub mod send_state;
#[cfg(all(feature = "cdk", any(target_os = "linux", target_os = "android")))]
pub mod token_vault;
#[cfg(all(
    feature = "cdk",
    feature = "persistence",
    any(target_os = "linux", target_os = "android")
))]
pub mod validated_send;
#[cfg(feature = "cdk")]
pub mod validated_token;
