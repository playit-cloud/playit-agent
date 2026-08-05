# playit-api-client

This crate contains the typed HTTP client used by the agent and daemon. The
endpoint models and methods in `src/api.rs` are generated; the transport and
authentication behavior in `src/http_client.rs` is maintained by hand.

## Regenerating `api.rs`

The Rust generator and its authoritative API schema currently live with the
playit API service and are not vendored in this repository. The root
`agent-schema-release.json` file is agent release metadata, not the input for
this client.

To update the generated client:

1. Generate the Rust agent client from the target playit API revision using
   the API service's client generator.
2. Replace `src/api.rs` as one generated artifact. Do not make unrelated
   hand-written model changes in that file.
3. Preserve the `@generated` marker and the two local safety invariants until
   they are emitted by the generator: no-fail endpoint violations use a
   descriptive `unreachable!`, and `PlayitHttpClient::call` receives the
   tracked caller location.
4. Run `cargo fmt --all`, `cargo check --workspace --all-targets`,
   `cargo test --workspace`, and
   `cargo clippy --workspace --all-targets --all-features`.
5. Review the complete generated diff, especially endpoint paths, request and
   response types, failure enums, and serialization attributes. Record the API
   source revision in the commit message.

An API change is not reproducibly regenerable from this repository alone. If
the upstream generator or schema is unavailable, do not hand-approximate an
update; retain the checked-in artifact and coordinate with the API owner.
