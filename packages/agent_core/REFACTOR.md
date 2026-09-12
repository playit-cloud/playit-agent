The refactor separates wire encoding, control session state, connection admission, and forwarding task ownership. Existing downstream packages remain in the workspace.

| Component | Responsibility |
| --- | --- |
| `agent_proto` | Wire messages, bounded token encoding, and UDP datagram classification. |
| `network::packet_io` | Datagram transport traits and dual-stack socket polling. |
| `ConnectedControl` | Endpoint validation, discovery refresh, registration exchanges, and queued client notifications. |
| `EstablishedControl` | Authenticated flow, session lease, matched ping responses, and MTU observations. |
| `MaintainedControl` | Request identifiers, heartbeat scheduling, renewal, and connection replacement. |
| `tcp_setup` | Origin resolution, claim exchange, origin connection, and proxy header transmission. |
| `TcpClients` | Admission limits, claim deduplication, pending tasks, active clients, and idle cleanup. |
| `TcpPipe` | Cancellable forwarding, byte accounting, and TCP half-close propagation. |
| `UdpChannel` | One worker owning session replacement, establishment timing, and tunnel socket I/O. |
| `UdpClients` | Full flow identity, generation-checked receiver IDs, origin sockets, and idle cleanup. |
| `Packets` | Owned buffers with semaphore permits and cancellation-safe allocation. |
| `PlayitAgent` | Control and UDP futures whose lifetimes follow the agent. |

The review found several concrete failures:

- Control expiry mixed server timestamps, local wall time, and sentinel values. A successful registration could immediately appear unauthenticated.
- Reused request IDs and unmatched pongs could update flow identity and liveness. Expiry subtraction could underflow.
- Registration consumed unrelated client notifications. Queued responses stopped receive processing during a sleep.
- TCP setup tasks were detached. Blocking writes escaped cancellation, and EOF did not explicitly propagate a write shutdown.
- UDP send and receive tasks shared partial session state. Dropping the channel could leave its receive task alive.
- UDP client keys omitted destination ports. Reply routing reconstructed port values with assertions and unchecked arithmetic.
- Packet allocation used raw pointers and a custom waiter queue with lost-wakeup and cancelled-waiter problems.
- Length-prefixed protocol tokens allocated memory before checking their length. Some encoders assumed reads and writes completed in one call.

Control scheduling and connection idle expiry now use Tokio's monotonic clock. Wire timestamps remain available for protocol messages and diagnostics.
Registration attempts have fixed receive deadlines, and unrelated packets cannot extend those deadlines. Up to 256 interleaved client notifications survive registration.
A fresh matched discovery pong precedes reauthentication. Failed address replacement leaves the current connection intact.
A stalled endpoint can be retried even when the routing list has not changed.

TCP setup has a 30-second total deadline, including origin resolution. Individual network stages retain shorter deadlines.
The default limits are 128 pending setups and 8,192 total TCP clients, including pending setups.
Claim reservations prevent duplicate notifications from starting concurrent or already-active claims.
A clean EOF shuts down the destination's write half while preserving reverse traffic. An I/O error cancels both directions.
Worker owners abort their tasks on drop. Explicit TCP manager shutdown cancels and joins pending setup tasks.

UDP defaults to 8,192 clients. TCP and UDP default to a 90-second idle timeout, refreshed by traffic in either direction.
Receiver IDs use `slotmap` generations, so late replies cannot attach to a reused slot.
Reply flows retain their complete addresses and port offsets. Session replacement clears the previous establishment timestamp.
Full queues and exhausted pools drop UDP packets without blocking receipt of establishment acknowledgements. Drop counters distinguish these cases.
Origin resolution for a new UDP flow has a two-second deadline.

These public interfaces changed:

- `UdpClients` no longer accepts wall-clock timestamps for packet dispatch or cleanup.
- `UdpChannel::send` and `update_session` return I/O results. `recv` returns `None` when its worker closes.
- `MaintainedControl::send_udp_session_auth` accepts a `Duration`.
- TCP and UDP settings include capacity and idle limits.
- `AgentRegister::update_signature` returns an I/O result for invalid encoding fields.
- Authentication implementations require `Send + Sync`. Transport futures require `Send`, without requiring `Sync`.
- Transport types moved to `network::packet_io`; the old control-module imports remain reexports.

Several limits remain explicit. UDP fragment reassembly is unsupported, so fragments are rejected before reaching an origin.
Packet buffers remain 2 KiB; outgoing payloads must also leave room for their flow footer. Oversized received datagrams are dropped rather than truncated.
The claim exchange still consumes an opaque eight-byte acknowledgement. Its contents need a server-side protocol contract before validation can be strengthened.
Establishment acknowledgements contain no session identifier. Delayed acknowledgements from the same endpoint cannot be attributed to a particular token generation.
Existing UDP clients retain their resolved origin until expiry. Updating routing does not migrate an active socket.
The API-backed origin catalog remains concrete; a separate resolver interface can be introduced when another catalog implementation exists.

Validation includes control expiry and stale-response tests, TCP claim and half-close tests, UDP port isolation, stale receiver IDs, and worker cancellation.
The existing IPv4/IPv6-origin integration tests pass. Wire compatibility tests remain in `agent_proto`.
The local UDP stress test passed 100,000 packets per payload size, in each direction, at 32, 128, 512, and 1,300 bytes.
These tests use local sockets and simulated clocks. They do not validate production control servers or Windows runtime behavior.

Reproduction commands:

```sh
cargo test -p playit-agent-core -p playit-agent-proto --all-targets
cargo test -p playit-agent-core --test udp_tunnel_integration udp_tunnel_stress_reports_bitrate_by_packet_size -- --ignored --nocapture
cargo check --workspace --all-targets
cargo clippy -p playit-agent-core -p playit-agent-proto --all-targets
```
