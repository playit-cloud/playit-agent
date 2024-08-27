COPY packages/api_client/Cargo.toml packages/api_client/Cargo.toml
COPY packages/ping_monitor/Cargo.toml packages/ping_monitor/Cargo.toml

RUN touch packages/agent_cli/src/lib.rs && touch packages/agent_core/src/lib.rs && touch packages/agent_proto/src/lib.rs && touch packages/api_client/src/lib.rs && touch packages/ping_monitor/src/lib.rs
RUN cargo fetch

# Build dep packages
COPY packages/agent_proto packages/agent_proto
RUN cargo build --release --package=playit-agent-proto

COPY packages/api_client packages/api_client
RUN cargo build --release --package=playit-api-client

COPY packages/ping_monitor packages/ping_monitor
RUN cargo build --release --package=playit-ping-monitor

COPY packages/agent_core packages/agent_core
RUN cargo build --release --package=playit-agent-core

# Build CLI
COPY packages/agent_cli packages/agent_cli
RUN cargo build --release --all

########## RUNTIME CONTAINER ##########

FROM alpine:3.18
ARG PLAYIT_GUID=2000
ARG PLAYIT_UUID=2000
RUN apk add --no-cache ca-certificates

COPY --from=build-env /src/playit-agent/target/release/playit-cli /usr/local/bin/playit
RUN mkdir /playit
COPY --chmod=1755 docker/entrypoint.sh /playit/

RUN addgroup -g ${PLAYIT_GUID} playit && adduser -Sh /playit -u ${PLAYIT_UUID} -G playit playit
USER playit

ENTRYPOINT ["/playit/entrypoint.sh"]
