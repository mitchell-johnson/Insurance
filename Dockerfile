# syntax=docker/dockerfile:1.7

# One image holds every binary: ic-node (a ledger node), ic-api (the public
# API, as a gateway to the nodes or as the single-process insurance in a
# box), ic-worker (the end-of-day worker) and ic-admin (the operator's
# command). The deployment chooses the command.

# --- build ------------------------------------------------------------------
FROM rust:1.97-bookworm AS build
WORKDIR /src
# clang for the Starlark and crypto builds. The Temporal SDK needs protoc and
# the well-known .proto files, which Debian ships in libprotobuf-dev.
RUN apt-get update \
    && apt-get install -y --no-install-recommends clang protobuf-compiler libprotobuf-dev \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --workspace --locked \
    && mkdir -p /out/bin /out/state/data /out/state/cold /out/state/roots \
    && find target/release -maxdepth 1 -type f -executable -name 'ic-*' -exec cp {} /out/bin/ \;

# --- test -------------------------------------------------------------------
# `docker build --target test .` runs formatting, lints and the whole test
# suite in the image, including the end-of-day workflow on a real Temporal
# dev server, which the Temporal SDK's test support fetches. Pass
# `--build-arg IC_SKIP_TEMPORAL_TESTS=1` where that download is not possible;
# the test then says in its output that it did not run.
#
# The checks build in the development profile, as CI does. The release
# profile (thin LTO, one codegen unit) needs several gigabytes to link each
# test binary, more than a laptop's Docker machine has, and the suite does
# not depend on optimisation.
FROM build AS test
ARG IC_SKIP_TEMPORAL_TESTS=
ENV IC_SKIP_TEMPORAL_TESTS=${IC_SKIP_TEMPORAL_TESTS}
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo fmt --all --check \
    && cargo clippy --workspace --all-targets --locked -- -D warnings \
    && cargo test --workspace --locked

# --- runtime ----------------------------------------------------------------
# Distroless: no shell, no package manager, not root.
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=build /out/bin/ /usr/local/bin/
# Mount points the non-root user can write, for volumes created from them.
COPY --from=build --chown=nonroot:nonroot /out/state/ /var/lib/ic/
USER nonroot
EXPOSE 8080 7000
CMD ["/usr/local/bin/ic-api"]
