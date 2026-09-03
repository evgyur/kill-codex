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
- Work runs on one background worker so service polling and process waits never block painting or window input. While an action runs, all four buttons are disabled; a second destructive action cannot start concurrently. The worker places the completed report in a process-local synchronized mailbox and posts a payload-free completion message to the window thread. The window thread takes the report, updates the controls, and re-enables all buttons. If `PostMessageW` fails, the worker clears the mailbox. `WM_DESTROY` marks the window dead and clears the mailbox; the worker rechecks that flag around mailbox publication, so a late result cannot leave an owned heap payload or target a live replacement window.
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

After direct matches are found, the application expands the set through the process-parent graph so descendants with generic names are included. It excludes the process killer's own PID. Direct predicates do not overlap. A process whose name directly matches another target is still included when it is a verified descendant of the selected target; descendant ownership takes precedence over its executable name.

The first process snapshot is candidate discovery, not sufficient identity proof. Before terminating anything, the worker opens every candidate with `PROCESS_TERMINATE | SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION` and keeps all successful handles open until the operation ends. It then takes a second Toolhelp snapshot while those handles pin the current process objects. The second snapshot must still contain each PID and independently classify it as a direct match or a descendant of a direct match using the selected target's predicate. `GetProcessTimes` supplies creation times; a parent-child edge is accepted only when both identities are pinned and the child was created no earlier than the pinned parent. Because each PID is pinned before the authoritative second graph is evaluated, it cannot be redirected by reuse after validation. A replacement that existed when its handle was pinned is eligible only if the second graph currently classifies it for the selected target. Candidates that disappear, cannot be opened, cannot be reclassified, or cannot be creation-time validated are skipped and reported as failures.

## Amnezia Service Handling

The Amnezia action opens the Windows Service Control Manager with `SC_MANAGER_ENUMERATE_SERVICE`. It enumerates services and selects only:

- the exact service name `AmneziaVPN-service`, compared case-insensitively;
- service names beginning with `AmneziaWGTunnel$`, compared case-insensitively.

The state returned by `EnumServicesStatusExW` is checked before requesting stop access. A service already stopped is reported as unchanged without opening it with `SERVICE_STOP`. Every selected non-stopped service is then opened separately with `SERVICE_STOP | SERVICE_QUERY_STATUS`; service access does not come from the SCM handle. `STOP_PENDING` receives no duplicate control request. Stop controls are issued to all running tunnel services first and then to the main service, without serial polling between requests. A single five-second deadline starts after enumeration and successful service-handle acquisition; all opened services are polled together every 100 ms. This prevents one tunnel from consuming the main service's entire wait budget. A service is successful only when `SERVICE_STOPPED` is observed; deadline, access, query, and control errors are reported, but they do not prevent process termination from continuing.

Stopping services does not change their startup type and does not delete them. After service handling, the Amnezia process predicate and descendant expansion run normally to terminate any remaining application processes.

## Execution and Error Handling

Each button dispatches to one shared operation:

1. Resolve the `AppTarget` from the control identifier.
2. For Amnezia only, enumerate and stop matching services in the required order.
3. Take the discovery snapshot, find direct matches, expand candidate descendants, and open all candidate handles with terminate, synchronization, and query rights.
4. Take the authoritative second snapshot, rebuild the selected target graph, validate pinned creation-time parent edges, discard stale candidates, and order the retained handles deepest-first.
5. Issue `TerminateProcess` to every retained handle, wait for confirmed exits against the shared deadline, then close every handle; continue on per-process failure.
6. Render a combined status and details report through the synchronized GUI mailbox.

Failure to enumerate services or processes is shown in the details control. Partial success is preserved and reported. Process termination is a two-phase operation: issue `TerminateProcess` only to retained, validated target handles, then wait for signaled handles against one operation-wide three-second deadline. The report distinguishes confirmed exits, accepted termination requests that did not signal before the deadline, and failures. `TerminateProcess` returning success alone is not reported as a confirmed exit.

`CloseServiceHandle` releases every SCM and service handle. `CloseHandle` releases snapshots, process handles, and token handles. `DeleteObject` releases every owned GDI brush; stock objects are never deleted. The synchronized mailbox owns a completed report until the window thread takes it, the worker clears it after a failed post, or window destruction clears it; the completion message carries no report pointer or other owned payload.

## Safe Inspection

Preserve `--dry-run`. The exact grammar is `Kill-Codex.exe --dry-run [codex|amnezia|discord|chrome]`. Omitting the target inspects Codex for backward compatibility. Target names are case-insensitive. Any unknown target, duplicate target, extra argument, or conflicting argument displays usage and exits with code 2; it never silently falls back to Codex. Dry-run reports matching services and processes but never opens a service with stop access, sends a service control request, opens a process with terminate access, or calls `TerminateProcess`.

## Code Boundaries

The current codebase is intentionally compact. Keep the implementation in `src/main.rs`, but separate concerns with small types/functions:

- target metadata and name predicates;
- process enumeration, creation-time validation, pinned-handle ownership, descendant expansion, termination, and bounded exit confirmation;
- service enumeration, matching, stop requests, and bounded status polling;
- one background-operation dispatcher and its completion-message ownership contract;
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
- command-line parsing, including rejected unknown and extra arguments;
- an injected/fake system-operations implementation proving that dry-run performs enumeration only and never reaches stop or terminate operations;
- descendant edge rejection when creation times are invalid and stable handle identity for accepted candidates;
- asynchronous dispatch's global busy-state and completion-message mapping as pure state transitions;
- report output for empty, successful, failed, and mixed results where pure formatting functions make this practical.

Repository verification consists of:

1. `cargo fmt --check`
2. `cargo test`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo build --release`
5. Launch the release executable in `--dry-run` mode for each target and confirm that the result window appears without terminating anything.
6. Launch the release executable normally without pressing a kill button and manually inspect the 2×2 layout, exact labels, logos, initial status, and enabled states. Exercise the dispatch/state transition through a non-destructive test backend in unit tests rather than against live applications.

The destructive buttons are not exercised automatically against the user's live Codex, Amnezia, Discord, or Chrome sessions.

## Delivery

The release build remains a standalone Windows executable. Resolve the current user's Desktop through the Windows known-folder location exposed by PowerShell/.NET for the delivery step; do not assume a literal filesystem path. Copy the verified release binary there as `Kill-Codex.exe`, replacing the prior build only after verification succeeds.
