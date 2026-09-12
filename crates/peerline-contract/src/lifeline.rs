//! The **lifeline**: the inherited pipe that makes a supervised child unable
//! to outlive its supervisor.
//!
//! # What it is
//!
//! A supervisor creates a pipe before each `fork`, passes the **read** end to
//! the child as an inherited file descriptor, and keeps the **write** end for
//! itself. Nobody ever writes a byte. The only event on that pipe is the one
//! the kernel produces for free: when the last copy of the write end is
//! closed, the child's read end reports **EOF**. A child that treats EOF as
//! "stop" therefore stops when its supervisor goes away — cleanly, by
//! `SIGKILL`, by a panic, by the process table filling up so that nothing can
//! run its cleanup. There is no code path a dying supervisor can skip, because
//! closing a file descriptor is not a code path: it is what the kernel does to
//! a process's descriptor table whatever killed it.
//!
//! This is the portable equivalent of Linux's `PR_SET_PDEATHSIG`, and unlike
//! it, it works on macOS and survives an intervening `exec`.
//!
//! # Why it is here
//!
//! Same reason as [`crate::coord`] and [`crate::manifest`]: the supervisor
//! writes the variable and the host reads it, both ends are outside this
//! crate, and neither may depend on the other. `peerline-cluster` renders the
//! envelope and `peerline-host` parses it; the dependency arrow runs from the
//! cluster layer to the host and never back, so the one thing they must agree
//! about lives below them both.
//!
//! # The envelope
//!
//! [`LIFELINE_FD_ENV`] holds a **decimal file descriptor number** and nothing
//! else — [`render`] writes it, [`parse`] reads it. The value is deliberately
//! not a path, a pid or a token: the whole mechanism is an open descriptor,
//! and a descriptor's only name is its number.
//!
//! The number must be **above the standard streams**. A supervisor that
//! nominated 0, 1 or 2 would be asking a child to watch its own stdin or
//! stdout for EOF, which is a different question with a different answer, and
//! the failure would be a service that exits when its log pipe is drained.
//! [`parse`] rejects it rather than letting it be a surprise at runtime.
//!
//! # Absence is normal, and must stay normal
//!
//! **An unset variable means "no lifeline", and that is an ordinary,
//! supported configuration** — a service started by hand, by systemd, by
//! launchd, or by a test harness that runs it directly. Nothing about a host's
//! behaviour may change because it has no supervisor watching it. An *empty*
//! variable is unset too ([`crate::env::var`]), because an exported-but-blank
//! variable is a shell artefact and never a request to watch descriptor zero.
//!
//! # Implementing this in a language that is not Rust
//!
//! A child that links `peerline-host` gets this for free: `Host::run` and
//! `Host::run_until` watch the descriptor and take the `SIGTERM` path on EOF.
//! **A child that does not link it sees nothing** — not an error, not a
//! signal, just a supervisor that is gone and a process that keeps running.
//! That gap is closed by implementing the seven rules below, and this section
//! is the canonical statement of them so that an implementor needs no Rust.
//!
//! Everything here is POSIX. There is no peerline protocol involved, nothing
//! is parsed, and no library is required: it is one environment variable and
//! one `read(2)`.
//!
//! ## 1. The variable
//!
//! `PEERLINE_LIFELINE_FD` ([`LIFELINE_FD_ENV`]) holds a **decimal file
//! descriptor number in ASCII**, and nothing else — no prefix, no whitespace,
//! no path. `peerline-cluster`'s supervisor passes `3` (the first number above
//! the standard streams) and the descriptor is `dup2`'d onto it with
//! close-on-exec cleared, so it survives the `exec` into your program.
//!
//! **Read the number; do not hard-code 3.** The number is what the variable is
//! for, and a supervisor that is not this one may choose differently.
//!
//! ## 2. Unset or empty means there is no lifeline
//!
//! And that is an ordinary, supported way to run: by hand, from systemd, from
//! launchd, from a test. A child that refuses to start without a lifeline has
//! turned a safety net into a startup dependency. Do nothing and run normally.
//!
//! ## 3. Read it; never write to it and never close it early
//!
//! Nobody ever sends a byte. The descriptor is a pipe held open by the
//! supervisor at the other end, and the only event it will ever produce is the
//! one the kernel produces for free.
//!
//! ## 4. `read(2)` returning **0** is the event
//!
//! Zero bytes on a blocking read is end-of-file, which the kernel delivers
//! when the **last** write end of the pipe closes. Since only the supervisor
//! holds a write end, EOF means the supervisor is gone — cleanly, by
//! `SIGKILL`, by a panic, by the machine running out of memory. React by
//! shutting down as you would for `SIGTERM`, and exit.
//!
//! There is no timeout to configure and no polling to do: the read blocks,
//! costing nothing, until the moment that matters.
//!
//! ## 5. A short read that is not zero is a supervisor bug, not a shutdown
//!
//! `read` returning `n > 0` means somebody wrote to a channel that carries no
//! data. Log it and read again. Exiting on it would let a stray byte stop a
//! healthy service.
//!
//! ## 6. `EINTR` and `EAGAIN` mean "ask again"; any other error means "stop
//! watching"
//!
//! A signal interrupting the read, or a spurious readiness on a non-blocking
//! descriptor, are both retries. **Any other error must not exit the
//! process.** A child that shut down because it could not read a pipe would
//! stop while its supervisor was perfectly alive — refusing to act on a
//! lifeline you cannot read costs this one process its safety net, whereas
//! acting on one you misread costs the service. Log, give up on watching, and
//! go on serving.
//!
//! ## 7. If you spawn children of your own, check before you adopt
//!
//! The variable is inherited by every descendant, and so is the descriptor
//! (close-on-exec is deliberately cleared) — which is why a peerline service
//! spawned by a peerline service correctly watches the same lifeline. But a
//! grandchild that closed or reused that number would be watching *its own*
//! unrelated socket. So `fstat(fd)` first and proceed only if `S_ISFIFO` is
//! true; if it is not, treat it as no lifeline and leave the descriptor
//! completely alone. Do not close it.
//!
//! ## A shell child
//!
//! ```sh
//! #!/bin/sh
//! # A wrapper script that must not outlive the supervisor that spawned it.
//! your_program &
//! main=$!
//!
//! # Unset means no supervisor is watching, which is a normal way to run.
//! if [ -n "${PEERLINE_LIFELINE_FD:-}" ]; then
//!     (
//!         # Reading a pipe nobody writes to blocks until the last write end
//!         # closes. `cat` returning *is* the supervisor going away.
//!         eval "exec <&${PEERLINE_LIFELINE_FD}"
//!         cat >/dev/null 2>&1
//!         kill -TERM "$main" 2>/dev/null
//!     ) &
//! fi
//!
//! wait "$main"
//! ```
//!
//! ## Any language with the raw syscalls
//!
//! Run this on a thread or an event loop of its own — the read blocks, and it
//! blocks for the entire healthy life of the process.
//!
//! ```text
//! raw = getenv("PEERLINE_LIFELINE_FD")
//! if raw is unset or empty:
//!     return                      # no lifeline; this is normal (rule 2)
//!
//! fd = parse_decimal(raw)
//! if fd is not a number or fd < 3:
//!     log("unusable lifeline"); return          # never fatal
//! if not is_fifo(fstat(fd)):
//!     log("not our pipe"); return               # rule 7 — and do not close it
//!
//! buf = byte[64]
//! loop:
//!     n = read(fd, buf)
//!     if n == 0:                  # EOF: the supervisor is gone (rule 4)
//!         shutdown_gracefully()
//!         exit(0)                 # a clean stop, not a failure
//!     if n > 0:
//!         log("a lifeline carries no data"); continue      # rule 5
//!     if errno == EINTR or errno == EAGAIN:
//!         continue                                          # rule 6
//!     log("lifeline unreadable; ignoring it"); return       # rule 6
//! ```
//!
//! Exit **zero**: EOF means you were asked to stop, which is the same clean
//! stop a `SIGTERM` would have been, and not the "restart me" a non-zero
//! status tells a supervisor.
//!
//! # What this does not replace
//!
//! The lifeline is **prevention**: the child cannot outlive the supervisor. It
//! is not **recovery**: it says nothing about a child that ignores its
//! lifeline, or that is wedged in a state where it cannot act on one. A
//! supervisor that also records what it started — `peerline-cluster` writes
//! pgids to a state file — keeps that record for exactly those cases. The two
//! mechanisms answer different questions and neither subsumes the other; see
//! §10.5 of `peerline-cluster/DESIGN.md`.

/// The environment variable naming the descriptor a child should watch for
/// EOF.
///
/// Set by a supervisor on every child it spawns; read by
/// `peerline_host::Host::run`. Unset means no lifeline, which is normal.
pub const LIFELINE_FD_ENV: &str = "PEERLINE_LIFELINE_FD";

/// The lowest descriptor a lifeline may use: anything at or below this is a
/// standard stream and means something else entirely.
const FIRST_NON_STDIO_FD: i32 = 3;

/// What a supervisor puts in [`LIFELINE_FD_ENV`] for a child that will see the
/// read end of its lifeline as descriptor `fd`.
///
/// `fd` is a `RawFd` in all but name — spelled `i32` so that this module
/// compiles and documents itself on every platform, while meaning nothing on
/// those without file descriptors.
#[must_use]
pub fn render(fd: i32) -> String {
    fd.to_string()
}

/// The inverse of [`render`], with the two rules the value has to obey.
///
/// # Errors
///
/// Returns a sentence naming what was wrong, for a log line: the value was not
/// a decimal number, or it named a standard stream (see the module docs). A
/// host treats either as *no lifeline* and carries on serving — refusing to
/// start because a supervisor mis-spelled a descriptor number would turn a
/// safety net into an outage.
pub fn parse(value: &str) -> Result<i32, String> {
    let fd: i32 = value
        .parse()
        .map_err(|_| format!("`{value}` is not a file descriptor number"))?;
    if fd < FIRST_NON_STDIO_FD {
        return Err(format!(
            "file descriptor {fd} is a standard stream; a lifeline must be {FIRST_NON_STDIO_FD} \
             or above"
        ));
    }
    Ok(fd)
}

/// The descriptor this process was told to watch, if it was told anything.
///
/// `Ok(None)` is the ordinary "nobody is supervising me" answer and the
/// overwhelmingly common one. `Err` means the variable was set to something
/// unusable, which a caller should log and then treat as `None`.
///
/// # Errors
///
/// See [`parse`]; additionally, a value that is not UTF-8 cannot be a decimal
/// number.
pub fn from_env() -> Result<Option<i32>, String> {
    let Some(raw) = crate::env::var(LIFELINE_FD_ENV) else {
        return Ok(None);
    };
    let text = raw
        .to_str()
        .ok_or_else(|| format!("{LIFELINE_FD_ENV} is not valid UTF-8"))?;
    parse(text).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_round_trips() {
        for fd in [3, 4, 17, 1024] {
            assert_eq!(parse(&render(fd)), Ok(fd));
        }
    }

    /// The mistake that would be silent: a supervisor handing over a standard
    /// stream, and a service that then exits when its own log pipe is drained
    /// rather than when its supervisor dies.
    #[test]
    fn a_standard_stream_is_not_a_lifeline() {
        for fd in [0, 1, 2, -1] {
            let err = parse(&render(fd)).expect_err("stdio is refused");
            assert!(err.contains("standard stream"), "{err}");
        }
    }

    #[test]
    fn nonsense_is_reported_rather_than_guessed_at() {
        assert!(parse("").is_err());
        assert!(parse("three").is_err());
        assert!(
            parse(" 3").is_err(),
            "no whitespace tolerance: it is a number"
        );
    }
}
