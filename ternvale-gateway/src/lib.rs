//! HTTPS gateway for the Step 83 control API (`ternvaled`).
//!
//! Talks to VMs only through `run/api.sock`. Full remote display streaming is
//! not implemented (see `docs/GATEWAY.md`).

#![forbid(unsafe_code)]

pub mod audit;
pub mod auth;
pub mod certs;
pub mod client;
pub mod config;
pub mod error;
pub mod extras;
pub mod http;
pub mod hub;
pub mod png;
pub mod rate;
pub mod routes;
pub mod schema;
pub mod serve;
pub mod tls;
pub mod ws;

pub use auth::{Scope, TokenSecret, TokenStore};
pub use client::GatewayClient;
pub use config::GatewayConfig;
pub use error::GatewayError;
pub use serve::Gateway;

#[cfg(test)]
mod https_tests;
#[cfg(test)]
mod quality;
#[cfg(test)]
mod quality_fuzz;
