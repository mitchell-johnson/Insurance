# syntax=docker/dockerfile:1.7

# --- build and test -------------------------------------------------------
FROM rust:1.97-bookworm AS build
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends clang && rm -rf /var/lib/apt/lists/*
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --workspace --locked \
    && mkdir -p /out \
    && find target/release -maxdepth 1 -type f -executable -name 'ic-*' -exec cp {} /out/ \;

# `docker build --target test .` runs the whole test suite in the image.
FROM build AS test
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo fmt --all --check \
    && cargo clippy --release --workspace --all-targets --locked -- -D warnings \
    && cargo test --release --workspace --locked

# --- runtime ----------------------------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=build /out/ /usr/local/bin/
USER nonroot
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/ic-api"]
