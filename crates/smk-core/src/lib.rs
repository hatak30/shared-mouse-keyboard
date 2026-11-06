//! Shared Mouse & Keyboard core engine.
//!
//! Platform-independent pieces (layout, protocol, TLS, discovery, engine) plus
//! per-OS input capture/injection backends in [`input`].

pub mod clipboard;
pub mod config;
pub mod discovery;
pub mod engine;
pub mod input;
pub mod keymap;
pub mod layout;
pub mod net;
pub mod permissions;
pub mod protocol;
pub mod tls;

pub use engine::{start, Command, EngineHandle, StartOptions, UiState};
