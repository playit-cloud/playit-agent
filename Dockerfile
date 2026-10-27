# syntax=docker/dockerfile:1
FROM rust:1.99-alpine3.24 AS build-env

WORKDIR /src/playit-agent

RUN apk add --no-cache build-base perl

COPY Cargo.toml Cargo.lock ./
COPY packages ./packages

ARG TARGETARCH
ARG CARGO_BUILD_JOBS=2
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=playit-target-${TARGETARCH},target=/src/playit-agent/target,sharing=locked \
    cargo build --release --locked --package playitd --bin playitd \
    && cp target/release/playitd /usr/local/bin/playitd

FROM build-env AS test
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=playit-target-${TARGETARCH},target=/src/playit-agent/target,sharing=locked \
    cargo test --workspace --all-targets --locked

FROM alpine:3.24
RUN apk add --no-cache ca-certificates

COPY --from=build-env /usr/local/bin/playitd /usr/local/bin/playitd
RUN mkdir /playit
COPY docker/entrypoint.sh /playit/entrypoint.sh
RUN chmod +x /playit/entrypoint.sh

ENTRYPOINT ["/playit/entrypoint.sh"]
