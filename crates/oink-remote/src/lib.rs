//! Remote desktop for use over a private network such as Tailscale.
//!
//! The host captures one monitor, sends the parts of the picture that changed
//! as JPEG rectangles and injects the keyboard and mouse events it receives.
//! The viewer shows the picture in a window and sends its input. The two talk
//! over one TCP connection with their own small protocol ([`proto`]); both
//! sides prove knowledge of a shared access key first ([`auth`]). The network
//! underneath (Tailscale) provides reachability and encryption.
//!
//! This connection is separate from Oink's local sharing network: it needs no
//! discovery and no device group, only an address and the access key.

pub mod auth;
pub mod client;
pub mod codec;
pub mod config;
pub mod host;
pub mod keymap;
pub mod proto;
pub mod scale;

#[cfg(windows)]
pub mod windows_host;

#[cfg(any(target_os = "macos", windows))]
pub mod viewer;
