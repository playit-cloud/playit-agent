# Agent architecture

Dependencies point from entry points toward application and engine contracts.
Generated API types stop in `playit-runtime/src/generated_gateway.rs`.
IPC version 2 types stop in `playit-ipc` and `playitd/src/ipc_server.rs`.
Operating-system mechanisms stop in `playit-platform`.

## Owners

| Concern | Owner |
| --- | --- |
| Application lifecycle | `playit-model::Phase` |
| Client-visible state | `playit-model::AppSnapshot` |
| Snapshot publication | `playit-runtime::SnapshotStore` |
| Application tasks and commands | `playit-runtime::AppSupervisor` |
| Interactive claim session, polling, and exchange | `playit-runtime::AppSupervisor` and `ClaimService` |
| Validated runtime limits and locations | `playit-model::AppConfig` |
| Tunnel-engine tasks | `playit-agent-core::EngineSupervisor` |
| Generated HTTP conversion | `playit-runtime::GeneratedClientGateway` |
| Installed-service stop policy | `playit-runtime::stop_installed_service_with_fallback` |
| Files, services, permissions, SIDs, and shortcuts | `playit-platform` |
| IPC version 2 conversion | `playitd::ipc_server` |

`playit-platform` owns service-manager calls but does not decide application lifecycle policy.
CLI and tray code supply IPC and service-manager effects to the runtime-owned stop workflow.
Standalone CLI claim commands may use `ClaimService` without creating a daemon session.
Interactive setup uses the daemon session and sends only typed claim commands over IPC.

Ordinary TCP connection and UDP flow I/O failures stay local to that child.
Panics, invariant failures, and shutdown-deadline failures can stop their owning service.

## Compatibility

IPC version 2 and installed file paths remain stable.
`playit-ipc::get_default_socket_path` remains as the transport's public default-endpoint convenience function.
Platform path modules are not re-exported through `playitd` or `playit-ipc`.

The dependency-boundary script checks these ownership rules in CI.
