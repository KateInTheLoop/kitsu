//! The renderer's output seam (T0-S3a).
//!
//! Every transcript line kitsu prints is a byte contract, so the seam is
//! deliberately thin: the pure formatters in [`crate`] produce the line's
//! bytes and a [`Surface`] decides where those bytes land. The classic
//! terminal backend ([`ClassicSurface`]) does exactly what the writers did
//! before the seam existed — lift the live row, `eprintln!` the line — so
//! routing a writer through the seam cannot change a single byte.

use std::sync::{Arc, OnceLock, RwLock};

use crate::live;

/// Which process stream a transcript line belongs on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fd {
    /// Assistant text.
    Stdout,
    /// Meta: tool activity, results, milestones, notices.
    Stderr,
}

/// A backend for the renderer's transcript lines.
pub trait Surface: Send + Sync {
    /// Emit one already-formatted transcript line on `fd`.
    ///
    /// `text` carries the line's final bytes (styling included) and must be
    /// written verbatim, with no re-styling, trimming or re-sanitizing.
    ///
    /// An implementation must **not** call [`set_surface`] from inside this
    /// method: the slot's write lock is not reentrant, so a backend swapping
    /// itself out from within its own write would self-deadlock.
    fn transcript_line(&self, fd: Fd, text: &str);
}

/// The classic terminal backend: the live row is lifted for the write and
/// repainted after, exactly as the unrouted writers did.
pub struct ClassicSurface;

impl Surface for ClassicSurface {
    fn transcript_line(&self, fd: Fd, text: &str) {
        live::with_live_row_suspended(|| match fd {
            Fd::Stdout => println!("{text}"),
            Fd::Stderr => eprintln!("{text}"),
        });
    }
}

fn slot() -> &'static RwLock<Arc<dyn Surface>> {
    static SLOT: OnceLock<RwLock<Arc<dyn Surface>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(Arc::new(ClassicSurface)))
}

/// The installed surface, detached from the slot.
///
/// The read guard lives only for the clone and is dropped before this returns,
/// so a caller can never hold it across a [`Surface`] call. That is what keeps
/// the seam safe for reentrant suspensions, nested async notices, synchronous
/// approval prompters under `spawn_blocking` and orchestrator tokio tasks: a
/// writer holds an `Arc`, never a lock, so a concurrent [`set_surface`] cannot
/// be starved behind an in-flight write.
pub fn active_surface() -> Arc<dyn Surface> {
    let guard = slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let surface = Arc::clone(&*guard);
    drop(guard);
    surface
}

/// Install `surface`, returning the one it displaced.
pub fn set_surface(surface: Arc<dyn Surface>) -> Arc<dyn Surface> {
    let mut guard = slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::replace(&mut *guard, surface)
}
