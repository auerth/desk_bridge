//! Windows implementations of the platform traits (`deskbridge-platform`).
//!
//! Each subsystem lives in its own module; `core` only sees the trait objects.
#![cfg(windows)]
#![allow(clippy::missing_safety_doc)]

pub mod audio;
pub mod clipboard;
pub mod display;
pub mod input;
pub mod keymap;
pub mod power;
pub mod screen;
pub mod secret;
pub mod video;
pub mod web;
