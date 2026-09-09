# peerline-host workspace tasks. List with `just`.
#
# The protocol crates this builds on (`peerline`, the transports) live in the
# sibling `peerline` repository; `Cargo.toml` path-depends on it, so a fresh
# clone needs it checked out alongside this one.

default:
    @just --list

# --- workspace ---

build:
    cargo build --release --workspace

test:
    cargo test --workspace

clippy:
    cargo clippy --workspace --all-targets

fmt:
    cargo fmt --all

# --- installation ---
#
# There is deliberately nothing here, and there never will be: nothing in this
# repository is a thing a platform installs. Both crates here are libraries a
# service links.
#
# The recipes this file used to carry installed `peerline-manager` plus a
# launchd plist and a systemd unit from `crates/peerline-manager/dist/`. That
# crate is gone, and so is the shape it assumed: under §10.5 of peerline-cluster's DESIGN.md
# there is **no per-service unit at all** — not for the coordinator either,
# which is a child of the supervisor like everything else. The one thing a
# platform installs is `peerline-cluster`, which lives in the sibling
# `peerline-cluster` repository, and so does whatever `install` recipe it
# eventually grows.

clean:
    cargo clean
