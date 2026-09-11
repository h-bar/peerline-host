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
