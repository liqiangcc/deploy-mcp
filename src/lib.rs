//! Deployment orchestration core.
//!
//! Domain and application code intentionally depend on ports, never on SSH/SFTP
//! implementations or MCP protocol details.

pub mod adapters;
pub mod application;
pub mod audit_persistence;
pub mod config;
pub mod domain;
pub mod error;
pub mod mcp;
pub mod persistence;
pub mod ports;
pub mod recovery_persistence;
pub mod rollback_persistence;
