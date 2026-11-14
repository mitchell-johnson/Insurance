# The self-hosted GitHub Actions runner that CI runs on: Ubuntu 24.04 (the
# same release as GitHub's ubuntu-latest) with the tools ci.yml and drill.yml
# use. It runs inside Docker Desktop on the Mac mini and drives that Docker
# through its socket, so the Temporal and PostgreSQL containers and the
# Compose cluster run beside it. tools/ci/runner/setup.sh builds, registers
# and starts it; see "CI" in docs/architecture.md.
#
# Everything the runner keeps between jobs (its registration, the work
# directory, Cargo's downloads, the Rust toolchains and the build output) is
# under /data, a Docker volume per runner, so rebuilding this image loses
# nothing.

FROM ubuntu:24.04

ARG RUNNER_VERSION=2.338.2
ARG RUNNER_SHA256=670adb2f268fe21dcbb58b3db0e0aa9d5b3bddfe2c651481336e561ff86de945
ARG RUSTUP_VERSION=1.29.3
ARG RUSTUP_SHA256=9f608c142e0e38a78957d0661ba1dfaf0b6fb37184cfaa4b9a0aa57af6184282
ARG KUBECTL_VERSION=1.37.2
ARG KUBECTL_SHA256=fcc16f63186659d9722014b7e02d5fdd2d4e669cb8c0ab66365d7d3b8ba03509
ARG BICEP_VERSION=0.49.2
ARG BICEP_SHA256=48d8f152bd78c8848a41a07ac2d12b573306d9329fc961a85354a98a95cc275e

ENV DEBIAN_FRONTEND=noninteractive

# Build dependencies of the workspace (clang for the Starlark and crypto
# builds, protoc and the well-known .proto files for the Temporal SDK), the
# Docker CLI with Compose for the test services and the drill, and what the
# runner and the actions it runs need.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl git git-lfs gnupg jq sudo unzip xz-utils zstd tini \
        build-essential pkg-config libssl-dev clang protobuf-compiler libprotobuf-dev \
        mold python3 \
    && install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc \
    && echo "deb [arch=arm64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu noble stable" \
        > /etc/apt/sources.list.d/docker.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends docker-ce-cli docker-buildx-plugin docker-compose-plugin \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL -o /usr/local/bin/kubectl "https://dl.k8s.io/release/v${KUBECTL_VERSION}/bin/linux/arm64/kubectl" \
    && echo "${KUBECTL_SHA256}  /usr/local/bin/kubectl" | sha256sum -c - \
    && curl -fsSL -o /usr/local/bin/bicep "https://github.com/Azure/bicep/releases/download/v${BICEP_VERSION}/bicep-linux-arm64" \
    && echo "${BICEP_SHA256}  /usr/local/bin/bicep" | sha256sum -c - \
    && curl -fsSL -o /usr/local/bin/rustup-init "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/aarch64-unknown-linux-gnu/rustup-init" \
    && echo "${RUSTUP_SHA256}  /usr/local/bin/rustup-init" | sha256sum -c - \
    && chmod 0755 /usr/local/bin/kubectl /usr/local/bin/bicep /usr/local/bin/rustup-init

# Rust links through clang with mold rather than GNU ld. Every test
# executable links the whole dependency tree; GNU ld took minutes and
# gigabytes a binary, and parallel links ran the Docker machine out of memory
# (the kernel killed the linker). A linker wrapper, not RUSTFLAGS, because
# cargo llvm-cov sets RUSTFLAGS and the wrapper must still apply.
RUN printf '#!/bin/sh\nexec clang -fuse-ld=mold "$@"\n' > /usr/local/bin/clang-mold \
    && chmod 0755 /usr/local/bin/clang-mold

# The runner works as an ordinary user with password-free sudo, as on
# GitHub's hosted runners. The ubuntu image's own uid 1000 user is removed so
# the runner can take that uid.
RUN userdel -r ubuntu 2>/dev/null || true \
    && useradd --create-home --uid 1000 --shell /bin/bash runner \
    && echo 'runner ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/runner \
    && chmod 0440 /etc/sudoers.d/runner \
    && mkdir -p /opt/actions-runner /data/target \
    && curl -fsSL -o /tmp/runner.tar.gz "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-arm64-${RUNNER_VERSION}.tar.gz" \
    && echo "${RUNNER_SHA256}  /tmp/runner.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/runner.tar.gz -C /opt/actions-runner \
    && rm /tmp/runner.tar.gz \
    && /opt/actions-runner/bin/installdependencies.sh \
    && rm -rf /var/lib/apt/lists/* \
    && chown -R runner:runner /opt/actions-runner /data

ENV CARGO_HOME=/data/cargo \
    RUSTUP_HOME=/data/rustup \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=/usr/local/bin/clang-mold \
    PATH=/data/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    RUNNER_ALLOW_RUNASROOT=0 \
    ACTIONS_RUNNER_HOOK_JOB_STARTED=/usr/local/bin/runner-job-cleanup \
    ACTIONS_RUNNER_HOOK_JOB_COMPLETED=/usr/local/bin/runner-job-cleanup

COPY --chmod=0755 entrypoint.sh /usr/local/bin/runner-entrypoint
# Kills what a timed-out job left running; see the script.
COPY --chmod=0755 job-cleanup.sh /usr/local/bin/runner-job-cleanup

USER runner
WORKDIR /opt/actions-runner
VOLUME ["/data"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/runner-entrypoint"]
CMD ["run"]
