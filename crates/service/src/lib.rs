//! Desk Bridge Windows service (milestone M6).
//!
//! * [`protocol`] (portable) – wire format of the helper and service pipes; the agent depends on
//!   this crate as a library for the types and the blocking [`client`].
//! * the supervisor ([`supervisor`], [`launch`], [`session`]) keeps the agent alive in the console
//!   session and the secure-desktop helper running on `winsta0\Winlogon`;
//! * the helper ([`helper`], [`helper_proc`], [`desktop`]) injects input on the secure desktop;
//! * [`control`] serves `ping` / `send_sas` / `helper_status`;
//! * [`winsvc`] is the raw SCM glue, [`runtime`] the SCM-independent core (used by `--console`).
//!
//! See `docs/adr/0006-service-and-secure-desktop.md` for the design and its risks.

#![cfg_attr(not(windows), allow(unused))]
#![allow(clippy::missing_safety_doc)]

pub mod cli;
pub mod protocol;

#[cfg(windows)]
pub mod cable_mic;
#[cfg(windows)]
pub mod client;
#[cfg(windows)]
pub mod control;
#[cfg(windows)]
pub mod desktop;
#[cfg(windows)]
pub mod helper;
#[cfg(windows)]
pub mod helper_proc;
#[cfg(windows)]
pub mod launch;
#[cfg(windows)]
pub mod logging;
#[cfg(windows)]
pub mod pipe_security;
#[cfg(windows)]
pub mod pipe_server;
#[cfg(windows)]
pub mod runtime;
#[cfg(windows)]
pub mod session;
#[cfg(windows)]
pub mod supervisor;
#[cfg(windows)]
pub mod winsvc;
#[cfg(windows)]
pub mod winutil;
