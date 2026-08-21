//! Private external-tool runtime mechanisms.
//!
//! E06 adds independently reviewed authority, lifecycle, inventory, and
//! protocol mechanisms here instead of creating a general plugin API.

pub(crate) mod authority;
#[cfg(target_os = "linux")]
mod backend;
pub(crate) mod identity;
mod lifecycle;
mod publication;
mod serial;
mod supervisor;
