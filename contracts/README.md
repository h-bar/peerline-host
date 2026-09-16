# The `coord/1` contracts directory

Two manifests describing the control protocol a service speaks to a cluster
coordinator, in the language-neutral contract-manifest format that
`peerline-contract-spec` defines and validates.

- **`coord.v1.json`** — the `coord.*` ops: register, await_ready, ready,
  heartbeat, update, resolve, list, bindings, watch. Nine ops, nineteen wire
  types, four error codes.
- **`peerline.contract.v1.json`** — a *vocabulary*: the shapes a service
  declares about itself and the coordinates it is handed back. No ops, and
  nothing produces it. It is separate because the same shapes travel on paths
  that `coord/1` is only one of — a binary writes `Produced` and `Consumed`
  into the probe file a supervisor reads with no coordinator involved, and a
  dashboard renders `Endpoints` and `Binding` without calling an op at all.

If you are implementing a joiner in a language that is not Rust, read
`coord.v1.json` first and then this file. The manifest is exhaustive about
shapes and says so; it is deliberately silent about several things you also
need, and those are below.

## These manifests document; they do not generate

Everywhere else in this stack the rule is that the manifest is the source and
the Rust is generated, so the two cannot drift. `coord/1` is a considered
exception, and the reasoning is recorded in `peerline-cluster`'s `PROPOSALS.md`
item 1.

The short version: `peerline-contract`'s types are not only wire shapes, they
are the public API of a library. `Host` takes a `Vec<Produced>`,
`Bindings::connect` returns a `Binding`, `Connection::dial` takes a `&HostId`
and an `&Endpoints`, a supervisor consumes a `Listing` and a `BindingGraph`.
Two crates each expanding their own copy of `coord/1` would get two distinct
Rust types and could not pass those values to one another. So the Rust in
`crates/peerline-contract/src/` stays hand-written and these files mirror it.

What makes that safe rather than merely cheap is
`crates/peerline-contract/tests/coord_manifest_drift.rs`. It serialises real
Rust values and checks field names, the three field states, default values,
union tags, arm spellings, op names, op kinds and error codes against what
these files declare. **Edit one side and that test fails naming the type and
the field.** Its module documentation states precisely what it does and does
not catch; read it before trusting it for more.

A non-Rust implementation is checked against these files and nothing else.

## What the format cannot say, and you still need

### The joining sequence

The manifest describes ops. Joining a cluster is a lifecycle, and the order is
part of the contract:

1. Call `coord.register` with a stable instance id, the process id, and every
   service this process mounts with its bound coordinates.
2. Open `coord.await_ready` and read it to completion. One `Binding` arrives
   per required consumed contract. The stream is seeded from current state, so
   a contract that is already resolvable yields its binding immediately.
3. Call `coord.ready` once the process can actually answer ops.
4. Call `coord.heartbeat` on a repeating interval well under the time-to-live
   the coordinator reports in `coord.list`, for the rest of the process's life.

On `{"status": "unknown_instance"}` from either `coord.ready` or
`coord.heartbeat`: re-register, then send `coord.ready` again. **Do not re-run
`coord.await_ready`.** It is a one-shot startup step, its bindings are already
in hand, and re-running it parks the process on dependencies it has already
resolved.

There is a fifth step, and it has no place in the sequence because it is not
part of starting up:

5. Whenever the set of transports you are actually answering on changes, call
   `coord.update` with the coordinates you are serving on **now** — all of
   them, not the ones that changed. A process that keeps running after one of
   its transports stops listening otherwise leaves an address in the registry
   that can only fail to dial.

A joiner that binds one transport, or that exits when any transport dies, never
sends it. `coord.update` raises `unknown_instance` rather than answering a
value, and the recovery is the one above — and note that the re-registration it
sends you to **carries your current coordinates**, so it is itself the update.
That is also why a joiner whose coordinator connection is down has nothing to
queue: reconnecting re-registers, and re-registering re-states where you are.

### The environment

A supervisor hands these to every child it spawns. None of them has a home in
the manifest format.

| Variable | Meaning |
| --- | --- |
| `PEERLINE_COORD_SOCK` | The Unix socket of the coordinator to register with. **Unset means no coordinator**, which is a valid configuration and not an error. |
| `PEERLINE_SERVICE_UDS` | The Unix socket this process should mount its service on. A convention: a process mounting several services distributes the one injected address itself. |
| `PEERLINE_MANIFEST_OUT` | If set, the process must write its probe manifest to this **path** and exit instead of serving. |
| `PEERLINE_LIFELINE_FD` | An inherited pipe descriptor. Hold it open and stop when it reports end-of-file: that is how a supervised child is made unable to outlive its supervisor. **Unset means no supervisor is watching.** |
| `PEERLINE_HOST_ID` | Overrides the derived host identity. See below. |

An empty value is treated as unset everywhere. An exported-but-blank variable
is a shell or supervisor artefact, never a request to dial `""`.

The conventional coordinator socket, when nothing says otherwise, is
`/tmp/peerline-coord.sock`. A process given neither a variable nor a
configured address simply does not join a cluster.

### The `HostId` derivation

**This is the one unwritten contract most likely to bite a non-Rust joiner, and
it fails silently.** It is stated in full in the prose of `Endpoints.host` in
`peerline.contract.v1.json`, and repeated here because this is the file
somebody reads first.

`Endpoints.host` is an opaque token naming the *filesystem namespace* in which
that set's `uds` paths mean anything. A consumer considers the `uds` list only
when its own token is exactly equal to the producer's. Get the derivation
wrong and one of two things happens:

- **Under-claim** — you mint a token nobody else shares. You silently never get
  uds to a Rust neighbour on the same machine, and a dependency that is
  reachable only over uds fails to connect with nothing in the failure saying
  why.
- **Over-claim** — you mint a token a process in a different namespace also
  has. You dial a path that resolves to an unrelated socket, and the dial
  *succeeds*. This is a mis-dial, not an error.

Derive it in this order:

1. If `PEERLINE_HOST_ID` is set to a non-empty value after trimming
   surrounding whitespace, that value is the token. Nothing else is consulted.
2. Otherwise read the file `peerline-host-id` in the system temporary
   directory — what `TMPDIR` names on Unix, `/tmp` when it does not. Its
   trimmed contents are the token if their length is at least 8 and at most
   128 characters. Anything shorter, longer, empty or whitespace-only is
   treated as **absent**, not adopted: every process that read the same damage
   would otherwise compare equal.
3. If there was nothing usable, create that file with a fresh random token
   spelled `pl-` followed by 32 lowercase hex characters (128 bits). Create it
   **exclusively** (`O_CREAT | O_EXCL`), so exactly one process wins and the
   losers re-read what the winner wrote. Losing the race is not an error: read
   the file and use its contents.
4. If the file can be neither read nor created — a read-only or unshared
   temporary directory — fall back to a token unique to this process and
   shared with nobody. Rust spells it `pl-proc-<pid>-<32 hex chars>`; the
   spelling does not matter, only that it cannot collide. **Warn when you take
   this branch**: it disables uds for every dependency of the process for its
   whole life, and nothing in the resulting failure says why.

Why a file rather than a hostname: two containers on one machine share a
hostname and do not share a filesystem, so hostnames compare equal in
precisely the case that must compare unequal. A machine id has the same flaw.
The cheapest honest way to name a namespace is to write a token into it and
read it back. Two processes sharing the file share a temporary directory, so
they share the mount namespace their uds paths resolve in. Two containers each
have their own, so they compare unequal — which is the conservative answer.
The token surviving a reboot is correct: the claim is about a namespace, not
about an epoch.

Do not invent a fifth source. Hashing a hostname, reading a machine id, or
minting per process without trying the file all get you one of the two
failures above.

The Rust implementation is `crates/peerline-host/src/hostid.rs`.

## Notes for a future migration

If the Host API is ever rewritten around generated types — option (b) in
`PROPOSALS.md` item 1 — these are the places where these files and today's
hand-written Rust are deliberately not identical. None of them is a wire
difference, and none is catchable by the drift test.

- **`Endpoints.host` is declared `string`.** In Rust it is `HostId`, a
  transparent newtype over `String`, which is the same JSON string. The format
  has no newtype form, and `Connection::dial` and `uds_usable_from` are keyed
  on the type — so a generated `String` would not compile at those call sites.
- **`Endpoints.uds` is declared as an array of `path`.** In Rust it is
  `Vec<String>`; a generated type would be `Vec<PathBuf>`. `path` is the
  truthful declaration — those strings are resolved on a particular machine,
  which is the whole point of `host` — and the two are the same JSON.
- **The millisecond fields are declared `u53`, and are `u64` in Rust.** The
  format's `u53` generates `u64` in Rust, so this is not a difference in the
  generated type at all; it is the declaration that the value stays inside
  ±(2^53 − 1), which every one of them does by many orders of magnitude, and
  which is what lets a JavaScript implementor use a plain number rather than a
  `bigint`. Nothing at runtime enforces the bound in either language.
- **`ContractId` is a `BTreeMap` and `BTreeSet` key** in the cluster layer's
  graph code, and the generator does not emit `Ord` or `Hash` on a struct.
- **The constants have no home in the format.** `PEERLINE_COORD_SOCK`,
  `PEERLINE_SERVICE_UDS`, `PEERLINE_MANIFEST_OUT`, `PEERLINE_LIFELINE_FD`,
  `PEERLINE_HOST_ID` and the default socket path are prose here and in the
  manifest, and a joiner needs every one.
