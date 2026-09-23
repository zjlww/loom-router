// LoomRouter - headless multi-provider gateway for coding agents.
//
// The library owns provider configuration, protocol translation, the local
// proxy, and agent catalog integration. The binary exposes it through the CLI.

pub mod claude_cli;
pub mod cli;
mod cli_locator;
pub mod codex;
pub mod config;
pub mod keypool;
pub mod network;
pub mod providers;
pub mod proxy;
pub mod secure_fs;
pub(crate) mod service;
pub mod sse;
pub mod state;
pub mod stats;
pub mod tooling;
pub mod translate;
pub mod visual;
mod wake_lock;
