use std::{
    path::PathBuf,
    process::{Child, Command},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use twodrive_windows::{ipc, protocol::*};
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Shell::*, WindowsAndMessaging::*},
};
static APP: OnceLock<App> = OnceLock::new();
struct App {
    root: PathBuf,
    full: bool,
    status: Mutex<Option<Snapshot>>,
    ui: Mutex<Option<Child>>,
    pending: AtomicBool,
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn rpc(command: crate::native::CommandType) -> anyhow::Result<Reply> {
    let app = APP.get().unwrap();
    let id = format!(
        "tray-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
    );
    tokio::runtime::Runtime::new()?.block_on(ipc::request(
        &app.root,
        &Request {
            version: VERSION,
            id,
            command,
        },
    ))
}
type CommandType = twodrive_windows::protocol::Command;
fn poll(command: CommandType) {
    let app = APP.get().unwrap();
    if app.pending.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let response = rpc(command);
        *app.status.lock().unwrap() = response.ok().filter(|r| r.ok).map(|r| r.snapshot);
        app.pending.store(false, Ordering::SeqCst);
    });
}
fn launch_ui(tray: bool) {
    let app = APP.get().unwrap();
    if !app.full {
        return;
    }
    let mut child = app.ui.lock().unwrap();
    if child
        .as_mut()
        .is_some_and(|c| c.try_wait().ok().flatten().is_none())
    {
        return;
    }
    let exe = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("ui")
        .join("twodrive_full.exe");
    let mut command = Command::new(exe);
    command.arg("--state").arg(&app.root);
    if tray {
        command.arg("--tray");
    }
    *child = command.spawn().ok();
}
fn tooltip() -> String {
    let app = APP.get().unwrap();
    let state = app.status.lock().unwrap();
    match state.as_ref() {
        None => "TwoDrive Preview - backend disconnected".into(),
        Some(s) => format!("TwoDrive Preview - {} / {}", s.mode, s.status),
    }
}
unsafe fn icon(hwnd: HWND, action: u32) {
    unsafe {
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_APP + 1;
        data.hIcon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
        for (dst, src) in data
            .szTip
            .iter_mut()
            .take(127)
            .zip(tooltip().encode_utf16())
        {
            *dst = src;
        }
        Shell_NotifyIconW(action, &data);
    }
}
unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_TIMER => {
                poll(CommandType::Snapshot);
                icon(hwnd, NIM_MODIFY);
                0
            }
            m if m == WM_APP + 1 => {
                if lparam as u32 == WM_LBUTTONUP {
                    if APP.get().unwrap().full {
                        launch_ui(true);
                    } else {
                        show_menu(hwnd);
                    }
                } else if lparam as u32 == WM_RBUTTONUP {
                    show_menu(hwnd);
                }
                0
            }
            WM_DESTROY => {
                icon(hwnd, NIM_DELETE);
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}
unsafe fn show_menu(hwnd: HWND) {
    unsafe {
        let app = APP.get().unwrap();
        let menu = CreatePopupMenu();
        AppendMenuW(menu, MF_STRING | MF_DISABLED, 0, wide(&tooltip()).as_ptr());
        if app.full {
            AppendMenuW(menu, MF_STRING, 1, wide("Management center").as_ptr());
        }
        let paused = app
            .status
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.paused);
        let connected = app.status.lock().unwrap().is_some();
        AppendMenuW(
            menu,
            MF_STRING | if connected { 0 } else { MF_DISABLED },
            2,
            wide(if paused {
                "Resume"
            } else {
                "Pause new transfers"
            })
            .as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING,
            3,
            wide("Close tray (engine keeps running)").as_ptr(),
        );
        let mut point: POINT = std::mem::zeroed();
        GetCursorPos(&mut point);
        SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);
        match chosen {
            1 => launch_ui(false),
            2 => poll(CommandType::SetPaused { paused: !paused }),
            3 => {
                DestroyWindow(hwnd);
            }
            _ => (),
        }
    }
}
pub fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 2 && args[0] == "--state",
        "usage: twodrive-tray --state ABSOLUTE_DIRECTORY [--full] [--mock]"
    );
    anyhow::ensure!(
        args.iter().skip(2).all(|a| a == "--full" || a == "--mock"),
        "unknown option"
    );
    let root = PathBuf::from(&args[1]);
    anyhow::ensure!(root.is_absolute(), "absolute state directory required");
    let _lock = ipc::lock(&root, "tray")?;
    APP.set(App {
        root: root.clone(),
        full: args.iter().any(|a| a == "--full"),
        status: Mutex::new(None),
        ui: Mutex::new(None),
        pending: AtomicBool::new(false),
    })
    .ok();
    if rpc(CommandType::Snapshot).is_err() {
        use std::os::windows::process::CommandExt;
        let exe = std::env::current_exe()?
            .parent()
            .unwrap()
            .join("twodrive-engine.exe");
        let mut command = Command::new(exe);
        command
            .arg("serve")
            .arg("--state")
            .arg(root)
            .creation_flags(0x08000000);
        if args.iter().any(|a| a == "--mock") {
            command.arg("--mock");
        }
        command.spawn()?;
    }
    unsafe {
        let class = wide("TwoDriveNativeTrayV1");
        let instance = GetModuleHandleW(std::ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(procedure);
        wc.hInstance = instance;
        wc.lpszClassName = class.as_ptr();
        anyhow::ensure!(RegisterClassW(&wc) != 0, "register tray class failed");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        anyhow::ensure!(!hwnd.is_null(), "create tray host failed");
        icon(hwnd, NIM_ADD);
        SetTimer(hwnd, 1, 1000, None);
        poll(CommandType::Snapshot);
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}
