# Both official-image indexes were resolved from Docker Hub on 2026-10-30.
FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
RUN apt-get update && apt-get install -y --no-install-recommends python3 pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
ARG SOURCE_REVISION
ARG MCPORT_TEST_DATABASE_URL
ENV MCPORT_BUILD_REVISION=${SOURCE_REVISION} CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
RUN test "$(rustc --version | cut -d' ' -f2)" = 1.98.0 \
    && test "${#SOURCE_REVISION}" = 40 \
    && cargo test --locked -p mcport-server -p mcport-core -p mcport-mcp \
    && if [ -n "$MCPORT_TEST_DATABASE_URL" ]; then \
         cargo test --locked -p mcport-server postgres_import_and_runtime_contract -- --ignored; \
       fi \
    && cargo build --locked --release -p mcport-server

FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 libssl3 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /source/target/release/mcport-server /usr/local/bin/mcport-server
ARG SOURCE_REVISION
LABEL org.opencontainers.image.source="https://github.com/teamofsilicons/silicon-mcport" \
      org.opencontainers.image.revision="${SOURCE_REVISION}" \
      org.opencontainers.image.title="Silicon MCPort service" \
      org.opencontainers.image.licenses="MIT"
ENV MCPORT_BIND=0.0.0.0:4381 MCPORT_DATA_DIR=/tmp/mcport PORT=4381
USER 10001:10001
EXPOSE 4381
ENTRYPOINT ["/usr/local/bin/mcport-server"]
