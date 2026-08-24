#![windows_subsystem = "windows"]

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateSolidBrush, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, FillRect, GetStockObject, SetBkMode, SetTextColor, TRANSPARENT,
    UpdateWindow,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_TERMINATE,
    TerminateProcess,
};
use windows_sys::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_SELECTED};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const ID_KILL: usize = 1001;
static STATUS: AtomicIsize = AtomicIsize::new(0);
static DETAILS: AtomicIsize = AtomicIsize::new(0);
static BUTTON: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Debug)]
struct ProcessInfo {
    pid: u32,
    parent_pid: u32,
    name: String,
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn is_target(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "chatgpt.exe"
        || name == "chatgpt"
        || name == "codex.exe"
        || name == "codex"
        || name.starts_with("codex-")
}

fn enumerate_processes() -> Result<Vec<ProcessInfo>, String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().to_string());
        }

        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut result = Vec::new();

        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let length = entry
                    .szExeFile
                    .iter()
                    .position(|&ch| ch == 0)
                    .unwrap_or(entry.szExeFile.len());
                result.push(ProcessInfo {
                    pid: entry.th32ProcessID,
                    parent_pid: entry.th32ParentProcessID,
                    name: String::from_utf16_lossy(&entry.szExeFile[..length]),
                });

                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }

        CloseHandle(snapshot);
        Ok(result)
    }
}

fn target_pids(processes: &[ProcessInfo], own_pid: u32) -> HashSet<u32> {
    let mut targets: HashSet<u32> = processes
        .iter()
        .filter(|process| process.pid != own_pid && is_target(&process.name))
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
            break;
        }
    }

    targets
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

fn find_targets() -> Result<Vec<ProcessInfo>, String> {
    let processes = enumerate_processes()?;
    let ids = target_pids(&processes, unsafe { GetCurrentProcessId() });
    let parents: HashMap<u32, u32> = processes
        .iter()
        .map(|process| (process.pid, process.parent_pid))
        .collect();
    let mut targets: Vec<ProcessInfo> = processes
        .into_iter()
        .filter(|process| ids.contains(&process.pid))
        .collect();
    targets.sort_by_key(|process| std::cmp::Reverse(process_depth(process, &parents)));
    Ok(targets)
}

fn kill_all() -> (Vec<String>, Vec<String>) {
    let targets = match find_targets() {
        Ok(targets) => targets,
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("Не удалось прочитать список процессов: {error}")],
            );
        }
    };
    let mut killed = Vec::new();
    let mut failed = Vec::new();

    for process in targets {
        let label = format!("{} (PID {})", process.name, process.pid);
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, process.pid);
            if handle.is_null() {
                failed.push(format!("{label}: {}", std::io::Error::last_os_error()));
                continue;
            }
            if TerminateProcess(handle, 1) != 0 {
                killed.push(label);
            } else {
                failed.push(format!("{label}: {}", std::io::Error::last_os_error()));
            }
            CloseHandle(handle);
        }
    }

    (killed, failed)
}

fn is_elevated() -> bool {
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation: TOKEN_ELEVATION = zeroed();
        let mut returned = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        ) != 0;
        CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
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

unsafe fn set_text(hwnd: HWND, value: &str) {
    unsafe { SetWindowTextW(hwnd, wide(value).as_ptr()) };
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
                let instance = GetModuleHandleW(null());
                let font = GetStockObject(DEFAULT_GUI_FONT);
                let title = CreateWindowExW(
                    0,
                    wide("STATIC").as_ptr(),
                    wide("KILL CODEX").as_ptr(),
                    WS_CHILD | WS_VISIBLE,
                    28,
                    22,
                    480,
                    38,
                    hwnd,
                    null_mut(),
                    instance,
                    null(),
                );
                let subtitle = CreateWindowExW(
                    0,
                    wide("STATIC").as_ptr(),
                    wide("ChatGPT.exe, codex.exe и все процессы codex-*").as_ptr(),
                    WS_CHILD | WS_VISIBLE,
                    30,
                    62,
                    480,
                    26,
                    hwnd,
                    null_mut(),
                    instance,
                    null(),
                );
                let button = CreateWindowExW(
                    0,
                    wide("BUTTON").as_ptr(),
                    wide("УБИТЬ ВСЕ ПРОЦЕССЫ CODEX").as_ptr(),
                    WS_CHILD | WS_VISIBLE | BS_OWNERDRAW as u32,
                    30,
                    100,
                    480,
                    58,
                    hwnd,
                    ID_KILL as *mut _,
                    instance,
                    null(),
                );
                let status = CreateWindowExW(
                    0,
                    wide("STATIC").as_ptr(),
                    wide("Готово к запуску").as_ptr(),
                    WS_CHILD | WS_VISIBLE,
                    30,
                    180,
                    480,
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
                    210,
                    480,
                    120,
                    hwnd,
                    null_mut(),
                    instance,
                    null(),
                );
                for control in [title, subtitle, button, status, details] {
                    SendMessageW(control, WM_SETFONT, font as usize, 1);
                }
                BUTTON.store(button as isize, Ordering::Relaxed);
                STATUS.store(status as isize, Ordering::Relaxed);
                DETAILS.store(details as isize, Ordering::Relaxed);
            }
            0
        }
        WM_COMMAND if wparam & 0xffff == ID_KILL => {
            unsafe { EnableWindow(BUTTON.load(Ordering::Relaxed) as HWND, 0) };
            let (killed, failed) = kill_all();
            let status = if failed.is_empty() {
                format!("Готово: завершено процессов — {}", killed.len())
            } else {
                format!("Завершено: {}; ошибок: {}", killed.len(), failed.len())
            };
            let mut lines: Vec<String> =
                killed.into_iter().map(|item| format!("✓ {item}")).collect();
            lines.extend(failed.into_iter().map(|item| format!("✗ {item}")));
            if lines.is_empty() {
                lines.push("Процессы Codex не найдены.".to_string());
            }
            unsafe {
                set_text(STATUS.load(Ordering::Relaxed) as HWND, &status);
                set_text(DETAILS.load(Ordering::Relaxed) as HWND, &lines.join("\r\n"));
                EnableWindow(BUTTON.load(Ordering::Relaxed) as HWND, 1);
            }
            0
        }
        WM_DRAWITEM => {
            let draw = unsafe { &*(lparam as *const DRAWITEMSTRUCT) };
            if draw.CtlID == ID_KILL as u32 {
                unsafe {
                    let selected = draw.itemState & ODS_SELECTED != 0;
                    let color = if selected { 0x002020A8 } else { 0x002C2CC4 };
                    let brush = CreateSolidBrush(color);
                    FillRect(draw.hDC, &draw.rcItem, brush);
                    DeleteObject(brush);
                    SetBkMode(draw.hDC, TRANSPARENT as i32);
                    SetTextColor(draw.hDC, 0x00FFFFFF);
                    let mut rect = draw.rcItem;
                    DrawTextW(
                        draw.hDC,
                        wide("УБИТЬ ВСЕ ПРОЦЕССЫ CODEX").as_ptr(),
                        -1,
                        &mut rect,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                }
                return 1;
            }
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn show_dry_run() {
    let message = match find_targets() {
        Ok(targets) if targets.is_empty() => "Процессы Codex не найдены.".to_string(),
        Ok(targets) => format!(
            "Будут завершены:\n\n{}",
            targets
                .iter()
                .map(|process| format!("{} (PID {})", process.name, process.pid))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        Err(error) => format!("Не удалось прочитать список процессов:\n\n{error}"),
    };
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(&message).as_ptr(),
            wide("Kill Codex — безопасная проверка").as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn run_gui() {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide("KillCodexWindow");
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
            wide("Kill Codex").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            556,
            390,
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

fn main() {
    if std::env::args().any(|argument| argument.eq_ignore_ascii_case("--dry-run")) {
        show_dry_run();
        return;
    }
    if !is_elevated() {
        if !relaunch_elevated() {
            unsafe {
                MessageBoxW(
                    null_mut(),
                    wide("Для завершения всех процессов нужны права администратора.").as_ptr(),
                    wide("Kill Codex").as_ptr(),
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

    #[test]
    fn matches_only_codex_process_names() {
        for name in [
            "ChatGPT.exe",
            "chatgpt",
            "codex.exe",
            "CODEX",
            "codex-code-mode-host.exe",
            "codex-computer-use.exe",
        ] {
            assert!(is_target(name), "expected target: {name}");
        }
        for name in ["Kill-Codex.exe", "explorer.exe", "my-codex.exe", "code.exe"] {
            assert!(!is_target(name), "unexpected target: {name}");
        }
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
        assert_eq!(target_pids(&processes, 12), HashSet::from([10, 11]));
        assert_eq!(target_pids(&processes, 99), HashSet::from([10, 11, 12]));
    }
}
