//! The services that need no capture: the shared clipboard, the shells for
//! the KVM's terminal panel, file reception, updates from the KVM, and the
//! host info.

pub mod clipboard;
mod credit;
pub mod file_transfer;
pub mod hostinfo;
pub mod terminal;
pub mod update;
