# Unified Dockerfile for fluvio-run
# Modes:
#   prebuilt (default): expects binary at ./fluvio-run (justfile k3d-push)
#   zigbuild: cross-compiles inside container (CI / emergency-push)
#
# Usage:
#   docker build -f k8-util/docker/Dockerfile .                    # prebuilt
#   docker buildx build --platform linux/arm64 \
#     --build-arg BUILD_MODE=zigbuild \
#     -f k8-util/docker/Dockerfile .                               # cross-compile

ARG BUILD_MODE=prebuilt

# --- Stage: zigbuild (runs natively on build host, cross-compiles via zig) ---
FROM --platform=$BUILDPLATFORM rust:1-trixie AS zigbuild
ARG TARGETARCH
RUN set -eux; \
    curl -fsSL https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz | tar -xJ -C /opt; \
    ln -s /opt/zig-x86_64-linux-0.15.2/zig /usr/local/bin/zig; \
    cargo install cargo-zigbuild --locked; \
    case "${TARGETARCH}" in \
      arm64|aarch64) rustup target add aarch64-unknown-linux-musl ;; \
      amd64|x86_64)  rustup target add x86_64-unknown-linux-musl ;; \
    esac
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
COPY connector/ connector/
COPY smartmodule/ smartmodule/
RUN set -eux; \
    case "${TARGETARCH}" in \
      arm64|aarch64) TARGET=aarch64-unknown-linux-musl ;; \
      amd64|x86_64)  TARGET=x86_64-unknown-linux-musl ;; \
    esac; \
    cargo zigbuild --target "${TARGET}" --release --bin fluvio-run -p fluvio-run; \
    cp "/app/target/${TARGET}/release/fluvio-run" /app/fluvio-run

# --- Stage: select binary based on BUILD_MODE ---
FROM scratch AS binary-prebuilt
COPY fluvio-run /fluvio-run

FROM scratch AS binary-zigbuild
COPY --from=zigbuild /app/fluvio-run /fluvio-run

FROM binary-${BUILD_MODE} AS binary

# --- Runtime ---
FROM alpine:3.21
COPY --from=binary /fluvio-run /fluvio-run
RUN chmod +x /fluvio-run
ENTRYPOINT ["/fluvio-run"]
