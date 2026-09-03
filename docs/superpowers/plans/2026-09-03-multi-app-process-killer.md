# Multi-App Process Killer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the existing one-button Kill Codex utility into a safe four-button native Windows process killer for Codex, Amnezia, Discord, and Chrome, including Amnezia service shutdown.

**Architecture:** Keep the single-binary Rust/Win32 design and split `src/main.rs` into small target, process, service, reporting, dispatch, and drawing units. Destructive work runs on one background worker; the UI receives completion through a synchronized mailbox and payload-free window message. Process identity is pinned and revalidated before termination, while service shutdown uses bounded group polling.

**Tech Stack:** Rust 2024, `windows-sys` 0.59, native Win32/GDI/SCM APIs, Cargo tests, Clippy, Zig resource compiler.

---

## File Map

- Modify `Cargo.toml`: enable Win32 service-management APIs.
- Modify `src/main.rs`: target model, CLI parsing, safe process engine, SCM service engine, asynchronous GUI, 2×2 buttons, vector logos, and unit tests.
- Modify `README.md`: document four actions, Amnezia service behavior, safe inspection grammar, and release build.
- Create/update `docs/superpowers/plans/2026-09-03-multi-app-process-killer.md`: implementation record and checked execution steps.
- Produce `target/release/kill-codex.exe`: verified release artifact.
- Replace the current user's Desktop `Kill-Codex.exe` only after all checks pass.

### Task 1: Target model and safe command grammar

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add failing target and CLI tests**

Add tests covering all direct-name predicates, predicate isolation, button IDs, default dry-run target, explicit target parsing, and rejection of unknown/extra arguments:

```rust
#[test]
fn matches_each_target_without_cross_matching() {
    assert!(AppTarget::Codex.matches_process("ChatGPT.exe"));
    assert!(AppTarget::Codex.matches_process("codex-code-mode-host.exe"));
    assert!(AppTarget::Amnezia.matches_process("AmneziaVPN-service.exe"));
    assert!(AppTarget::Amnezia.matches_process("AmneziaWG.exe"));
    assert!(AppTarget::Discord.matches_process("Discord.exe"));
    assert!(AppTarget::Chrome.matches_process("chrome.exe"));
    assert!(!AppTarget::Chrome.matches_process("chromedriver.exe"));
    assert!(!AppTarget::Discord.matches_process("Update.exe"));
    assert!(!AppTarget::Codex.matches_process("Kill-Codex.exe"));
}

#[test]
fn parses_dry_run_grammar_strictly() {
    assert_eq!(parse_args(["app.exe", "--dry-run"]), Ok(RunMode::DryRun(AppTarget::Codex)));
    assert_eq!(parse_args(["app.exe", "--dry-run", "chrome"]), Ok(RunMode::DryRun(AppTarget::Chrome)));
    assert_eq!(parse_args(["app.exe"]), Ok(RunMode::Gui));
    assert!(parse_args(["app.exe", "--dry-run", "unknown"]).is_err());
    assert!(parse_args(["app.exe", "--dry-run", "chrome", "extra"]).is_err());
}
```

- [ ] **Step 2: Run focused tests and confirm red**

Run: `cargo test matches_each_target_without_cross_matching`

Run: `cargo test parses_dry_run_grammar_strictly`

Expected: compilation fails because `AppTarget`, `RunMode`, and `parse_args` are not defined.

- [ ] **Step 3: Implement target metadata and parser**

Add `AppTarget::{Codex, Amnezia, Discord, Chrome}`, stable control IDs `1001..=1004`, `label`, `display_name`, `matches_process`, `from_button_id`, and `from_name`. Add:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunMode { Gui, DryRun(AppTarget) }

fn parse_args<I, S>(args: I) -> Result<RunMode, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<String> = args.into_iter()
        .skip(1)
        .map(|value| value.as_ref().to_string_lossy().into_owned())
        .collect();
    match args.as_slice() {
        [] => Ok(RunMode::Gui),
        [flag] if flag.eq_ignore_ascii_case("--dry-run") =>
            Ok(RunMode::DryRun(AppTarget::Codex)),
        [flag, target] if flag.eq_ignore_ascii_case("--dry-run") =>
            AppTarget::from_name(target)
                .map(RunMode::DryRun)
                .ok_or_else(|| usage_error()),
        _ => Err(usage_error()),
    }
}
```

- [ ] **Step 4: Run the target tests**

Run: `cargo test`

Expected: all target/parser tests and the two existing tests pass.

- [ ] **Step 5: Commit the target model**

Run: `git add src/main.rs && git commit -m "feat: add multi-app target model"`

### Task 2: Safe process discovery and termination

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add failing pure graph and result tests**

Cover direct matches, generic descendants, cross-named descendants, self exclusion, invalid creation-time edges, and stale second-snapshot candidates:

```rust
#[test]
fn validates_selected_graph_against_second_snapshot() {
    let first = vec![
        ProcessInfo { pid: 10, parent_pid: 1, name: "codex.exe".into() },
        ProcessInfo { pid: 11, parent_pid: 10, name: "conhost.exe".into() },
        ProcessInfo { pid: 12, parent_pid: 11, name: "chrome.exe".into() },
    ];
    let second = first.clone();
    let pinned = HashMap::from([(10, 100_u64), (11, 110), (12, 120)]);
    assert_eq!(validated_target_pids(AppTarget::Codex, &first, &second, &pinned, 99),
               HashSet::from([10, 11, 12]));
}

#[test]
fn rejects_stale_or_impossible_parent_edges() {
    let first = vec![
        ProcessInfo { pid: 10, parent_pid: 1, name: "codex.exe".into() },
        ProcessInfo { pid: 11, parent_pid: 10, name: "conhost.exe".into() },
        ProcessInfo { pid: 12, parent_pid: 11, name: "helper.exe".into() },
    ];
    let mut second = first.clone();
    second.retain(|process| process.pid != 11);
    let pinned = HashMap::from([(10, 100_u64), (11, 110), (12, 90)]);
    assert_eq!(validated_target_pids(AppTarget::Codex, &first, &second, &pinned, 99),
               HashSet::from([10]));
}
```

- [ ] **Step 2: Confirm the new safety tests fail**

Run: `cargo test validates_selected_graph_against_second_snapshot`

Run: `cargo test rejects_stale_or_impossible_parent_edges`

Expected: compilation fails because the validated graph helper and pinned-time model are absent.

- [ ] **Step 3: Implement pinned handle ownership and two-snapshot validation**

Add an RAII handle wrapper and process candidate type:

```rust
struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) { unsafe { CloseHandle(self.0); } }
}

struct PinnedProcess {
    info: ProcessInfo,
    creation_time: u64,
    handle: OwnedHandle,
}
```

The process engine must:

1. enumerate the discovery snapshot;
2. compute candidate PIDs from the selected direct predicate and descendants;
3. open all candidates with `PROCESS_TERMINATE | SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION` and read `GetProcessTimes`;
4. enumerate a second snapshot while handles remain open;
5. rebuild membership from the selected direct predicate and only creation-time-valid parent edges;
6. retain only PIDs present in the authoritative graph;
7. issue termination to all retained handles before waiting;
8. use one three-second deadline with `WaitForSingleObject` and report confirmed, timed out, or failed results.

- [ ] **Step 4: Verify process behavior**

Run: `cargo test`

Expected: graph, identity, target, parser, and legacy tests all pass.

- [ ] **Step 5: Commit the process engine**

Run: `git add src/main.rs && git commit -m "feat: safely terminate selected process trees"`

### Task 3: Amnezia service shutdown

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/main.rs`

- [ ] **Step 1: Add failing service matching and orchestration tests**

```rust
#[test]
fn matches_only_amnezia_services() {
    assert!(is_amnezia_service("AmneziaVPN-service"));
    assert!(is_amnezia_service("amneziawgtunnel$AmneziaVPN"));
    assert!(!is_amnezia_service("WireGuardTunnel$other"));
    assert!(!is_amnezia_service("MyAmneziaHelper"));
}

#[test]
fn dry_run_never_mutates_system() {
    let fake = FakeSystemOps::with_amnezia_targets();
    let report = run_action(AppTarget::Amnezia, ExecutionMode::DryRun, &fake);
    assert!(!report.lines.is_empty());
    assert_eq!(fake.stop_service_calls(), 0);
    assert_eq!(fake.terminate_process_calls(), 0);
}
```

- [ ] **Step 2: Confirm service tests fail**

Run: `cargo test matches_only_amnezia_services`

Run: `cargo test dry_run_never_mutates_system`

Expected: compilation fails because service matching and the injectable execution boundary are absent.

- [ ] **Step 3: Enable and implement Service Control Manager APIs**

Add `"Win32_System_Services"` to the `windows-sys` feature list. Implement RAII `OwnedServiceHandle` using `CloseServiceHandle`; enumerate with `OpenSCManagerW(SC_MANAGER_ENUMERATE_SERVICE)` and `EnumServicesStatusExW`; match exact `AmneziaVPN-service` or case-insensitive `AmneziaWGTunnel$` prefix.

For non-stopped entries only, open with `SERVICE_STOP | SERVICE_QUERY_STATUS`. Issue stop controls to tunnels before the main service, then poll all opened handles every 100 ms against one five-second deadline. Report already-stopped, confirmed-stopped, timed-out, and failed states. Continue into process cleanup regardless of service errors.

- [ ] **Step 4: Verify the service and dry-run boundaries**

Run: `cargo test`

Expected: all tests pass; the fake backend records zero mutation calls in dry-run.

- [ ] **Step 5: Commit service support**

Run: `git add Cargo.toml Cargo.lock src/main.rs && git commit -m "feat: stop Amnezia services safely"`

### Task 4: Asynchronous four-button Win32 interface

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add failing dispatch-state tests**

```rust
#[test]
fn dispatch_blocks_concurrent_actions_until_completion() {
    let mut state = UiState::default();
    assert!(state.begin(AppTarget::Chrome));
    assert!(!state.begin(AppTarget::Discord));
    state.complete();
    assert!(state.begin(AppTarget::Discord));
}

#[test]
fn every_button_maps_to_exactly_one_target() {
    assert_eq!(AppTarget::from_button_id(ID_KILL_CODEX), Some(AppTarget::Codex));
    assert_eq!(AppTarget::from_button_id(ID_KILL_AMNEZIA), Some(AppTarget::Amnezia));
    assert_eq!(AppTarget::from_button_id(ID_KILL_DISCORD), Some(AppTarget::Discord));
    assert_eq!(AppTarget::from_button_id(ID_KILL_CHROME), Some(AppTarget::Chrome));
}
```

- [ ] **Step 2: Confirm dispatch tests fail**

Run: `cargo test dispatch_blocks_concurrent_actions_until_completion`

Run: `cargo test every_button_maps_to_exactly_one_target`

Expected: compilation fails until the four IDs and `UiState` exist.

- [ ] **Step 3: Build the 2×2 UI and background dispatcher**

Create four owner-drawn controls, store their handles, and map `WM_COMMAND` IDs through `AppTarget::from_button_id`. On a valid click, atomically enter busy state, disable all buttons, set the selected status, and spawn one worker thread. The worker runs the shared action, publishes `ActionReport` into `OnceLock<Mutex<Option<ActionReport>>>`, and posts `WM_APP + 1` with no payload. Completion takes the report, renders it, exits busy state, and enables every button. `WM_DESTROY` marks the window dead and clears the mailbox; failed posting also clears it.

Draw the buttons through shared owner-draw code using target-specific colors and GDI primitives. Render recognizable monochrome Codex, Amnezia, and Discord marks and a multicolor Chrome ring. Center each logo and exact English label with pressed-state feedback. Delete every created brush/pen after drawing and never delete stock objects.

- [ ] **Step 4: Verify UI state and compile quality**

Run: `cargo fmt --check && cargo test && cargo clippy --all-targets -- -D warnings`

Expected: formatting is clean, all tests pass, and Clippy emits no warnings.

- [ ] **Step 5: Commit the interface**

Run: `git add src/main.rs && git commit -m "feat: add four-button process killer UI"`

### Task 5: Documentation, release build, and desktop delivery

**Files:**
- Modify: `README.md`
- Produce: `target/release/kill-codex.exe`
- Replace: current user's Desktop `Kill-Codex.exe`

- [ ] **Step 1: Update user documentation**

Document all four buttons, exact process families, Amnezia service handling, immediate destructive behavior, unsaved-work warning, and exact safe-inspection forms:

```powershell
Kill-Codex.exe --dry-run
Kill-Codex.exe --dry-run amnezia
Kill-Codex.exe --dry-run discord
Kill-Codex.exe --dry-run chrome
```

- [ ] **Step 2: Run the complete source-quality gate**

Run: `cargo fmt --check && cargo test && cargo clippy --all-targets -- -D warnings && git diff --check`

Expected: all commands exit 0 with every unit test passing and no warnings or whitespace errors.

- [ ] **Step 3: Build the final release binary**

Run: `cargo build --release`

Expected: exit 0 and `target/release/kill-codex.exe` exists as a native Windows GUI executable with embedded application resources.

- [ ] **Step 4: Exercise all non-destructive launch paths**

Launch each dry-run target and confirm the informational window appears without terminating live processes. Launch the main GUI without clicking destructive buttons and inspect the 2×2 layout, labels, logos, enabled states, status, and window repainting.

- [ ] **Step 5: Copy the verified candidate to Desktop**

Resolve Desktop with `[Environment]::GetFolderPath('Desktop')`, copy to a temporary sibling name, verify size and SHA-256 against `target/release/kill-codex.exe`, then atomically replace `Kill-Codex.exe`. Do not run any destructive button during delivery verification.

- [ ] **Step 6: Commit documentation and record final evidence**

Run: `git add README.md docs/superpowers/plans/2026-09-03-multi-app-process-killer.md && git commit -m "docs: document multi-app process killer"`

Record final `cargo test`, Clippy, release build, executable SHA-256, desktop SHA-256, and `git status --short` results in the completion report.
