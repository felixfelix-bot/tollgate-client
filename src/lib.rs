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
pub mod discovery;
pub mod policy;
pub mod purchase;
pub mod scoring;
pub mod http;
pub mod runner;
