//! Antigravity source module.

mod adapter;
mod config;
mod discovery;
mod normalization;
mod project;
mod protobuf;
pub mod quota;
mod reader;
mod snapshot;
mod storage;

pub use adapter::AntigravityAdapter;
pub use config::{
    ANTIGRAVITY_CONFIG_INVALID, AntigravityConfig, AntigravityConfigError,
    AntigravityConfigResolution,
};

/// Antigravity usage parser version.
pub const ANTIGRAVITY_USAGE_PARSER_VERSION: i64 = 2;
