#!/usr/bin/env bash
set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
cd "$repository_root"

forbidden_api='playit_api_client|playit-api-client|PlayitApi'
if grep -R -n -E "$forbidden_api" packages/agent_core packages/playitd/src packages/playitd/Cargo.toml; then
  echo "agent_core and playitd must access the generated API client through playit-runtime" >&2
  exit 1
fi

if grep -R -n -E 'playit_runtime|playit-runtime' \
  packages/agent_core/src packages/agent_core/Cargo.toml; then
  echo "agent_core must not depend on the application runtime" >&2
  exit 1
fi

forbidden_ipc='playit_api_client|playit-api-client|PlayitApi|reqwest|login_guest|guest_login_cache'
if grep -R -n -E "$forbidden_ipc" packages/playit-ipc packages/playitd/src/ipc_server.rs; then
  echo "IPC must not own HTTP clients or login caches" >&2
  exit 1
fi

if grep -R -n -E 'Client[A-Z][A-Za-z]*View|playit_ipc|playit-ipc|interprocess|reqwest|tokio|windows[_-](sys|service)' \
  packages/playit-model/src packages/playit-model/Cargo.toml; then
  echo "playit-model must not contain presentation, IPC, HTTP, runtime, or OS types" >&2
  exit 1
fi

if grep -R -n -E 'playit[_-](model|runtime)|api_client|playit-api-client' \
  packages/playit-platform/src packages/playit-platform/Cargo.toml; then
  echo "playit-platform must contain OS mechanisms, not application lifecycle or API policy" >&2
  exit 1
fi

if grep -R -n -E '^pub use playit_platform' packages/playitd/src packages/playit-ipc/src; then
  echo "entry-point and IPC crates must not re-export platform compatibility paths" >&2
  exit 1
fi

if [ -e packages/playit-ipc/src/paths.rs ] || grep -q '^pub mod paths;' packages/playit-ipc/src/lib.rs; then
  echo "playit-ipc path forwarding must not return" >&2
  exit 1
fi

if [ -e packages/playit-runtime/src/gateway.rs ] || \
  grep -R -n -E '^pub use (playit_agent_core|crate::gateway)' \
    packages/playit-runtime/src packages/agent_core/src; then
  echo "runtime and agent_core migration re-exports must not return" >&2
  exit 1
fi

api_adapter_files="$(grep -R -l -E 'playit_api_client|PlayitApi' \
  packages --include='*.rs' --exclude-dir=api_client || true)"
if [ "$api_adapter_files" != "packages/playit-runtime/src/generated_gateway.rs" ]; then
  echo "expected one generated API adapter source, found:" >&2
  printf '%s\n' "$api_adapter_files" >&2
  exit 1
fi

require_one_definition() {
  local pattern="$1"
  local label="$2"
  local count
  count="$(grep -R -h -E "$pattern" packages --include='*.rs' | wc -l)"
  if [ "$count" -ne 1 ]; then
    echo "expected exactly one $label definition, found $count" >&2
    exit 1
  fi
}

require_one_definition '^pub enum Phase[[:space:]]*\{' 'application lifecycle enum'
require_one_definition '^pub struct AppSnapshot[[:space:]]*\{' 'application snapshot'
require_one_definition '^pub struct SnapshotStore[[:space:]]*\{' 'snapshot authority'
require_one_definition '^pub struct AppSupervisor[[:space:]]*\{' 'application supervisor'
require_one_definition '^pub struct EngineSupervisor[[:space:]]*\{' 'engine supervisor'
require_one_definition '^pub struct GeneratedClientGateway[[:space:]]*\{' 'generated API adapter'
require_one_definition '^pub struct SupervisedEnginePort[[:space:]]*\{' 'engine application adapter'
require_one_definition '^pub async fn stop_installed_service_with_fallback' 'installed-service stop policy'
require_one_definition '^pub enum EngineExit[[:space:]]*\{' 'engine exit type'
require_one_definition '^pub enum EngineService[[:space:]]*\{' 'engine service type'
require_one_definition '^pub enum ServiceExit[[:space:]]*\{' 'service exit type'

if grep -R -n 'PhaseKind' packages --include='*.rs'; then
  echo "application lifecycle projections must match directly on Phase" >&2
  exit 1
fi

owned_task_sources=(
  packages/playit-cli/src/main.rs
  packages/playit-cli/src/signal_handle.rs
  packages/playitd/src/daemon.rs
  packages/playitd/src/ipc_server.rs
  packages/playit-runtime/src/engine.rs
  packages/agent_core/src/playit_agent.rs
  packages/agent_core/src/network
)
if grep -R -n -E '^[[:space:]]*tokio::spawn[[:space:]]*\(' "${owned_task_sources[@]}"; then
  echo "production tasks must retain a join owner" >&2
  exit 1
fi

if grep -n -E '^playitd[[:space:]]*=' \
  packages/playit-cli/Cargo.toml \
  packages/playitd-tray/Cargo.toml \
  packages/playitd-windows-setup/Cargo.toml; then
  echo "clients and installer helpers must use playit-platform instead of playitd platform internals" >&2
  exit 1
fi

shortcut_owners="$(grep -R -l 'CoCreateInstance.*ShellLink\|CoCreateInstance(&ShellLink' \
  packages --include='*.rs' || true)"
if [ "$(printf '%s\n' "$shortcut_owners" | sed '/^$/d' | wc -l)" -ne 1 ]; then
  echo "expected exactly one Windows COM shortcut implementation, found:" >&2
  printf '%s\n' "$shortcut_owners" >&2
  exit 1
fi
