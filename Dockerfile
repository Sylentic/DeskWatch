# DeskWatch bridge image. Guide: docs/docker.md
#
#   docker build -t deskwatch-bridge .
#   docker run --rm deskwatch-bridge --demo
#
# Two stages: a Rust toolchain builds the release binary, and only that binary
# lands in a small Debian runtime image that runs as a non-root user.

# ---- build stage -----------------------------------------------------------
# Rust 1.88 is the minimum the workspace supports (rust-version in Cargo.toml).
FROM rust:1.88-bookworm AS build
WORKDIR /src

# The workspace also lists the simulator and the UI crate, so their manifests
# must exist; only the bridge (and what it depends on) is compiled.
COPY Cargo.toml Cargo.lock ./
COPY bridge ./bridge
COPY sim ./sim
COPY ui ./ui
RUN cargo build --release --locked -p deskwatch-bridge \
    && strip target/release/deskwatch-bridge

# ---- runtime stage ---------------------------------------------------------
FROM debian:bookworm-slim

# Needs no package manager: the CA certificates (for HTTPS to GitHub, Azure
# DevOps and TLS brokers) come from the build image, and the non-root user is a
# plain /etc/passwd entry.
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN echo 'deskwatch:x:10001:10001::/nonexistent:/usr/sbin/nologin' >> /etc/passwd \
    && echo 'deskwatch:x:10001:' >> /etc/group

COPY --from=build /src/target/release/deskwatch-bridge /usr/local/bin/deskwatch-bridge
COPY bridge/config.example.toml /usr/share/deskwatch/config.example.toml

# Config at /etc/deskwatch/bridge.toml (the bridge's default path) and one file
# per secret in /run/credentials, which is what a plain `token_file = "name"`
# in the config resolves against. Mount both read-only; see docs/docker.md.
ENV CREDENTIALS_DIRECTORY=/run/credentials \
    RUST_LOG=info

USER deskwatch
# The webhook listener (`[http] listen`, only open when a Gitea source exists).
EXPOSE 8787

# No HEALTHCHECK on purpose: the bridge has no status endpoint, and it exits
# (so the restart policy handles it) when it cannot start. See docs/docker.md.
ENTRYPOINT ["deskwatch-bridge"]
CMD ["/etc/deskwatch/bridge.toml"]
