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

    /// Emit a *block* of already-formatted bytes on `fd` under at most one
    /// live-row suspension.
    ///
    /// Some writers are not line writers: they emit several lines, or a
    /// partial line, that must reach the terminal as one unit. Routing those
    /// per line would open one suspension per line and, with a drawn row,
    /// clear and repaint between them — different bytes. `block` is handed a
    /// writer and may write any number of lines (or none) to it.
    ///
    /// # Default implementation
    ///
    /// The default buffers the block and forwards each resulting line to
    /// [`Surface::transcript_line`]. That keeps the trait implementable by a
    /// backend that only knows about lines, but it **loses the
    /// single-suspension property**: a line-oriented backend that lifts a row
    /// per `transcript_line` will lift it once per line of the block. A
    /// backend for which that matters — any alternative backend painting over
    /// the same terminal — should override this method and bracket the whole
    /// block itself, as [`ClassicSurface`] does.
    fn write_block(&self, fd: Fd, block: &mut dyn FnMut(&mut dyn std::io::Write)) {
        let mut bytes: Vec<u8> = Vec::new();
        block(&mut bytes);
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines() {
            self.transcript_line(fd, line);
        }
    }
}

/// Does a write on `fd` share a terminal with the live status row, and so
/// require the row to be lifted around it?
///
/// The row is painted on **stderr**, so a stderr write always shares its
/// terminal with the row. A stdout write shares it only when stdout is itself
/// a styled terminal: under `kitsu chat > file` the stdout bytes land in the
/// file and cannot disturb the row, and lifting it anyway would insert a
/// clear/repaint cycle and change the emitted *stderr* bytes. That asymmetry
/// already exists in `print_fragment`; this preserves it, in one testable
/// place.
pub(crate) fn suspends_for(fd: Fd, stdout_styled: bool) -> bool {
    matches!(fd, Fd::Stderr) || stdout_styled
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

    /// The block runs against the real process descriptor, inside one
    /// suspension when [`suspends_for`] says the write shares the row's
    /// terminal and directly otherwise. One suspension for the whole block is
    /// what makes this byte-identical to the unrouted writers, which each
    /// wrapped their whole multi-line body in a single
    /// `with_live_row_suspended`.
    fn write_block(&self, fd: Fd, block: &mut dyn FnMut(&mut dyn std::io::Write)) {
        // `run` is `mut` because it mutably borrows `*block`.
        let mut run = || {
            match fd {
                Fd::Stdout => {
                    let mut out = std::io::stdout();
                    block(&mut out);
                    let _ = std::io::Write::flush(&mut out);
                }
                Fd::Stderr => {
                    let mut out = std::io::stderr();
                    block(&mut out);
                    let _ = std::io::Write::flush(&mut out);
                }
            };
        };
        if suspends_for(fd, crate::stdout_style()) {
            live::with_live_row_suspended(run);
        } else {
            run();
        }
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
