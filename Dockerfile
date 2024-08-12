FROM rust:1.80-alpine AS build-env

WORKDIR /src/playit-agent

RUN apk --no-cache --update add build-base perl

# Setup project structure with blank code so we can download libraries for better docker caching
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p packages/agent_cli/src && mkdir -p packages/agent_core/src && mkdir -p packages/agent_proto/src
COPY packages/agent_cli/Cargo.toml packages/agent_cli/Cargo.toml
COPY packages/agent_core/Cargo.toml packages/agent_core/Cargo.toml
COPY packages/agent_proto/Cargo.toml packages/agent_proto/Cargo.toml

RUN touch packages/agent_cli/src/lib.rs && touch packages/agent_core/src/lib.rs && touch packages/agent_proto/src/lib.rs
RUN cargo fetch

# Build dep packages
COPY packages/agent_proto packages/agent_proto
RUN cargo build --release --package=playit-agent-proto

COPY packages/agent_core packages/agent_core
RUN cargo build --release --package=playit-agent-core

# Build CLI
COPY packages/agent_cli packages/agent_cli
RUN cargo build --release --all

########## RUNTIME CONTAINER ##########

FROM alpine:3.18
RUN apk add --no-cache ca-certificates

COPY --from=build-env /src/playit-agent/target/release/playit-cli /usr/local/bin/playit
RUN mkdir /playit
COPY docker/entrypoint.sh /playit/entrypoint.sh
RUN chmod +x /playit/entrypoint.sh

ENTRYPOINT ["/playit/entrypoint.sh"]
