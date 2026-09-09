//! Minting this process's [`HostId`].
//!
//! # The obligation
//!
//! `HostId` equality is a claim about the **socket namespace**, not about a
//! machine and not about a name: two reports carrying equal ids assert that a
//! `uds` path in one, dialled from the other, reaches the process that
//! published it. Everything below exists to make that claim true, because
//! [`Bindings::connect`] prefers uds whenever it holds — and a wrong "same
//! host" is not a failed dial but a *successful* one, to an unrelated socket
//! that happens to sit at the same path.
//!
//! [`Bindings::connect`]: crate::Bindings::connect
//!
//! # Why not `gethostname`
//!
//! Two containers on one machine share a hostname and do **not** share a
//! filesystem, so hostnames compare equal in precisely the case that must
//! compare unequal. A machine id has the same flaw. What is wanted is an
//! identity of the *filesystem namespace*, and the cheapest honest way to name
//! a namespace is to write a token into it and read it back.
//!
//! # What is minted
//!
//! A random token stored at `{temp_dir}/peerline-host-id`, created once by
//! whichever process gets there first and read by every process after it.
//!
//! - Two processes sharing that file share a `/tmp`, so they share the mount
//!   namespace their uds paths are resolved in. The claim holds.
//! - Two containers each have their own `/tmp` and so their own token, even on
//!   one kernel and under one hostname. They compare unequal, which is the
//!   conservative answer: their consumers fall back to ws or iroh rather than
//!   mis-dialling.
//! - The token survives a reboot, which is *correct*: the claim is about a
//!   namespace, not about an epoch. Nothing is invalidated by living longer.
//!
//! Two escape hatches, in priority order:
//!
//! - `PEERLINE_HOST_ID` overrides everything, for a deployment that knows
//!   better — containers deliberately sharing a socket directory, say.
//! - If the file can be neither read nor created (a read-only or unshared
//!   `/tmp`), a per-process random token is used. That can only ever *under*
//!   claim locality: uds is skipped and the consumer dials ws or iroh instead.
//!   A host whose producer is uds-only will fail to connect and say so, which
//!   is the failure that can be diagnosed rather than the one that cannot.

use std::io::Read;
use std::path::PathBuf;
use std::sync::OnceLock;

use peerline_contract::HostId;

/// Overrides the minted value entirely.
const HOST_ID_ENV: &str = "PEERLINE_HOST_ID";

/// The file whose *location* is the identity being named.
const HOST_ID_FILE: &str = "peerline-host-id";

/// This process's host identity, minted once and cached.
///
/// See the module docs for what equality asserts and why the value is not a
/// hostname.
pub fn host_id() -> &'static HostId {
    static ID: OnceLock<HostId> = OnceLock::new();
    ID.get_or_init(mint)
}

fn mint() -> HostId {
    mint_at(&std::env::temp_dir().join(HOST_ID_FILE))
}

/// [`mint`], against a nominated file — the seam the tests use, so that
/// exercising the fallback does not mean creating the real shared token in
/// this machine's temp directory as a side effect.
fn mint_at(path: &PathBuf) -> HostId {
    if let Some(id) = std::env::var_os(HOST_ID_ENV) {
        let id = id.to_string_lossy().trim().to_string();
        if !id.is_empty() {
            return HostId::new(id);
        }
    }
    match shared_token(path) {
        Ok(token) => HostId::new(token),
        Err(e) => {
            // Not fatal — an under-claimed locality costs a uds preference,
            // never a mis-dial — but not a detail either, and `debug!` is
            // below what a service logs in production, so the one place this
            // ever showed up was nowhere. It disables uds for **every**
            // dependency of this process for its whole life: a consumer of a
            // uds-only producer on this very machine will fail to connect,
            // and nothing later in the failure says why. Warn, and say what
            // it costs.
            tracing::warn!(path = %path.display(), error = %e,
                "peerline-host: cannot read or create the shared host-id file, so this \
                 process is using a per-process identity: uds coordinates published by \
                 other processes here will be treated as belonging to a different host \
                 and skipped, and a dependency that is reachable only over uds will \
                 fail to connect. Set PEERLINE_HOST_ID to say otherwise.");
            HostId::new(format!("pl-proc-{}-{}", std::process::id(), random_token()))
        }
    }
}

/// Read the namespace's token, creating it if this is the first process here.
fn shared_token(path: &PathBuf) -> Result<String, String> {
    if let Some(token) = read_token(path) {
        return Ok(token);
    }
    let token = format!("pl-{}", random_token());
    // `create_new` is the whole race resolution: exactly one process writes,
    // everyone else loses and reads what the winner wrote.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(token.as_bytes())
                .map_err(|e| format!("write {}: {e}", path.display()))?;
            Ok(token)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_token(path).ok_or_else(|| format!("{} is empty", path.display()))
        }
        Err(e) => Err(format!("create {}: {e}", path.display())),
    }
}

fn read_token(path: &PathBuf) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let token = text.trim();
    // A truncated or scribbled-on file must not become a *shared* identity: a
    // short or empty token would compare equal for every process that read the
    // same damage. Anything unconvincing is treated as absent.
    (token.len() >= 8 && token.len() <= 128).then(|| token.to_string())
}

/// 128 bits of randomness as hex, without a dependency.
///
/// `/dev/urandom` where it exists; otherwise the standard library's own
/// per-process random seed (`RandomState`, seeded by the OS) mixed with the
/// clock and the pid. Both are ample: the value only has to not collide with
/// another machine's.
fn random_token() -> String {
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok()
    {
        return hex(&bytes);
    }
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    let a = h.finish();
    h.write_u64(a);
    let b = h.finish();
    bytes[..8].copy_from_slice(&a.to_le_bytes());
    bytes[8..].copy_from_slice(&b.to_le_bytes());
    hex(&bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_file_yields_the_same_identity_and_a_new_one_does_not() {
        let dir = std::env::temp_dir().join(format!("pl-hostid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let a = dir.join("a");
        let first = shared_token(&a).expect("mint");
        let second = shared_token(&a).expect("read back");
        assert_eq!(first, second, "one namespace, one identity");

        let b = dir.join("b");
        assert_ne!(
            shared_token(&b).expect("mint"),
            first,
            "a different namespace must not compare equal"
        );

        // Damage is treated as absent rather than shared: two processes must
        // not both adopt the same short scribble as an identity. Nothing
        // overwrites it either — that would race whoever else is reading it —
        // so the caller falls back to a per-process identity, which can only
        // under-claim locality.
        std::fs::write(&a, "  ").expect("truncate");
        assert!(
            shared_token(&a).is_err(),
            "a damaged token must not become a shared identity"
        );
        // And minting *against that very file* falls back to a per-process
        // identity rather than adopting the damage. This used to call
        // `mint()`, which reads the real `{temp_dir}/peerline-host-id` and so
        // compared a value neither branch of the assertion could ever produce
        // — a test that could not fail, whose only observable effect was
        // creating this machine's shared host-id file as a side effect.
        let fallback = mint_at(&a);
        assert_ne!(
            fallback,
            HostId::new(&first),
            "and it is not adopted anyway"
        );
        assert!(
            fallback.as_str().starts_with("pl-proc-"),
            "the fallback must be the per-process identity, not the damaged file: {fallback}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tokens_are_random_and_long() {
        let (a, b) = (random_token(), random_token());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
