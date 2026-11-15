#![windows_subsystem = "windows"]

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_MORE_DATA, ERROR_NO_MORE_FILES, ERROR_SERVICE_NOT_ACTIVE, FILETIME, HANDLE,
    HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, POINT, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateSolidBrush, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, Ellipse, FillRect, GetStockObject, NULL_PEN, Pie, Polygon, RoundRect,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT, UpdateWindow,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW,
    OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_ENUM_PROCESS_INFO, SC_MANAGER_CONNECT,
    SC_MANAGER_ENUMERATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_CONTROL_STOP,
    SERVICE_QUERY_STATUS, SERVICE_STATE_ALL, SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP,
    SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_WIN32,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_SELECTED};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const ID_KILL_CODEX: usize = 1001;
const ID_KILL_AMNEZIA: usize = 1002;
const ID_KILL_DISCORD: usize = 1003;
const ID_KILL_CHROME: usize = 1004;
const WM_ACTION_COMPLETE: u32 = WM_APP + 1;
const PROCESS_WAIT: Duration = Duration::from_secs(3);
const SERVICE_WAIT: Duration = Duration::from_secs(5);

static STATUS: AtomicIsize = AtomicIsize::new(0);
static DETAILS: AtomicIsize = AtomicIsize::new(0);
static BUTTONS: [AtomicIsize; 4] = [const { AtomicIsize::new(0) }; 4];
static WINDOW_ALIVE: AtomicBool = AtomicBool::new(false);
static REPORT_MAILBOX: OnceLock<Mutex<Option<ActionReport>>> = OnceLock::new();
static UI_STATE: OnceLock<Mutex<UiState>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppTarget {
    Codex,
    Amnezia,
    Discord,
    Chrome,
}

impl AppTarget {
    const ALL: [Self; 4] = [Self::Codex, Self::Amnezia, Self::Discord, Self::Chrome];

    fn index(self) -> usize {
        match self {
            Self::Codex => 0,
            Self::Amnezia => 1,
            Self::Discord => 2,
            Self::Chrome => 3,
        }
    }

    fn button_id(self) -> usize {
        match self {
            Self::Codex => ID_KILL_CODEX,
            Self::Amnezia => ID_KILL_AMNEZIA,
            Self::Discord => ID_KILL_DISCORD,
            Self::Chrome => ID_KILL_CHROME,
        }
    }

    fn from_button_id(id: usize) -> Option<Self> {
        match id {
            ID_KILL_CODEX => Some(Self::Codex),
            ID_KILL_AMNEZIA => Some(Self::Amnezia),
            ID_KILL_DISCORD => Some(Self::Discord),
            ID_KILL_CHROME => Some(Self::Chrome),
            _ => None,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|target| target.slug().eq_ignore_ascii_case(name))
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Amnezia => "amnezia",
            Self::Discord => "discord",
            Self::Chrome => "chrome",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Amnezia => "Amnezia",
            Self::Discord => "Discord",
            Self::Chrome => "Chrome",
        }
    }

    fn button_label(self) -> &'static str {
        match self {
            Self::Codex => "KILL CODEX",
            Self::Amnezia => "KILL AMNEZIA",
            Self::Discord => "KILL DISCORD",
            Self::Chrome => "KILL CHROME",
        }
    }

    fn matches_process(self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        match self {
            Self::Codex => {
                name == "chatgpt.exe"
                    || name == "chatgpt"
                    || name == "codex.exe"
                    || name == "codex"
                    || name == "deepseekdesktop.exe"
                    || name == "deepseekdesktop"
                    || name.starts_with("codex-")
            }
            Self::Amnezia => matches!(
                name.as_str(),
                "amneziavpn.exe"
                    | "amneziavpn"
                    | "amneziavpn-service.exe"
                    | "amneziavpn-service"
                    | "amneziawg.exe"
                    | "amneziawg"
            ),
            Self::Discord => name == "discord.exe" || name == "discord",
            Self::Chrome => name == "chrome.exe" || name == "chrome",
        }
    }

    fn colors(self, pressed: bool, disabled: bool) -> u32 {
        if disabled {
            return rgb(142, 142, 146);
        }
        match (self, pressed) {
            (Self::Codex, false) => rgb(32, 33, 35),
            (Self::Codex, true) => rgb(5, 5, 5),
            (Self::Amnezia, false) => rgb(102, 66, 224),
            (Self::Amnezia, true) => rgb(75, 43, 183),
            (Self::Discord, false) => rgb(88, 101, 242),
            (Self::Discord, true) => rgb(64, 78, 204),
            (Self::Chrome, false) => rgb(31, 111, 216),
            (Self::Chrome, true) => rgb(20, 82, 164),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunMode {
    Gui,
    DryRun(AppTarget),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionMode {
    Execute,
    DryRun,
}

#[derive(Default)]
struct UiState {
    busy: bool,
}

impl UiState {
    fn begin(&mut self, _target: AppTarget) -> bool {
        if self.busy {
            false
        } else {
            self.busy = true;
            true
        }
    }

    fn complete_if_report(&mut self, report_present: bool) -> bool {
        if self.busy && report_present {
            self.busy = false;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProcessInfo {
    pid: u32,
    parent_pid: u32,
    name: String,
}

#[derive(Clone, Debug)]
struct ServiceInfo {
    name: String,
    state: u32,
}

#[derive(Default)]
struct OperationResult {
    lines: Vec<String>,
    succeeded: usize,
    failed: usize,
}

struct ActionReport {
    target: AppTarget,
    lines: Vec<String>,
    processes: usize,
    services: usize,
    failed: usize,
    dry_run: bool,
}

impl ActionReport {
    fn status(&self) -> String {
        if self.dry_run {
            return format!(
                "Проверка {}: процессов — {}, служб — {}",
                self.target.display_name(),
                self.processes,
                self.services
            );
        }
        if self.failed == 0 {
            format!(
                "{}: завершено процессов — {}, служб — {}",
                self.target.display_name(),
                self.processes,
                self.services
            )
        } else {
            format!(
                "{}: процессов — {}, служб — {}, ошибок — {}",
                self.target.display_name(),
                self.processes,
                self.services,
                self.failed
            )
        }
    }

    fn details(&self) -> String {
        if self.lines.is_empty() {
            format!(
                "Процессы и службы {} не найдены.",
                self.target.display_name()
            )
        } else {
            self.lines.join("\r\n")
        }
    }
}

trait SystemOps {
    fn enumerate_processes(&self) -> Result<Vec<ProcessInfo>, String>;
    fn enumerate_services(&self) -> Result<Vec<ServiceInfo>, String>;
    fn stop_services(&self) -> OperationResult;
    fn terminate_processes(&self, target: AppTarget) -> OperationResult;
}

struct WindowsSystemOps;

impl SystemOps for WindowsSystemOps {
    fn enumerate_processes(&self) -> Result<Vec<ProcessInfo>, String> {
        enumerate_processes()
    }

    fn enumerate_services(&self) -> Result<Vec<ServiceInfo>, String> {
        enumerate_amnezia_services()
    }

    fn stop_services(&self) -> OperationResult {
        stop_amnezia_services()
    }

    fn terminate_processes(&self, target: AppTarget) -> OperationResult {
        kill_processes(target)
    }
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

unsafe fn wide_ptr_to_string(pointer: *const u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let mut length = 0;
    unsafe {
        while *pointer.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length))
    }
}

fn usage_error() -> String {
    "Использование: Kill-Codex.exe --dry-run [codex|amnezia|discord|chrome]".to_string()
}

fn parse_args<I, S>(args: I) -> Result<RunMode, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<String> = args
        .into_iter()
        .skip(1)
        .map(|value| value.as_ref().to_string_lossy().into_owned())
        .collect();
    match args.as_slice() {
        [] => Ok(RunMode::Gui),
        [flag] if flag.eq_ignore_ascii_case("--dry-run") => Ok(RunMode::DryRun(AppTarget::Codex)),
        [flag, target] if flag.eq_ignore_ascii_case("--dry-run") => AppTarget::from_name(target)
            .map(RunMode::DryRun)
            .ok_or_else(usage_error),
        _ => Err(usage_error()),
    }
}

fn enumerate_processes() -> Result<Vec<ProcessInfo>, String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let snapshot = OwnedHandle(snapshot);
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut result = Vec::new();

        if Process32FirstW(snapshot.0, &mut entry) == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        loop {
            let length = entry
                .szExeFile
                .iter()
                .position(|&character| character == 0)
                .unwrap_or(entry.szExeFile.len());
            result.push(ProcessInfo {
                pid: entry.th32ProcessID,
                parent_pid: entry.th32ParentProcessID,
                name: String::from_utf16_lossy(&entry.szExeFile[..length]),
            });
            if Process32NextW(snapshot.0, &mut entry) == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                    return Err(error.to_string());
                }
                break;
            }
        }
        Ok(result)
    }
}

fn target_pids(processes: &[ProcessInfo], target: AppTarget, own_pid: u32) -> HashSet<u32> {
    let mut targets: HashSet<u32> = processes
        .iter()
        .filter(|process| process.pid != own_pid && target.matches_process(&process.name))
        .map(|process| process.pid)
        .collect();

    loop {
        let before = targets.len();
        for process in processes {
            if process.pid != own_pid && targets.contains(&process.parent_pid) {
                targets.insert(process.pid);
            }
        }
        if targets.len() == before {
            return targets;
        }
    }
}

fn validated_target_pids(
    target: AppTarget,
    discovery: &[ProcessInfo],
    current: &[ProcessInfo],
    pinned_times: &HashMap<u32, u64>,
    own_pid: u32,
) -> HashSet<u32> {
    let candidates = target_pids(discovery, target, own_pid);
    let mut valid: HashSet<u32> = current
        .iter()
        .filter(|process| {
            process.pid != own_pid
                && candidates.contains(&process.pid)
                && pinned_times.contains_key(&process.pid)
                && target.matches_process(&process.name)
        })
        .map(|process| process.pid)
        .collect();

    loop {
        let before = valid.len();
        for process in current {
            if process.pid == own_pid
                || !candidates.contains(&process.pid)
                || !valid.contains(&process.parent_pid)
            {
                continue;
            }
            if let (Some(child_time), Some(parent_time)) = (
                pinned_times.get(&process.pid),
                pinned_times.get(&process.parent_pid),
            ) && child_time >= parent_time
            {
                valid.insert(process.pid);
            }
        }
        if valid.len() == before {
            return valid;
        }
    }
}

fn process_depth(process: &ProcessInfo, parents: &HashMap<u32, u32>) -> usize {
    let mut depth = 0;
    let mut parent = process.parent_pid;
    let mut seen = HashSet::new();
    while let Some(next_parent) = parents.get(&parent) {
        if !seen.insert(parent) {
            break;
        }
        depth += 1;
        parent = *next_parent;
    }
    depth
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct PinnedProcess {
    info: ProcessInfo,
    creation_time: u64,
    handle: OwnedHandle,
}

fn process_creation_time(handle: HANDLE) -> Result<u64, String> {
    unsafe {
        let mut creation: FILETIME = zeroed();
        let mut exit: FILETIME = zeroed();
        let mut kernel: FILETIME = zeroed();
        let mut user: FILETIME = zeroed();
        if GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

fn pin_process(process: &ProcessInfo) -> Result<PinnedProcess, String> {
    unsafe {
        let access = PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
        let handle = OpenProcess(access, 0, process.pid);
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let handle = OwnedHandle(handle);
        let creation_time = process_creation_time(handle.0)?;
        Ok(PinnedProcess {
            info: process.clone(),
            creation_time,
            handle,
        })
    }
}

fn kill_processes(target: AppTarget) -> OperationResult {
    let mut result = OperationResult::default();
    let discovery = match enumerate_processes() {
        Ok(processes) => processes,
        Err(error) => {
            result.failed += 1;
            result.lines.push(format!("✗ Список процессов: {error}"));
            return result;
        }
    };
    let own_pid = unsafe { GetCurrentProcessId() };
    let candidates = target_pids(&discovery, target, own_pid);
    let discovery_by_pid: HashMap<u32, &ProcessInfo> = discovery
        .iter()
        .map(|process| (process.pid, process))
        .collect();
    let mut pinned = Vec::new();

    for pid in candidates {
        let process = discovery_by_pid[&pid];
        match pin_process(process) {
            Ok(item) => pinned.push(item),
            Err(error) => {
                result.failed += 1;
                result.lines.push(format!(
                    "✗ {} (PID {}): не удалось закрепить процесс: {error}",
                    process.name, process.pid
                ));
            }
        }
    }

    if pinned.is_empty() {
        return result;
    }

    let current = match enumerate_processes() {
        Ok(processes) => processes,
        Err(error) => {
            result.failed += 1;
            result
                .lines
                .push(format!("✗ Повторная проверка процессов: {error}"));
            return result;
        }
    };
    let pinned_times: HashMap<u32, u64> = pinned
        .iter()
        .map(|process| (process.info.pid, process.creation_time))
        .collect();
    let valid = validated_target_pids(target, &discovery, &current, &pinned_times, own_pid);
    for process in &pinned {
        if !valid.contains(&process.info.pid) {
            result.failed += 1;
            result.lines.push(format!(
                "✗ {} (PID {}): процесс изменился до проверки и пропущен",
                process.info.name, process.info.pid
            ));
        }
    }
    pinned.retain(|process| valid.contains(&process.info.pid));

    let parents: HashMap<u32, u32> = current
        .iter()
        .map(|process| (process.pid, process.parent_pid))
        .collect();
    pinned.sort_by_key(|process| {
        let info = current
            .iter()
            .find(|current| current.pid == process.info.pid)
            .unwrap_or(&process.info);
        std::cmp::Reverse(process_depth(info, &parents))
    });

    let mut pending = Vec::new();
    for process in pinned {
        unsafe {
            if WaitForSingleObject(process.handle.0, 0) == WAIT_OBJECT_0 {
                result.succeeded += 1;
                result.lines.push(format!(
                    "✓ {} (PID {}): уже завершён",
                    process.info.name, process.info.pid
                ));
            } else if TerminateProcess(process.handle.0, 1) != 0 {
                pending.push(process);
            } else {
                result.failed += 1;
                result.lines.push(format!(
                    "✗ {} (PID {}): {}",
                    process.info.name,
                    process.info.pid,
                    std::io::Error::last_os_error()
                ));
            }
        }
    }

    let deadline = Instant::now() + PROCESS_WAIT;
    for process in pending {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = remaining.as_millis().min(u32::MAX as u128) as u32;
        let wait = unsafe { WaitForSingleObject(process.handle.0, timeout) };
        if wait == WAIT_OBJECT_0 {
            result.succeeded += 1;
            result.lines.push(format!(
                "✓ {} (PID {})",
                process.info.name, process.info.pid
            ));
        } else if wait == WAIT_TIMEOUT {
            result.failed += 1;
            result.lines.push(format!(
                "✗ {} (PID {}): запрос принят, завершение не подтверждено за 3 с",
                process.info.name, process.info.pid
            ));
        } else {
            result.failed += 1;
            result.lines.push(format!(
                "✗ {} (PID {}): ошибка ожидания: {}",
                process.info.name,
                process.info.pid,
                std::io::Error::last_os_error()
            ));
        }
    }
    result
}

fn is_amnezia_service(name: &str) -> bool {
    name.eq_ignore_ascii_case("AmneziaVPN-service")
        || name
            .get(.."AmneziaWGTunnel$".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("AmneziaWGTunnel$"))
}

struct OwnedServiceHandle(windows_sys::Win32::System::Services::SC_HANDLE);

impl Drop for OwnedServiceHandle {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}

fn enumerate_amnezia_services() -> Result<Vec<ServiceInfo>, String> {
    unsafe {
        let manager = OpenSCManagerW(null(), null(), SC_MANAGER_ENUMERATE_SERVICE);
        if manager.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let manager = OwnedServiceHandle(manager);
        let mut services = Vec::new();
        let mut resume = 0;
        let mut buffer_size = 64 * 1024_usize;

        loop {
            let mut buffer = vec![0_u8; buffer_size];
            let mut bytes_needed = 0;
            let mut count = 0;
            let ok = EnumServicesStatusExW(
                manager.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut bytes_needed,
                &mut count,
                &mut resume,
                null(),
            );
            let error = if ok == 0 {
                Some(std::io::Error::last_os_error())
            } else {
                None
            };
            let entries = std::slice::from_raw_parts(
                buffer.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW,
                count as usize,
            );
            services.extend(entries.iter().filter_map(|entry| {
                let service = ServiceInfo {
                    name: wide_ptr_to_string(entry.lpServiceName),
                    state: entry.ServiceStatusProcess.dwCurrentState,
                };
                is_amnezia_service(&service.name).then_some(service)
            }));

            if ok != 0 {
                return Ok(services);
            }
            let error = error.expect("failed service enumeration must have an OS error");
            if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                return Err(error.to_string());
            }
            if count == 0 {
                if bytes_needed as usize <= buffer_size {
                    return Err("Перечисление служб не продвинулось".to_string());
                }
                buffer_size = bytes_needed as usize;
            }
        }
    }
}

struct ActiveService {
    name: String,
    handle: OwnedServiceHandle,
    stopped: bool,
    failed: bool,
}

fn query_service_state(
    handle: windows_sys::Win32::System::Services::SC_HANDLE,
) -> Result<u32, String> {
    unsafe {
        let mut status: SERVICE_STATUS_PROCESS = zeroed();
        let mut bytes_needed = 0;
        if QueryServiceStatusEx(
            handle,
            SC_STATUS_PROCESS_INFO,
            &mut status as *mut _ as *mut u8,
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut bytes_needed,
        ) == 0
        {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(status.dwCurrentState)
        }
    }
}

fn stop_amnezia_services() -> OperationResult {
    let mut result = OperationResult::default();
    let mut services = match enumerate_amnezia_services() {
        Ok(services) => services,
        Err(error) => {
            result.failed += 1;
            result.lines.push(format!("✗ Службы Amnezia: {error}"));
            return result;
        }
    };
    services.sort_by_key(|service| {
        if service
            .name
            .get(.."AmneziaWGTunnel$".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("AmneziaWGTunnel$"))
        {
            0
        } else {
            1
        }
    });

    let manager = unsafe {
        OpenSCManagerW(
            null(),
            null(),
            SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE,
        )
    };
    if manager.is_null() {
        result.failed += 1;
        result.lines.push(format!(
            "✗ Диспетчер служб: {}",
            std::io::Error::last_os_error()
        ));
        return result;
    }
    let manager = OwnedServiceHandle(manager);
    let mut active = Vec::new();

    for service in services {
        if service.state == SERVICE_STOPPED {
            result
                .lines
                .push(format!("• Служба {} уже остановлена", service.name));
            continue;
        }
        let name = wide(&service.name);
        let handle = unsafe {
            OpenServiceW(
                manager.0,
                name.as_ptr(),
                SERVICE_STOP | SERVICE_QUERY_STATUS,
            )
        };
        if handle.is_null() {
            result.failed += 1;
            result.lines.push(format!(
                "✗ Служба {}: {}",
                service.name,
                std::io::Error::last_os_error()
            ));
            continue;
        }
        active.push(ActiveService {
            name: service.name,
            handle: OwnedServiceHandle(handle),
            stopped: false,
            failed: false,
        });
    }

    let deadline = Instant::now() + SERVICE_WAIT;
    for service in &mut active {
        match query_service_state(service.handle.0) {
            Ok(SERVICE_STOPPED) => service.stopped = true,
            Ok(SERVICE_STOP_PENDING) => {}
            Ok(_) => unsafe {
                let mut status: SERVICE_STATUS = zeroed();
                if ControlService(service.handle.0, SERVICE_CONTROL_STOP, &mut status) == 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(ERROR_SERVICE_NOT_ACTIVE as i32) {
                        service.stopped = true;
                    } else {
                        service.failed = true;
                        result.failed += 1;
                        result
                            .lines
                            .push(format!("✗ Служба {}: {error}", service.name));
                    }
                }
            },
            Err(error) => {
                service.failed = true;
                result.failed += 1;
                result
                    .lines
                    .push(format!("✗ Служба {}: {error}", service.name));
            }
        }
    }

    while Instant::now() < deadline
        && active
            .iter()
            .any(|service| !service.stopped && !service.failed)
    {
        for service in active
            .iter_mut()
            .filter(|service| !service.stopped && !service.failed)
        {
            match query_service_state(service.handle.0) {
                Ok(SERVICE_STOPPED) => service.stopped = true,
                Ok(_) => {}
                Err(error) => {
                    service.failed = true;
                    result.failed += 1;
                    result
                        .lines
                        .push(format!("✗ Служба {}: {error}", service.name));
                }
            }
        }
        if active
            .iter()
            .any(|service| !service.stopped && !service.failed)
        {
            thread::sleep(Duration::from_millis(100));
        }
    }

    for service in active {
        if service.stopped {
            result.succeeded += 1;
            result.lines.push(format!("✓ Служба {}", service.name));
        } else if !service.failed {
            result.failed += 1;
            result.lines.push(format!(
                "✗ Служба {}: остановка не подтверждена за 5 с",
                service.name
            ));
        }
    }
    result
}

fn inspect_target<O: SystemOps>(target: AppTarget, ops: &O) -> ActionReport {
    let mut lines = Vec::new();
    let mut failures = 0;
    let services = if target == AppTarget::Amnezia {
        match ops.enumerate_services() {
            Ok(services) => {
                for service in &services {
                    let state = if service.state == SERVICE_STOPPED {
                        "остановлена"
                    } else {
                        "активна"
                    };
                    lines.push(format!("• Служба {} ({state})", service.name));
                }
                services.len()
            }
            Err(error) => {
                failures += 1;
                lines.push(format!("✗ Службы Amnezia: {error}"));
                0
            }
        }
    } else {
        0
    };

    let processes = match ops.enumerate_processes() {
        Ok(processes) => {
            let ids = target_pids(&processes, target, unsafe { GetCurrentProcessId() });
            for process in processes
                .iter()
                .filter(|process| ids.contains(&process.pid))
            {
                lines.push(format!("• {} (PID {})", process.name, process.pid));
            }
            ids.len()
        }
        Err(error) => {
            failures += 1;
            lines.push(format!("✗ Список процессов: {error}"));
            0
        }
    };

    ActionReport {
        target,
        lines,
        processes,
        services,
        failed: failures,
        dry_run: true,
    }
}

fn execute_target<O: SystemOps>(target: AppTarget, ops: &O) -> ActionReport {
    let services = if target == AppTarget::Amnezia {
        ops.stop_services()
    } else {
        OperationResult::default()
    };
    let processes = ops.terminate_processes(target);
    let mut lines = services.lines;
    lines.extend(processes.lines);
    ActionReport {
        target,
        lines,
        processes: processes.succeeded,
        services: services.succeeded,
        failed: processes.failed + services.failed,
        dry_run: false,
    }
}

fn run_action<O: SystemOps>(target: AppTarget, mode: ExecutionMode, ops: &O) -> ActionReport {
    match mode {
        ExecutionMode::Execute => execute_target(target, ops),
        ExecutionMode::DryRun => inspect_target(target, ops),
    }
}

fn is_elevated() -> bool {
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let token = OwnedHandle(token);
        let mut elevation: TOKEN_ELEVATION = zeroed();
        let mut returned = 0;
        GetTokenInformation(
            token.0,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        ) != 0
            && elevation.TokenIsElevated != 0
    }
}

fn relaunch_elevated() -> bool {
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return false,
    };
    let executable = wide(&executable.to_string_lossy());
    let runas = wide("runas");
    unsafe {
        ShellExecuteW(
            null_mut(),
            runas.as_ptr(),
            executable.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        ) as isize
            > 32
    }
}

fn report_mailbox() -> &'static Mutex<Option<ActionReport>> {
    REPORT_MAILBOX.get_or_init(|| Mutex::new(None))
}

fn ui_state() -> &'static Mutex<UiState> {
    UI_STATE.get_or_init(|| Mutex::new(UiState::default()))
}

fn lock_unpoisoned<T>(mutex: &'static Mutex<T>) -> MutexGuard<'static, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

unsafe fn set_text(hwnd: HWND, value: &str) {
    unsafe {
        SetWindowTextW(hwnd, wide(value).as_ptr());
    }
}

unsafe fn enable_all_buttons(enabled: bool) {
    for button in &BUTTONS {
        let hwnd = button.load(Ordering::Relaxed) as HWND;
        if !hwnd.is_null() {
            unsafe {
                EnableWindow(hwnd, enabled as i32);
            }
        }
    }
}

fn start_action(hwnd: HWND, target: AppTarget) {
    if !lock_unpoisoned(ui_state()).begin(target) {
        return;
    }
    unsafe {
        enable_all_buttons(false);
        set_text(
            STATUS.load(Ordering::Relaxed) as HWND,
            &format!("{}: выполняется…", target.display_name()),
        );
        set_text(
            DETAILS.load(Ordering::Relaxed) as HWND,
            "Завершение процессов. Пожалуйста, подождите.",
        );
    }
    let window = hwnd as isize;
    thread::spawn(move || {
        let report = run_action(target, ExecutionMode::Execute, &WindowsSystemOps);
        if !WINDOW_ALIVE.load(Ordering::Acquire) {
            lock_unpoisoned(report_mailbox()).take();
            return;
        }
        *lock_unpoisoned(report_mailbox()) = Some(report);
        if !WINDOW_ALIVE.load(Ordering::Acquire)
            || unsafe { PostMessageW(window as HWND, WM_ACTION_COMPLETE, 0, 0) } == 0
        {
            lock_unpoisoned(report_mailbox()).take();
        }
    });
}

fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    red as u32 | ((green as u32) << 8) | ((blue as u32) << 16)
}

unsafe fn filled_ellipse(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    color: u32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let old_brush = SelectObject(hdc, brush);
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        Ellipse(hdc, left, top, right, bottom);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        DeleteObject(brush);
    }
}

unsafe fn filled_polygon(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    color: u32,
    points: &[POINT],
) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let old_brush = SelectObject(hdc, brush);
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        Polygon(hdc, points.as_ptr(), points.len() as i32);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        DeleteObject(brush);
    }
}

unsafe fn filled_round_rect(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    color: u32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    radius: i32,
) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let old_brush = SelectObject(hdc, brush);
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        RoundRect(hdc, left, top, right, bottom, radius, radius);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        DeleteObject(brush);
    }
}

unsafe fn filled_pie(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    color: u32,
    box_rect: (i32, i32, i32, i32),
    start: (i32, i32),
    end: (i32, i32),
) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let old_brush = SelectObject(hdc, brush);
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        Pie(
            hdc, box_rect.0, box_rect.1, box_rect.2, box_rect.3, start.0, start.1, end.0, end.1,
        );
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        DeleteObject(brush);
    }
}

unsafe fn draw_logo(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    target: AppTarget,
    left: i32,
    top: i32,
) {
    let white = rgb(255, 255, 255);
    unsafe {
        match target {
            AppTarget::Codex => {
                for (dx, dy) in [(8, 0), (18, 5), (18, 17), (8, 22), (-2, 17), (-2, 5)] {
                    filled_ellipse(
                        hdc,
                        white,
                        left + dx,
                        top + dy,
                        left + dx + 12,
                        top + dy + 12,
                    );
                }
                filled_ellipse(
                    hdc,
                    rgb(32, 33, 35),
                    left + 9,
                    top + 10,
                    left + 21,
                    top + 22,
                );
            }
            AppTarget::Amnezia => {
                filled_polygon(
                    hdc,
                    white,
                    &[
                        POINT {
                            x: left + 15,
                            y: top,
                        },
                        POINT {
                            x: left + 31,
                            y: top + 30,
                        },
                        POINT {
                            x: left + 22,
                            y: top + 30,
                        },
                        POINT {
                            x: left + 18,
                            y: top + 21,
                        },
                        POINT {
                            x: left + 10,
                            y: top + 21,
                        },
                        POINT {
                            x: left + 6,
                            y: top + 30,
                        },
                        POINT {
                            x: left - 1,
                            y: top + 30,
                        },
                    ],
                );
                filled_polygon(
                    hdc,
                    rgb(102, 66, 224),
                    &[
                        POINT {
                            x: left + 14,
                            y: top + 10,
                        },
                        POINT {
                            x: left + 18,
                            y: top + 18,
                        },
                        POINT {
                            x: left + 11,
                            y: top + 18,
                        },
                    ],
                );
            }
            AppTarget::Discord => {
                filled_round_rect(hdc, white, left, top + 4, left + 32, top + 27, 12);
                filled_ellipse(
                    hdc,
                    rgb(88, 101, 242),
                    left + 8,
                    top + 12,
                    left + 13,
                    top + 17,
                );
                filled_ellipse(
                    hdc,
                    rgb(88, 101, 242),
                    left + 20,
                    top + 12,
                    left + 25,
                    top + 17,
                );
                filled_round_rect(
                    hdc,
                    rgb(88, 101, 242),
                    left + 11,
                    top + 20,
                    left + 22,
                    top + 23,
                    2,
                );
            }
            AppTarget::Chrome => {
                let rect = (left, top, left + 32, top + 32);
                let center = (left + 16, top + 16);
                filled_pie(
                    hdc,
                    rgb(234, 67, 53),
                    rect,
                    (left + 16, top),
                    (left + 32, top + 23),
                );
                filled_pie(
                    hdc,
                    rgb(52, 168, 83),
                    rect,
                    (left + 32, top + 23),
                    (left, top + 23),
                );
                filled_pie(
                    hdc,
                    rgb(251, 188, 4),
                    rect,
                    (left, top + 23),
                    (left + 16, top),
                );
                filled_ellipse(
                    hdc,
                    white,
                    center.0 - 8,
                    center.1 - 8,
                    center.0 + 8,
                    center.1 + 8,
                );
                filled_ellipse(
                    hdc,
                    rgb(66, 133, 244),
                    center.0 - 6,
                    center.1 - 6,
                    center.0 + 6,
                    center.1 + 6,
                );
            }
        }
    }
}

unsafe fn draw_button(draw: &DRAWITEMSTRUCT, target: AppTarget) {
    unsafe {
        let selected = draw.itemState & ODS_SELECTED != 0;
        let disabled = draw.itemState & ODS_DISABLED != 0;
        let brush = CreateSolidBrush(target.colors(selected, disabled));
        FillRect(draw.hDC, &draw.rcItem, brush);
        DeleteObject(brush);

        let icon_top = draw.rcItem.top + (draw.rcItem.bottom - draw.rcItem.top - 32) / 2;
        draw_logo(draw.hDC, target, draw.rcItem.left + 18, icon_top);
        SetBkMode(draw.hDC, TRANSPARENT as i32);
        SetTextColor(draw.hDC, rgb(255, 255, 255));
        let mut text_rect = draw.rcItem;
        text_rect.left += 62;
        text_rect.right -= 12;
        DrawTextW(
            draw.hDC,
            wide(target.button_label()).as_ptr(),
            -1,
            &mut text_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => {
            unsafe {
                WINDOW_ALIVE.store(true, Ordering::Release);
                let instance = GetModuleHandleW(null());
                let font = GetStockObject(DEFAULT_GUI_FONT);
                let title = CreateWindowExW(
                    0,
                    wide("STATIC").as_ptr(),
                    wide("KILL APPS").as_ptr(),
                    WS_CHILD | WS_VISIBLE,
                    30,
                    20,
                    556,
                    34,
                    hwnd,
                    null_mut(),
                    instance,
                    null(),
                );
                let subtitle = CreateWindowExW(
                    0,
                    wide("STATIC").as_ptr(),
                    wide("Codex + DeepSeek Harness · Amnezia · Discord · Chrome").as_ptr(),
                    WS_CHILD | WS_VISIBLE,
                    30,
                    56,
                    556,
                    25,
                    hwnd,
                    null_mut(),
                    instance,
                    null(),
                );
                let positions = [(30, 92), (316, 92), (30, 168), (316, 168)];
                for (target, (x, y)) in AppTarget::ALL.into_iter().zip(positions) {
                    let button = CreateWindowExW(
                        0,
                        wide("BUTTON").as_ptr(),
                        wide(target.button_label()).as_ptr(),
                        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
                        x,
                        y,
                        270,
                        64,
                        hwnd,
                        target.button_id() as *mut _,
                        instance,
                        null(),
                    );
                    BUTTONS[target.index()].store(button as isize, Ordering::Relaxed);
                    SendMessageW(button, WM_SETFONT, font as usize, 1);
                }
                let status = CreateWindowExW(
                    0,
                    wide("STATIC").as_ptr(),
                    wide("Готово к запуску").as_ptr(),
                    WS_CHILD | WS_VISIBLE,
                    30,
                    252,
                    556,
                    24,
                    hwnd,
                    null_mut(),
                    instance,
                    null(),
                );
                let details = CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    wide("EDIT").as_ptr(),
                    wide("После нажатия здесь появится результат.").as_ptr(),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_VSCROLL
                        | ES_MULTILINE as u32
                        | ES_AUTOVSCROLL as u32
                        | ES_READONLY as u32,
                    30,
                    282,
                    556,
                    118,
                    hwnd,
                    null_mut(),
                    instance,
                    null(),
                );
                for control in [title, subtitle, status, details] {
                    SendMessageW(control, WM_SETFONT, font as usize, 1);
                }
                STATUS.store(status as isize, Ordering::Relaxed);
                DETAILS.store(details as isize, Ordering::Relaxed);
            }
            0
        }
        WM_COMMAND => {
            if let Some(target) = AppTarget::from_button_id(wparam & 0xffff) {
                start_action(hwnd, target);
                0
            } else {
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
        WM_ACTION_COMPLETE => {
            let report = lock_unpoisoned(report_mailbox()).take();
            let completed = lock_unpoisoned(ui_state()).complete_if_report(report.is_some());
            if completed && let Some(report) = report {
                unsafe {
                    set_text(STATUS.load(Ordering::Relaxed) as HWND, &report.status());
                    set_text(DETAILS.load(Ordering::Relaxed) as HWND, &report.details());
                    enable_all_buttons(true);
                }
            }
            0
        }
        WM_DRAWITEM => {
            let draw = unsafe { &*(lparam as *const DRAWITEMSTRUCT) };
            if let Some(target) = AppTarget::from_button_id(draw.CtlID as usize) {
                unsafe { draw_button(draw, target) };
                1
            } else {
                0
            }
        }
        WM_DESTROY => {
            WINDOW_ALIVE.store(false, Ordering::Release);
            lock_unpoisoned(report_mailbox()).take();
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn show_report(report: &ActionReport, caption: &str) {
    let message = format!("{}\n\n{}", report.status(), report.details());
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(&message).as_ptr(),
            wide(caption).as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn run_gui() {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide("KillAppsWindow");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as *mut _,
            lpszClassName: class_name.as_ptr(),
            ..zeroed()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("KILL APPS").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            634,
            456,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        let mut message: MSG = zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn show_usage(error: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(error).as_ptr(),
            wide("Kill Apps — неверные параметры").as_ptr(),
            MB_OK | MB_ICONWARNING,
        );
    }
}

fn main() {
    #[cfg(debug_assertions)]
    if std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem().map(|stem| stem.to_owned()))
        .is_some_and(|stem| stem.eq_ignore_ascii_case("kill-apps-preview"))
    {
        run_gui();
        return;
    }

    let mode = match parse_args(std::env::args_os()) {
        Ok(mode) => mode,
        Err(error) => {
            show_usage(&error);
            std::process::exit(2);
        }
    };
    if let RunMode::DryRun(target) = mode {
        let report = run_action(target, ExecutionMode::DryRun, &WindowsSystemOps);
        show_report(&report, "Kill Apps — безопасная проверка");
        return;
    }
    if !is_elevated() {
        if !relaunch_elevated() {
            unsafe {
                MessageBoxW(
                    null_mut(),
                    wide("Для завершения всех процессов нужны права администратора.").as_ptr(),
                    wide("Kill Apps").as_ptr(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
        return;
    }
    run_gui();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn matches_each_target_without_cross_matching() {
        assert!(AppTarget::Codex.matches_process("ChatGPT.exe"));
        assert!(AppTarget::Codex.matches_process("codex-code-mode-host.exe"));
        assert!(AppTarget::Codex.matches_process("DeepSeekDesktop.exe"));
        assert!(AppTarget::Amnezia.matches_process("AmneziaVPN-service.exe"));
        assert!(AppTarget::Amnezia.matches_process("AmneziaWG.exe"));
        assert!(AppTarget::Discord.matches_process("Discord.exe"));
        assert!(AppTarget::Chrome.matches_process("chrome.exe"));
        assert!(!AppTarget::Chrome.matches_process("chromedriver.exe"));
        assert!(!AppTarget::Discord.matches_process("Update.exe"));
        assert!(!AppTarget::Codex.matches_process("Kill-Codex.exe"));
        assert!(!AppTarget::Codex.matches_process("DeepSeek.exe"));
    }

    #[test]
    fn parses_dry_run_grammar_strictly() {
        assert_eq!(
            parse_args(["app.exe", "--dry-run"]),
            Ok(RunMode::DryRun(AppTarget::Codex))
        );
        assert_eq!(
            parse_args(["app.exe", "--dry-run", "chrome"]),
            Ok(RunMode::DryRun(AppTarget::Chrome))
        );
        assert_eq!(parse_args(["app.exe"]), Ok(RunMode::Gui));
        assert!(parse_args(["app.exe", "--dry-run", "unknown"]).is_err());
        assert!(parse_args(["app.exe", "--dry-run", "chrome", "extra"]).is_err());
    }

    #[test]
    fn includes_descendants_but_never_self() {
        let processes = vec![
            ProcessInfo {
                pid: 10,
                parent_pid: 1,
                name: "codex.exe".into(),
            },
            ProcessInfo {
                pid: 11,
                parent_pid: 10,
                name: "conhost.exe".into(),
            },
            ProcessInfo {
                pid: 12,
                parent_pid: 11,
                name: "helper.exe".into(),
            },
            ProcessInfo {
                pid: 20,
                parent_pid: 1,
                name: "explorer.exe".into(),
            },
        ];
        assert_eq!(
            target_pids(&processes, AppTarget::Codex, 12),
            HashSet::from([10, 11])
        );
        assert_eq!(
            target_pids(&processes, AppTarget::Codex, 99),
            HashSet::from([10, 11, 12])
        );
    }

    #[test]
    fn codex_target_includes_deepseek_harness_webview_only() {
        let processes = vec![
            ProcessInfo {
                pid: 30,
                parent_pid: 1,
                name: "DeepSeekDesktop.exe".into(),
            },
            ProcessInfo {
                pid: 31,
                parent_pid: 30,
                name: "msedgewebview2.exe".into(),
            },
            ProcessInfo {
                pid: 40,
                parent_pid: 1,
                name: "msedgewebview2.exe".into(),
            },
        ];
        assert_eq!(
            target_pids(&processes, AppTarget::Codex, 99),
            HashSet::from([30, 31])
        );
    }

    #[test]
    fn validates_selected_graph_against_second_snapshot() {
        let first = vec![
            ProcessInfo {
                pid: 10,
                parent_pid: 1,
                name: "codex.exe".into(),
            },
            ProcessInfo {
                pid: 11,
                parent_pid: 10,
                name: "conhost.exe".into(),
            },
            ProcessInfo {
                pid: 12,
                parent_pid: 11,
                name: "chrome.exe".into(),
            },
        ];
        let pinned = HashMap::from([(10, 100_u64), (11, 110), (12, 120)]);
        assert_eq!(
            validated_target_pids(AppTarget::Codex, &first, &first, &pinned, 99),
            HashSet::from([10, 11, 12])
        );
    }

    #[test]
    fn rejects_stale_or_impossible_parent_edges() {
        let first = vec![
            ProcessInfo {
                pid: 10,
                parent_pid: 1,
                name: "codex.exe".into(),
            },
            ProcessInfo {
                pid: 11,
                parent_pid: 10,
                name: "conhost.exe".into(),
            },
            ProcessInfo {
                pid: 12,
                parent_pid: 11,
                name: "helper.exe".into(),
            },
        ];
        let mut second = first.clone();
        second.retain(|process| process.pid != 11);
        let pinned = HashMap::from([(10, 100_u64), (11, 110), (12, 90)]);
        assert_eq!(
            validated_target_pids(AppTarget::Codex, &first, &second, &pinned, 99),
            HashSet::from([10])
        );
    }

    #[test]
    fn matches_only_amnezia_services() {
        assert!(is_amnezia_service("AmneziaVPN-service"));
        assert!(is_amnezia_service("amneziawgtunnel$AmneziaVPN"));
        assert!(!is_amnezia_service("WireGuardTunnel$other"));
        assert!(!is_amnezia_service("MyAmneziaHelper"));
    }

    struct FakeSystemOps {
        process_enumerations: Cell<usize>,
        service_enumerations: Cell<usize>,
        stop_service_calls: Cell<usize>,
        terminate_process_calls: Cell<usize>,
    }

    impl SystemOps for FakeSystemOps {
        fn enumerate_processes(&self) -> Result<Vec<ProcessInfo>, String> {
            self.process_enumerations
                .set(self.process_enumerations.get() + 1);
            Ok(Vec::new())
        }

        fn enumerate_services(&self) -> Result<Vec<ServiceInfo>, String> {
            self.service_enumerations
                .set(self.service_enumerations.get() + 1);
            Ok(Vec::new())
        }

        fn stop_services(&self) -> OperationResult {
            self.stop_service_calls
                .set(self.stop_service_calls.get() + 1);
            OperationResult::default()
        }

        fn terminate_processes(&self, _target: AppTarget) -> OperationResult {
            self.terminate_process_calls
                .set(self.terminate_process_calls.get() + 1);
            OperationResult::default()
        }
    }

    #[test]
    fn dry_run_never_mutates_system() {
        let fake = FakeSystemOps {
            process_enumerations: Cell::new(0),
            service_enumerations: Cell::new(0),
            stop_service_calls: Cell::new(0),
            terminate_process_calls: Cell::new(0),
        };
        let report = run_action(AppTarget::Amnezia, ExecutionMode::DryRun, &fake);
        assert!(report.dry_run);
        assert_eq!(fake.process_enumerations.get(), 1);
        assert_eq!(fake.service_enumerations.get(), 1);
        assert_eq!(fake.stop_service_calls.get(), 0);
        assert_eq!(fake.terminate_process_calls.get(), 0);
    }

    #[test]
    fn live_backend_can_inspect_every_target_without_execution() {
        for target in AppTarget::ALL {
            let report = run_action(target, ExecutionMode::DryRun, &WindowsSystemOps);
            assert_eq!(report.target, target);
            assert!(report.dry_run);
        }
    }

    #[test]
    fn dispatch_blocks_concurrent_actions_until_completion() {
        let mut state = UiState::default();
        assert!(state.begin(AppTarget::Chrome));
        assert!(!state.begin(AppTarget::Discord));
        assert!(!state.complete_if_report(false));
        assert!(!state.begin(AppTarget::Discord));
        assert!(state.complete_if_report(true));
        assert!(state.begin(AppTarget::Discord));
    }

    #[test]
    fn report_formats_empty_success_and_mixed_results() {
        let empty = ActionReport {
            target: AppTarget::Chrome,
            lines: Vec::new(),
            processes: 0,
            services: 0,
            failed: 0,
            dry_run: false,
        };
        assert_eq!(empty.details(), "Процессы и службы Chrome не найдены.");
        assert!(empty.status().contains("процессов — 0"));

        let success = ActionReport {
            target: AppTarget::Amnezia,
            lines: vec!["✓ Служба AmneziaVPN-service".into()],
            processes: 2,
            services: 1,
            failed: 0,
            dry_run: false,
        };
        assert!(success.status().contains("процессов — 2, служб — 1"));
        assert_eq!(success.details(), "✓ Служба AmneziaVPN-service");

        let mixed = ActionReport {
            target: AppTarget::Discord,
            lines: vec!["✓ Discord.exe".into(), "✗ Discord.exe: denied".into()],
            processes: 1,
            services: 0,
            failed: 1,
            dry_run: false,
        };
        assert!(mixed.status().contains("ошибок — 1"));
        assert!(mixed.details().contains("\r\n"));
    }

    #[test]
    fn every_button_maps_to_exactly_one_target() {
        assert_eq!(
            AppTarget::from_button_id(ID_KILL_CODEX),
            Some(AppTarget::Codex)
        );
        assert_eq!(
            AppTarget::from_button_id(ID_KILL_AMNEZIA),
            Some(AppTarget::Amnezia)
        );
        assert_eq!(
            AppTarget::from_button_id(ID_KILL_DISCORD),
            Some(AppTarget::Discord)
        );
        assert_eq!(
            AppTarget::from_button_id(ID_KILL_CHROME),
            Some(AppTarget::Chrome)
        );
        assert_eq!(AppTarget::from_button_id(9999), None);
    }
}
