FROM rust:1.88-alpine AS build-env

WORKDIR /src/playit-agent

RUN apk --no-cache --update add build-base perl

# Setup project structure with blank code so we can download libraries for better docker caching
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p packages/playit-cli/src && mkdir -p packages/playit-ipc/src && mkdir -p packages/playitd/src && mkdir -p packages/playitd-tray/src && mkdir -p packages/playitd-windows-setup/src && mkdir -p packages/agent_core/src && mkdir -p packages/agent_proto/src && mkdir -p packages/api_client/src
COPY packages/playit-cli/Cargo.toml packages/playit-cli/Cargo.toml
COPY packages/playit-ipc/Cargo.toml packages/playit-ipc/Cargo.toml
COPY packages/playitd/Cargo.toml packages/playitd/Cargo.toml
COPY packages/playitd-tray/Cargo.toml packages/playitd-tray/Cargo.toml
COPY packages/playitd-windows-setup/Cargo.toml packages/playitd-windows-setup/Cargo.toml
COPY packages/agent_core/Cargo.toml packages/agent_core/Cargo.toml
COPY packages/agent_proto/Cargo.toml packages/agent_proto/Cargo.toml
COPY packages/api_client/Cargo.toml packages/api_client/Cargo.toml

RUN touch packages/playit-cli/src/lib.rs && touch packages/playit-ipc/src/lib.rs && touch packages/playitd/src/lib.rs && touch packages/playitd-tray/src/main.rs && touch packages/playitd-windows-setup/src/main.rs && touch packages/agent_core/src/lib.rs && touch packages/agent_proto/src/lib.rs && touch packages/api_client/src/lib.rs
RUN cargo fetch

# Build dep packages
COPY packages/agent_proto packages/agent_proto
RUN cargo build --release --package=playit-agent-proto

COPY packages/api_client packages/api_client
RUN cargo build --release --package=playit-api-client

COPY packages/agent_core packages/agent_core
RUN cargo build --release --package=playit-agent-core

# Build daemon
COPY packages/playit-ipc packages/playit-ipc
COPY packages/playitd packages/playitd
RUN cargo build --release --package playitd --bin playitd

########## RUNTIME CONTAINER ##########

FROM alpine:3.18
RUN apk add --no-cache ca-certificates

COPY --from=build-env /src/playit-agent/target/release/playitd /usr/local/bin/playitd
RUN mkdir /playit
COPY docker/entrypoint.sh /playit/entrypoint.sh
RUN chmod +x /playit/entrypoint.sh

ENTRYPOINT ["/playit/entrypoint.sh"]
