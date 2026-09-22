//! Antigravity source module.

pub mod adapter;
pub mod annotation;
pub mod config;
pub mod discovery;
pub mod normalization;
pub mod project;
pub mod protobuf;
pub mod reader;
pub mod snapshot;
pub mod storage;

pub use adapter::AntigravityAdapter;
pub use config::{
    ANTIGRAVITY_CONFIG_INVALID, AntigravityConfig, AntigravityConfigError,
    AntigravityConfigResolution,
};

/// Antigravity usage parser version (fixed to 1 in Track D).
pub const ANTIGRAVITY_USAGE_PARSER_VERSION: i64 = 1;
