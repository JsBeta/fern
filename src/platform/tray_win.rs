//! Minimal Windows notification-area icon.
//!
//! The server runs on its own threads; the tray lives on a dedicated thread
//! with a hidden message-only window. Everything here is best effort: if any
//! step fails the server keeps running without an icon.
//!
//! The artwork is `ui/fern.ico` embedded at compile time (`tools/make-icon.ps1`
//! regenerates it); Win32 has no SVG support, so the multi-size .ico is the
//! only format the shell can consume.

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

/// `GetSystemMetrics` index and the embedded artwork, both used to size the
/// tray icon the way the shell would.
const SM_CXSMICON: i32 = 49;
const FALLBACK_ICON_SIZE: i32 = 16;
static APP_ICON: &[u8] = include_bytes!("../../ui/fern.ico");

const MENU_OPEN: usize = 1;
const MENU_EXIT: usize = 2;

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
    fn CreateIconFromResourceEx(
        bits: *const u8,
        size: u32,
        is_icon: i32,
        version: u32,
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

/// Picks the `.ico` entry closest to `size` and turns it into an `HICON`.
/// Returns null on anything unexpected so the caller can fall back.
unsafe fn load_app_icon(size: i32) -> HICON {
    let data = APP_ICON;
    if data.len() < 6 {
        return std::ptr::null_mut();
    }
    let count = u16::from_le_bytes([data[4], data[5]]) as usize;
    if count == 0 || data.len() < 6 + count * 16 {
        return std::ptr::null_mut();
    }

    let mut best: Option<(usize, usize, i32)> = None;
    for index in 0..count {
        let entry = &data[6 + index * 16..6 + index * 16 + 16];
        let width = match entry[0] {
            0 => 256,
            value => value as i32,
        };
        let length = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
        let offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        if length == 0 || offset + length > data.len() {
            continue;
        }
        let delta = (width - size).abs();
        if best.map_or(true, |(_, _, closest)| delta < closest) {
            best = Some((offset, length, delta));
        }
    }

    let Some((offset, length, _)) = best else {
        return std::ptr::null_mut();
    };
    CreateIconFromResourceEx(
        data[offset..].as_ptr(),
        length as u32,
        1,           // fIcon
        0x0003_0000, // version 3.0
        0,           // width: keep the size stored in the image
        0,           // height
        0,           // LR_DEFAULTCOLOR
    )
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
    let open = wide("Open in browser");
    let exit = wide("Exit");
    AppendMenuW(menu, MF_STRING, MENU_OPEN, open.as_ptr());
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
    let embedded = load_app_icon(small_icon_size());
    let icon = if embedded.is_null() {
        LoadIconW(std::ptr::null_mut(), wide_resource(IDI_APPLICATION))
    } else {
        embedded
    };
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
