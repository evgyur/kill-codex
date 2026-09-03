# Multi-App Process Killer Design

## Goal

Expand the existing native Windows utility from one Codex action to four immediate actions: `KILL CODEX`, `KILL AMNEZIA`, `KILL DISCORD`, and `KILL CHROME`. Each action terminates only the selected application's known processes and their descendants. The Amnezia action also stops its Windows services.

## Scope

The work remains a single small Rust/Win32 executable. It does not add an installer, background agent, telemetry, configuration UI, confirmation dialogs, or automatic process discovery by executable path. Existing elevation behavior remains: the application relaunches through UAC before showing the main window.

## User Interface

- Rename the main heading and window caption to `KILL APPS`.
- Replace the single action button with four owner-drawn buttons in a 2×2 grid.
- Button labels are exactly:
  - `KILL CODEX`
  - `KILL AMNEZIA`
  - `KILL DISCORD`
  - `KILL CHROME`
- Each button uses an application-specific color and a recognizable vector logo drawn through GDI. Logos are compiled into behavior/code and require no external image files at runtime.
- A click executes immediately without a confirmation dialog.
- While an action runs, the clicked button is disabled to prevent re-entry. It is re-enabled after the operation completes.
- The existing status and read-only details controls remain below the grid. They report the selected application, the number of stopped services and processes, individual successes, and individual failures.
- If nothing is running, the details control says that no processes or services for the selected application were found.

## Target Model

Introduce an `AppTarget` enum with `Codex`, `Amnezia`, `Discord`, and `Chrome`. Each target provides its button identifier, display label, process-name predicate, colors, and optional service-name predicate.

Process matching is case-insensitive and intentionally narrow:

| Target | Direct process matches |
| --- | --- |
| Codex | `ChatGPT.exe`, `ChatGPT`, `codex.exe`, `codex`, and names beginning with `codex-` |
| Amnezia | `AmneziaVPN.exe`, `AmneziaVPN`, `AmneziaVPN-service.exe`, `AmneziaVPN-service`, `AmneziaWG.exe`, and `AmneziaWG` |
| Discord | `Discord.exe` and `Discord` |
| Chrome | `chrome.exe` and `chrome` |

After direct matches are found, the application expands the set through the process-parent graph so descendants with generic names are included. It excludes the process killer's own PID. Targets do not overlap: clicking one button never runs another target's predicate.

## Amnezia Service Handling

The Amnezia action opens the Windows Service Control Manager with enumerate and stop access. It enumerates services and selects only:

- the exact service name `AmneziaVPN-service`;
- service names beginning with `AmneziaWGTunnel$`.

Tunnel services are stopped before the main service. A service already stopped is reported as unchanged, not as an error. A stop request is considered successful when the service is already stopped or reaches the stopped state within a bounded wait. Timeout and access errors are reported, but they do not prevent process termination from continuing.

Stopping services does not change their startup type and does not delete them. After service handling, the Amnezia process predicate and descendant expansion run normally to terminate any remaining application processes.

## Execution and Error Handling

Each button dispatches to one shared operation:

1. Resolve the `AppTarget` from the control identifier.
2. For Amnezia only, enumerate and stop matching services in the required order.
3. Snapshot processes once, find direct matches, expand descendants, and order deepest descendants first.
4. Open each process with `PROCESS_TERMINATE`, terminate it, close the handle, and continue on per-process failure.
5. Render a combined status and details report.

Failure to enumerate services or processes is shown in the details control. Partial success is preserved and reported. Handles returned by Win32 APIs are closed on every path.

## Safe Inspection

Preserve `--dry-run`. With no target argument it inspects Codex for backward compatibility. An optional target name (`codex`, `amnezia`, `discord`, or `chrome`) selects another target. Dry-run reports matching services and processes but never sends a service control request and never calls `TerminateProcess`.

## Code Boundaries

The current codebase is intentionally compact. Keep the implementation in `src/main.rs`, but separate concerns with small types/functions:

- target metadata and name predicates;
- process enumeration, descendant expansion, and termination;
- service enumeration, matching, stop requests, and bounded status polling;
- Win32 control construction and owner drawing;
- report formatting and dry-run output.

No new third-party runtime dependency is required. Add only the necessary `windows-sys` feature flags for Service Control Manager APIs.

## Verification

Unit tests cover:

- positive and negative process-name matches for all four targets;
- isolation between target predicates;
- descendant expansion and self-PID exclusion;
- exact and prefix-based Amnezia service matching;
- button identifier mapping;
- report output for empty, successful, failed, and mixed results where pure formatting functions make this practical.

Repository verification consists of:

1. `cargo fmt --check`
2. `cargo test`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo build --release`
5. Launch the release executable in `--dry-run` mode for each target and confirm that the result window appears without terminating anything.

The destructive buttons are not exercised automatically against the user's live Codex, Amnezia, Discord, or Chrome sessions.

## Delivery

The release build remains a standalone Windows executable. Copy the verified release binary to the desktop as `Kill-Codex.exe`, replacing the prior build only after verification succeeds.
