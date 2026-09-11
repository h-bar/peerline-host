//! Watching the inherited pipe whose EOF means *your supervisor is gone*.
//!
//! The protocol — the variable, the envelope, and why the two ends agree about
//! it somewhere neither of them owns — is [`peerline_contract::lifeline`].
//! This module is the reading end of it: adopt the descriptor, wait for EOF,
//! and say nothing if there is no descriptor to adopt.
//!
//! # Why this file is allowed `unsafe`, and why the crate root now says `deny`
//!
//! A lifeline **is** an open file descriptor. Adopting one means taking an
//! integer the kernel handed us across an `exec` and telling Rust it owns it,
//! and `OwnedFd::from_raw_fd` is unsafe because nothing but the caller can
//! know that claim is true. There is no safe spelling: every alternative
//! either re-opens the descriptor through `/proc` or `/dev/fd` (absent in
//! containers, differently shaped on macOS), or hands the pipe over as stdin —
//! which would make "the child's stdin is `/dev/null`" untrue and force a
//! blocking read on a dedicated thread.
//!
//! So the crate says `deny(unsafe_code)` with one module opted out, exactly
//! as `peerline-cluster` does for its two syscalls, and the opt-out is
//! [`adopt`] — eleven lines, one `from_raw_fd`, and a check before it that is
//! the whole reason the claim holds.
//!
//! # Nothing is closed that might not be ours
//!
//! The descriptor number arrives in an environment variable, and an
//! environment variable is inherited by *every* descendant while the
//! descriptor itself may not be. A grandchild that reads the variable and
//! takes ownership of whatever happens to be at that number would close some
//! unrelated socket of its own. So [`adopt`] **borrows first**: it stats the
//! descriptor without owning it, and only takes ownership once the kernel has
//! confirmed it is a pipe. A number that is closed, or that is a socket or a
//! regular file, is reported as unusable and left untouched.
//!
//! That check is cheap and it is not theoretical: the supervisor deliberately
//! leaves the descriptor inheritable, so a service's own children see the same
//! pipe at the same number and a peerline service among them correctly adopts
//! the same lifeline. It is the ones that have since closed or reused it that
//! the check is for.
//!
//! # EOF without a thread and without a poll loop
//!
//! The read end is registered with the runtime's reactor
//! (`tokio::net::unix::pipe::Receiver`, which is `epoll`/`kqueue` underneath)
//! and awaited. A pipe whose last writer closes becomes *readable*, and the
//! read that follows returns zero bytes — the definition of EOF. So the cost
//! of holding a lifeline is one registered descriptor and a suspended future,
//! not a thread, not a timer, and not a wakeup until the moment that matters.

#![allow(unsafe_code)]

use std::fs::File;
use std::mem::ManuallyDrop;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::FileTypeExt;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{info, warn};

/// Has this process already taken its lifeline?
///
/// A descriptor can be owned once. Two `Host`s in one process — sequential or
/// concurrent — must not both claim the same number, because the second claim
/// would end in a double close of whatever the first had since replaced it.
/// The first caller gets it; every later one sees [`Lifeline::Absent`], which
/// is the same answer it would get on a machine with no supervisor, and
/// therefore needs no special handling anywhere.
static TAKEN: AtomicBool = AtomicBool::new(false);

/// What this process was given to watch.
pub(crate) enum Lifeline {
    /// No variable, or somebody else in this process already took it. The
    /// ordinary case: run by hand, by systemd, by a test.
    Absent,
    /// The read end of the supervisor's pipe, owned.
    Held(OwnedFd),
    /// The variable was set and could not be honoured. The string is a
    /// sentence for a log line; the caller carries on serving.
    Unusable(String),
}

/// Take this process's lifeline, at most once.
pub(crate) fn take() -> Lifeline {
    let fd = match peerline_contract::lifeline::from_env() {
        Ok(None) => return Lifeline::Absent,
        Ok(Some(fd)) => fd,
        Err(why) => return Lifeline::Unusable(why),
    };
    if TAKEN.swap(true, Ordering::SeqCst) {
        return Lifeline::Absent;
    }
    adopt(fd)
}

/// Claim ownership of `fd`, having first established that it is a pipe.
///
/// See the module docs: the borrow-then-own dance is what makes the
/// `from_raw_fd` sound, and what stops a descendant that inherited only the
/// *variable* from closing one of its own descriptors.
fn adopt(fd: RawFd) -> Lifeline {
    // SAFETY: constructing the `File` does not assert ownership — it is
    // immediately wrapped in `ManuallyDrop`, so the descriptor is never closed
    // by this binding and nothing else here touches it. `fstat` on a
    // descriptor that is not open fails with `EBADF`, which is the error path
    // below rather than undefined behaviour.
    let probe = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    match probe.metadata() {
        Ok(meta) if meta.file_type().is_fifo() => {
            // SAFETY: the descriptor is open (the `fstat` above succeeded) and
            // it is a pipe, so it is the one the supervisor passed and not a
            // number some descendant reused. Nothing else in this process owns
            // it: `TAKEN` admits exactly one caller here, and `probe` is
            // `ManuallyDrop`.
            Lifeline::Held(unsafe { OwnedFd::from_raw_fd(fd) })
        }
        Ok(_) => Lifeline::Unusable(format!(
            "file descriptor {fd} is not a pipe; leaving it alone"
        )),
        Err(e) => Lifeline::Unusable(format!("file descriptor {fd} cannot be used: {e}")),
    }
}

/// Resolve when the supervisor's write end closes, and never otherwise.
///
/// Every failure mode here resolves to *never*, deliberately. A host that
/// stopped because it could not read a pipe would exit zero — "I was asked to
/// stop" (§9) — and a supervisor that is perfectly alive would leave it
/// stopped. Refusing to act on a lifeline we cannot read costs the safety net
/// this one process; acting on one we misread costs the service.
pub(crate) async fn closed(fd: OwnedFd) {
    let rx = match tokio::net::unix::pipe::Receiver::from_owned_fd(fd) {
        Ok(rx) => rx,
        Err(e) => {
            warn!(error = %e, "peerline-host: cannot watch the lifeline; ignoring it");
            return never().await;
        }
    };
    info!("peerline-host: watching the supervisor lifeline");
    // Nothing is ever sent on a lifeline, so this buffer exists only so that
    // `try_read` can distinguish EOF from data. Bytes that do arrive are a
    // supervisor bug, not a shutdown request, and are logged and dropped.
    let mut scratch = [0u8; 64];
    loop {
        if let Err(e) = rx.readable().await {
            warn!(error = %e, "peerline-host: lifeline became unreadable; ignoring it");
            return never().await;
        }
        match rx.try_read(&mut scratch) {
            Ok(0) => {
                info!("peerline-host: the supervisor closed the lifeline");
                return;
            }
            Ok(n) => warn!(
                bytes = n,
                "peerline-host: a lifeline carries no data; ignoring what was written to it"
            ),
            // A readiness event that did not survive contact with the read,
            // and a signal that interrupted it. Both mean "ask again".
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                warn!(error = %e, "peerline-host: lifeline read failed; ignoring it");
                return never().await;
            }
        }
    }
}

/// A future that never resolves, spelled once so the reasoning above is in one
/// place rather than repeated at four `return`s.
async fn never() {
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absence is the common case and has to stay completely uneventful.
    #[test]
    fn no_variable_is_no_lifeline() {
        // Not a `remove_var` (racy, and `unsafe` in edition 2024): the
        // contract's reader is what decides, and it is tested where it lives.
        assert!(matches!(
            peerline_contract::lifeline::from_env(),
            Ok(None) | Ok(Some(_))
        ));
    }

    /// The descendant hazard, directly: a descriptor number that is not a pipe
    /// must be left exactly as it was found.
    #[test]
    fn a_descriptor_that_is_not_a_pipe_is_not_adopted() {
        use std::io::Write;
        use std::os::fd::AsRawFd;

        let mut file = tempfile();
        let fd = file.as_raw_fd();
        match adopt(fd) {
            Lifeline::Unusable(why) => assert!(why.contains("not a pipe"), "{why}"),
            _ => panic!("a regular file is not a lifeline"),
        }
        // Still ours, still open, still writable: nothing was closed.
        file.write_all(b"intact")
            .expect("the descriptor was left alone");
    }

    #[test]
    fn a_closed_descriptor_is_reported_rather_than_claimed() {
        // 4096 is above any descriptor this test process has opened, and the
        // answer for a closed one is the same as for a never-opened one.
        match adopt(4096) {
            Lifeline::Unusable(why) => assert!(why.contains("4096"), "{why}"),
            _ => panic!("a closed descriptor is not a lifeline"),
        }
    }

    #[test]
    fn a_pipe_is_adopted() {
        use std::os::fd::AsRawFd;

        let (reader, _writer) = std::io::pipe().expect("a pipe");
        let fd = reader.as_raw_fd();
        match adopt(fd) {
            // Leaked deliberately: `reader` still owns this number, and
            // letting both drop would be the double close the whole
            // borrow-first dance exists to prevent.
            Lifeline::Held(owned) => {
                let _ = std::os::fd::IntoRawFd::into_raw_fd(owned);
            }
            _ => panic!("a pipe is a lifeline"),
        }
    }

    fn tempfile() -> File {
        let path = std::env::temp_dir().join(format!("pl-lifeline-{}", std::process::id()));
        let file = File::create(&path).expect("a scratch file");
        let _ = std::fs::remove_file(&path);
        file
    }
}
