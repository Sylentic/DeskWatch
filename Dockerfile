# DeskWatch bridge image. Guide: docs/docker.md
#
#   docker build -t deskwatch-bridge .
#   docker run --rm deskwatch-bridge --demo
#
# Two stages: a Rust toolchain builds the release binary, and only that binary
# lands in a small Debian runtime image that runs as a non-root user.

# ---- build stage -----------------------------------------------------------
# Rust 1.88 is the minimum the workspace supports (rust-version in Cargo.toml).
#
# This stage always runs on the machine that does the build (BUILDPLATFORM) and
# cross-compiles for the image's CPU (TARGETARCH), so a multi-arch `buildx` build
# never compiles Rust under emulation, which is many times slower. When the
# build machine and the target are the same CPU (a plain `docker build`, also
# on a Raspberry Pi) nothing is cross-compiled and no extra toolchain is added.
FROM --platform=$BUILDPLATFORM rust:1.88-bookworm AS build
ARG BUILDARCH
ARG TARGETARCH
WORKDIR /src

# Pick the Rust target and, only when cross-compiling, the matching C compiler
# (a few dependencies, such as the TLS library, contain C code). The two files
# written here are read by the build step below.
RUN case "$TARGETARCH" in \
      amd64) echo x86_64-unknown-linux-gnu > /rust-target; gnu=x86-64-linux-gnu; cc=x86_64-linux-gnu-gcc ;; \
      arm64) echo aarch64-unknown-linux-gnu > /rust-target; gnu=aarch64-linux-gnu; cc=aarch64-linux-gnu-gcc ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && rustup target add "$(cat /rust-target)" \
    && if [ "$BUILDARCH" != "$TARGETARCH" ]; then \
         apt-get update \
         && apt-get install -y --no-install-recommends "gcc-$gnu" libc6-dev-"${TARGETARCH}"-cross \
         && rm -rf /var/lib/apt/lists/* \
         && echo "$cc" > /rust-linker; \
       else echo cc > /rust-linker; fi

# The workspace also lists the simulator and the UI crate, so their manifests
# must exist; only the bridge (and what it depends on) is compiled.
COPY Cargo.toml Cargo.lock ./
COPY bridge ./bridge
COPY sim ./sim
COPY ui ./ui
RUN target="$(cat /rust-target)"; linker="$(cat /rust-linker)"; \
    upper="$(echo "$target" | tr 'a-z-' 'A-Z_')"; \
    export "CARGO_TARGET_${upper}_LINKER=$linker" \
           "CC_$(echo "$target" | tr '-' '_')=$linker" \
           CARGO_PROFILE_RELEASE_STRIP=symbols; \
    cargo build --release --locked -p deskwatch-bridge --target "$target" \
    && cp "target/$target/release/deskwatch-bridge" /deskwatch-bridge

# The runtime image's user entry (uid 10001, no shell, no home).
RUN mkdir /out \
    && { cat /etc/passwd; echo 'deskwatch:x:10001:10001::/nonexistent:/usr/sbin/nologin'; } > /out/passwd \
    && { cat /etc/group; echo 'deskwatch:x:10001:'; } > /out/group

# ---- runtime stage ---------------------------------------------------------
FROM debian:bookworm-slim

# Needs no package manager and no RUN step (a RUN here would need CPU emulation
# for the arm64 image): the CA certificates (for HTTPS to GitHub, Azure DevOps
# and TLS brokers) and the passwd/group files with the non-root user are
# prepared in the build stage. Both are plain Debian bookworm files plus one line.
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /out/passwd /etc/passwd
COPY --from=build /out/group /etc/group

COPY --from=build /deskwatch-bridge /usr/local/bin/deskwatch-bridge
COPY bridge/config.example.toml /usr/share/deskwatch/config.example.toml

# Config at /etc/deskwatch/bridge.toml (the bridge's default path) and one file
# per secret in /run/credentials, which is what a plain `token_file = "name"`
# in the config resolves against. Mount both read-only; see docs/docker.md.
ENV CREDENTIALS_DIRECTORY=/run/credentials \
    RUST_LOG=info

USER deskwatch
# The HTTP listener (`[http] listen`): the dashboard page and the Gitea webhook.
# It only opens when `[kiosk] enabled` is set or a Gitea source exists.
EXPOSE 8787

# The bridge checks itself (the image has no curl): it asks its own listener
# for `/` over loopback. With no dashboard and no Gitea source there is no
# listener and the check passes, since startup problems already end the
# process. See docs/docker.md.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD ["deskwatch-bridge", "--healthcheck"]
ENTRYPOINT ["deskwatch-bridge"]
CMD ["/etc/deskwatch/bridge.toml"]
