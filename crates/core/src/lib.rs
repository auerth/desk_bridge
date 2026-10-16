//! Desk Bridge core: configuration, session orchestration, state machines and the UI protocol.
//!
//! Everything here is platform independent; OS access comes in through [`platform::Platform`].

pub mod backoff;
pub mod brand;
pub mod clock;
pub mod config;
pub mod diagnostics;
pub mod engine;
pub mod ipc;
pub mod ipc_server;
pub mod logging;
pub mod netaddr;
pub mod paths;
pub mod platform;
pub mod procstats;
pub mod zipwriter;
