# Contributing

## Logging policy

Logs are part of the user interface: daemon output is shown in Docker logs,
service log files, the CLI TUI, and stdout attach mode.

- `ERROR` means the agent cannot do its job or the user must take action.
- `WARN` means the agent is degraded but retrying, or a suspicious
  configuration deserves attention. A transient warning must have a matching
  `INFO` recovery message.
- `INFO` is reserved for user-relevant lifecycle transitions such as startup,
  connection, tunnel changes, reconnection, and shutdown.
- `DEBUG` contains retries, individual connection attempts, protocol details,
  and per-connection lifecycle.
- `TRACE` contains packet-level diagnostics.

User-facing messages must be sentence case and stand on their own. Say what
happened, what it means, and what the user should do or what the agent will do
next. Do not expose Rust `Debug` output, module names, or protocol
implementation details at `INFO` or above. If the agent will silently recover,
the event is not an error.
