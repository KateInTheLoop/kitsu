//! Flag-carrying `renameat2` for Linux, usable from every libc flavour.
//!
//! The `libc` crate only exposes `renameat2` and its `RENAME_*` constants for
//! glibc targets. Calling them directly builds on `*-linux-gnu` and fails to
//! compile on `*-linux-musl`, which is what the release pipeline actually ships
//! — the break is invisible to a gnu-only CI job.
//!
//! Both problems are avoided by issuing the syscall directly and defining the
//! flags here. The values are stable kernel UAPI (`include/uapi/linux/fs.h`),
//! unchanged since `renameat2` landed in Linux 3.15, and are not a libc detail.

/// Fail with `EEXIST` rather than replacing an existing target.
pub const RENAME_NOREPLACE: libc::c_uint = 1;
/// Atomically swap the two paths; both must exist.
pub const RENAME_EXCHANGE: libc::c_uint = 2;

/// `renameat2(2)`. Returns the raw errno failure as an [`std::io::Error`].
///
/// # Safety
///
/// Both directory descriptors must be open, and both names must be valid
/// NUL-terminated paths, for the duration of the call — the same contract the
/// libc binding carries.
pub unsafe fn renameat2(
    old_dir: libc::c_int,
    old: *const libc::c_char,
    new_dir: libc::c_int,
    new: *const libc::c_char,
    flags: libc::c_uint,
) -> std::io::Result<()> {
    // `syscall` is variadic, so every argument is passed at its natural width;
    // the descriptors and flags are widened explicitly to match the kernel ABI.
    let result = libc::syscall(
        libc::SYS_renameat2,
        old_dir as libc::c_long,
        old,
        new_dir as libc::c_long,
        new,
        flags as libc::c_long,
    );
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
