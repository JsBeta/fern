//! Minimal Windows notification-area icon.
//!
//! The server runs on its own threads; the tray lives on a dedicated thread
//! with a hidden message-only window. Everything here is best effort: if any
//! step fails the server keeps running without an icon.
//!
//! The artwork is `ui/fern.ico` (`tools/make-icon.ps1` regenerates it). It is
//! not embedded here: `build.rs` compiles `ui/fern.rc` into the executable's
//! `.rsrc` section and the icon is loaded back out of it with `LoadImageW`. So
//! the file exists exactly once in the binary, and Explorer shows the same
//! artwork for `fern.exe` as the tray does. Win32 has no SVG support, so the
//! multi-size .ico is the only format the shell can consume.

#![allow(non_snake_case)]

use std::ffi::c_void;

type HWND = *mut c_void;
type HINSTANCE = *mut c_void;
type HICON = *mut c_void;
type HCURSOR = *mut c_void;
type HBRUSH = *mut c_void;
type HMENU = *mut c_void;
type LPCWSTR = *const u16;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;
type WNDPROC = Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>;

const WM_NULL: u32 = 0x0000;
const WM_DESTROY: u32 = 0x0002;
const WM_COMMAND: u32 = 0x0111;
const WM_APP: u32 = 0x8000;
const WM_TRAY: u32 = WM_APP + 1;
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_RBUTTONUP: u32 = 0x0205;

const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIM_ADD: u32 = 0x00;
const NIM_DELETE: u32 = 0x02;

const MF_STRING: u32 = 0x0000;
const MF_SEPARATOR: u32 = 0x0800;
const TPM_RIGHTBUTTON: u32 = 0x0002;
const TPM_RETURNCMD: u32 = 0x0100;

const IDI_APPLICATION: usize = 32512;
const IDC_ARROW: usize = 32512;

/// `GetSystemMetrics` index and the icon id, both used to size the tray icon
/// the way the shell would.
const SM_CXSMICON: i32 = 49;
const FALLBACK_ICON_SIZE: i32 = 16;

/// Id of the `RT_GROUP_ICON` in `ui/fern.rc`; keep the two in sync.
const RES_ICON: usize = 1;
const IMAGE_ICON: u32 = 1;
const LR_DEFAULTCOLOR: u32 = 0;
/// Avoids a `DestroyIcon`: the handle is cached and shared by the process.
const LR_SHARED: u32 = 0x8000;

const MENU_OPEN: usize = 1;
const MENU_RESTART: usize = 2;
const MENU_ABOUT: usize = 3;
const MENU_EXIT: usize = 4;

const MB_OK: u32 = 0x0000_0000;
const MB_ICONINFORMATION: u32 = 0x0000_0040;

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct Msg {
    hwnd: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
    time: u32,
    pt: Point,
    private: u32,
}

#[repr(C)]
struct WndClassW {
    style: u32,
    wnd_proc: WNDPROC,
    cls_extra: i32,
    wnd_extra: i32,
    instance: HINSTANCE,
    icon: HICON,
    cursor: HCURSOR,
    background: HBRUSH,
    menu_name: LPCWSTR,
    class_name: LPCWSTR,
}

#[repr(C)]
struct NotifyIconDataW {
    cb_size: u32,
    hwnd: HWND,
    id: u32,
    flags: u32,
    callback_message: u32,
    icon: HICON,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: [u8; 16],
    balloon_icon: HICON,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(class: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class_name: LPCWSTR,
        window_name: LPCWSTR,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: HWND,
        menu: HMENU,
        instance: HINSTANCE,
        param: *mut c_void,
    ) -> HWND;
    fn DefWindowProcW(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT;
    fn DestroyWindow(hwnd: HWND) -> i32;
    fn PostMessageW(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: HWND, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> LRESULT;
    fn PostQuitMessage(code: i32);
    fn LoadIconW(instance: HINSTANCE, name: LPCWSTR) -> HICON;
    fn LoadCursorW(instance: HINSTANCE, name: LPCWSTR) -> HCURSOR;
    fn GetSystemMetrics(index: i32) -> i32;
    fn LoadImageW(
        instance: HINSTANCE,
        name: LPCWSTR,
        kind: u32,
        width: i32,
        height: i32,
        flags: u32,
    ) -> HICON;
    fn SetForegroundWindow(hwnd: HWND) -> i32;
    fn CreatePopupMenu() -> HMENU;
    fn AppendMenuW(menu: HMENU, flags: u32, id: usize, text: LPCWSTR) -> i32;
    fn TrackPopupMenu(
        menu: HMENU,
        flags: u32,
        x: i32,
        y: i32,
        reserved: i32,
        hwnd: HWND,
        rect: *const c_void,
    ) -> i32;
    fn DestroyMenu(menu: HMENU) -> i32;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn MessageBoxW(window: HWND, text: LPCWSTR, caption: LPCWSTR, flags: u32) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconDataW) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: LPCWSTR) -> HINSTANCE;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_resource(id: usize) -> LPCWSTR {
    id as LPCWSTR
}

fn fill(buffer: &mut [u16], text: &str) {
    let encoded: Vec<u16> = text.encode_utf16().collect();
    let n = encoded.len().min(buffer.len().saturating_sub(1));
    buffer[..n].copy_from_slice(&encoded[..n]);
    buffer[n] = 0;
}

/// Edge length of the shell's small icon: 16 px at 100% scaling, larger on a
/// high-DPI desktop.
unsafe fn small_icon_size() -> i32 {
    let size = GetSystemMetrics(SM_CXSMICON);
    if size > 0 {
        size
    } else {
        FALLBACK_ICON_SIZE
    }
}

/// Loads the icon resource that `build.rs` linked in, scaled to `size`.
/// `LoadImageW` picks the closest entry in the group and scales it, so no
/// hand-rolled `.ico` parsing is needed. Returns null if the resource is
/// missing, which lets the caller fall back.
unsafe fn load_app_icon(size: i32) -> HICON {
    let module = GetModuleHandleW(std::ptr::null());
    let handle = LoadImageW(
        module,
        wide_resource(RES_ICON),
        IMAGE_ICON,
        size,
        size,
        LR_DEFAULTCOLOR | LR_SHARED,
    );
    if handle.is_null() {
        LoadIconW(module, wide_resource(IDI_APPLICATION))
    } else {
        handle
    }
}

/// URL opened when the icon is double clicked or the menu item is chosen.
struct TrayState {
    url: Vec<u16>,
    tip: Vec<u16>,
}

static mut STATE: Option<TrayState> = None;

unsafe fn state<'a>() -> Option<&'a TrayState> {
    let ptr = std::ptr::addr_of!(STATE);
    (*ptr).as_ref()
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let event = (l as u32) & 0xffff;
            if event == WM_LBUTTONDBLCLK {
                open_url();
            } else if event == WM_RBUTTONUP {
                show_menu(hwnd);
            }
            0
        }
        WM_COMMAND => {
            match w & 0xffff {
                MENU_OPEN => open_url(),
                MENU_RESTART => restart_app(hwnd),
                MENU_ABOUT => show_about(hwnd),
                MENU_EXIT => exit_app(hwnd),
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

unsafe fn open_url() {
    if let Some(s) = state() {
        let url = String::from_utf16_lossy(&s.url[..s.url.len().saturating_sub(1)]);
        let _ = crate::platform::command("cmd")
            .args(["/c", "start", "", &url])
            .spawn();
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let open = wide("打开");
    let restart = wide("重启");
    let about = wide("关于");
    let exit = wide("退出");
    AppendMenuW(menu, MF_STRING, MENU_OPEN, open.as_ptr());
    AppendMenuW(menu, MF_STRING, MENU_RESTART, restart.as_ptr());
    AppendMenuW(menu, MF_STRING, MENU_ABOUT, about.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, MENU_EXIT, exit.as_ptr());
    let mut pt = Point { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    let choice = TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD,
        pt.x,
        pt.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    DestroyMenu(menu);
    // Documented workaround: without this the popup can linger on screen when
    // dismissed by clicking elsewhere (KB135788).
    PostMessageW(hwnd, WM_NULL, 0, 0);
    match choice as usize {
        MENU_OPEN => open_url(),
        MENU_RESTART => restart_app(hwnd),
        MENU_ABOUT => show_about(hwnd),
        MENU_EXIT => exit_app(hwnd),
        _ => {}
    }
}

unsafe fn build_icon_data(hwnd: HWND, icon: HICON) -> NotifyIconDataW {
    let mut data: NotifyIconDataW = std::mem::zeroed();
    data.cb_size = std::mem::size_of::<NotifyIconDataW>() as u32;
    data.hwnd = hwnd;
    data.id = 1;
    data.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.callback_message = WM_TRAY;
    data.icon = icon;
    if let Some(s) = state() {
        fill(&mut data.tip, &String::from_utf16_lossy(&s.tip[..s.tip.len() - 1]));
    }
    data
}

unsafe fn remove_icon(hwnd: HWND) {
    let mut data: NotifyIconDataW = std::mem::zeroed();
    data.cb_size = std::mem::size_of::<NotifyIconDataW>() as u32;
    data.hwnd = hwnd;
    data.id = 1;
    Shell_NotifyIconW(NIM_DELETE, &mut data);
}

/// Drops the icon and ends the process.
///
/// The tray thread only owns the message loop; the server threads sit in
/// `accept()` with nothing that can wake them, so letting `run()` return would
/// leave a headless server listening with no icon to click. All logging is
/// written and flushed per line, so tearing the process down here is safe.
unsafe fn exit_app(hwnd: HWND) -> ! {
    remove_icon(hwnd);
    DestroyWindow(hwnd);
    crate::log::info("exit requested from the tray menu");
    std::process::exit(0);
}

/// Replaces the running process with a fresh copy of itself.
///
/// The same limitation as `exit_app` applies: the listener threads cannot be
/// asked to stop, so the old process is torn down and a new one is started with
/// the original command line. Windows keeps the listening socket reserved until
/// this process is gone, so the replacement is told which pid to wait for
/// (`--wait-pid`) before it tries to bind.
unsafe fn restart_app(hwnd: HWND) -> ! {
    remove_icon(hwnd);
    DestroyWindow(hwnd);
    crate::log::info("restart requested from the tray menu");
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            crate::log::error(&format!("restart failed: {}", e));
            std::process::exit(1);
        }
    };
    let mut cmd = crate::platform::command(&exe.to_string_lossy());
    cmd.args(std::env::args_os().skip(1))
        .arg("--wait-pid")
        .arg(std::process::id().to_string());
    if let Err(e) = cmd.spawn() {
        crate::log::error(&format!("restart failed: {}", e));
        std::process::exit(1);
    }
    std::process::exit(0);
}

/// Shows the version and the served address in a message box.
unsafe fn show_about(hwnd: HWND) {
    let mut body = format!("fern {}\n\n", env!("CARGO_PKG_VERSION"));
    if let Some(s) = state() {
        let url = String::from_utf16_lossy(&s.url[..s.url.len().saturating_sub(1)]);
        body.push_str(&url);
        body.push('\n');
    }
    body.push_str("A small HTTP file server.");
    let text = wide(&body);
    let caption = wide("关于 fern");
    MessageBoxW(
        hwnd,
        text.as_ptr(),
        caption.as_ptr(),
        MB_OK | MB_ICONINFORMATION,
    );
}

/// Adds the notification icon. `url` is opened on double click, `tip` becomes
/// the hover text. Returns immediately; the caller keeps running.
pub fn spawn(url: String, tip: String) {
    let url = wide(&url);
    let tip = wide(&tip);
    unsafe {
        let ptr = std::ptr::addr_of_mut!(STATE);
        *ptr = Some(TrayState { url, tip });
    }
    let _ = std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || unsafe {
            run();
        });
}

unsafe fn run() {
    let instance = GetModuleHandleW(std::ptr::null());
    let class_name = wide("fern_tray_window");
    let window_title = wide("fern");
    let icon = load_app_icon(small_icon_size());
    let class = WndClassW {
        style: 0,
        wnd_proc: Some(wnd_proc),
        cls_extra: 0,
        wnd_extra: 0,
        instance,
        icon,
        cursor: LoadCursorW(std::ptr::null_mut(), wide_resource(IDC_ARROW)),
        background: std::ptr::null_mut(),
        menu_name: std::ptr::null(),
        class_name: class_name.as_ptr(),
    };
    if RegisterClassW(&class) == 0 {
        crate::log::warn("tray: cannot register the window class");
        return;
    }
    let hwnd = CreateWindowExW(
        0,
        class.class_name,
        window_title.as_ptr(),
        0,
        0,
        0,
        0,
        0,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        instance,
        std::ptr::null_mut(),
    );
    if hwnd.is_null() {
        crate::log::warn("tray: cannot create the hidden window");
        return;
    }
    let mut data = build_icon_data(hwnd, icon);
    if Shell_NotifyIconW(NIM_ADD, &mut data) == 0 {
        crate::log::warn("tray: Shell_NotifyIconW failed");
        return;
    }
    crate::log::info("tray icon added");
    let mut msg: Msg = std::mem::zeroed();
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}
