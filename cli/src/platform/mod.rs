//! Dev platforms (design §3 `platform/`, §10): build, launch, wait for the
//! first frame, screenshot, logs and stop, one module per platform;
//! `commands/mod.rs` routes each platform's commands to its module.

pub mod desktop;
pub mod ios_device;
pub mod ios_sim;
