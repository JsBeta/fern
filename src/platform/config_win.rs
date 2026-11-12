//! Configuration window opened from the tray menu.
//!
//! The crate ships no UI toolkit, so the form is a plain top-level window with
//! child controls, built with the same hand-written FFI as `tray_win`. Saving
//! merges the edited values into the configuration file, validates the result
//! with [`Config::from_text`] before it touches the disk, and then swaps the
//! live configuration so the new values apply immediately. A changed
//! `Fern.Listen` cannot be rebound in place, so that case hands the process
//! over to a fresh copy through `--wait-pid`, like the tray "restart" item.
//!
//! Directives the window does not show are preserved verbatim: only the blocks
//! listed in [`Config::editable_blocks`] are rewritten, everything else in the
//! file — comments, blank lines, `Fern.User`, `Fern.Mime` — survives untouched.

#![allow(non_snake_case)]

use crate::config::{
    merge_directives, parse_perms, perms_text, quote_value, Config, DirectiveBlock, LogLevel,
};
use crate::server::Server;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

type HWND = *mut c_void;
type HINSTANCE = *mut c_void;
type HICON = *mut c_void;
type HCURSOR = *mut c_void;
type HBRUSH = *mut c_void;
type HFONT = *mut c_void;
type LPCWSTR = *const u16;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;
type WNDPROC = Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>;

const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_COMMAND: u32 = 0x0111;
const WM_SETFONT: u32 = 0x0030;

const WS_OVERLAPPED: u32 = 0x0000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_MINIMIZEBOX: u32 = 0x0002_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_VSCROLL: u32 = 0x0020_0000;

const ES_AUTOHSCROLL: u32 = 0x0080;
const CBS_DROPDOWNLIST: u32 = 0x0003;
const BS_AUTOCHECKBOX: u32 = 0x0003;
const BS_DEFPUSHBUTTON: u32 = 0x0001;
const BS_PUSHBUTTON: u32 = 0x0000;
const SS_LEFT: u32 = 0x0000;

const SW_SHOW: i32 = 5;
const CW_USEDEFAULT: i32 = 0x8000_0000u32 as i32;

const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const CB_ERR: isize = -1;
const BM_SETCHECK: u32 = 0x00F1;
const BM_GETCHECK: u32 = 0x00F0;
const BST_CHECKED: WPARAM = 1;

const COLOR_BTNFACE: usize = 15;
const DEFAULT_GUI_FONT: usize = 17;

const MB_OK: u32 = 0x0000_0000;
const MB_ICONERROR: u32 = 0x0000_0010;
const MB_ICONINFORMATION: u32 = 0x0000_0040;

const ID_SAVE: usize = 1001;
const ID_CANCEL: usize = 1002;
const ID_BROWSE: usize = 1003;

/// `SHBrowseForFolder` flags: real directories only, plus the resizable tree.
const BIF_RETURNONLYFSDIRS: u32 = 0x0001;
const BIF_EDITBOX: u32 = 0x0010;
const BIF_NEWDIALOGSTYLE: u32 = 0x0040;
const COINIT_APARTMENTTHREADED: u32 = 0x0002;
const S_OK: i32 = 0;
const S_FALSE: i32 = 1;

const MAX_PATH_CHARS: usize = 260;

const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

/// Fixed client width; the height is measured from the laid-out rows by
/// [`fit`], which also centres the window.
const WIDTH: i32 = 520;
const HEIGHT: i32 = 380;
const MARGIN: i32 = 16;
const LABEL_X: i32 = 16;
const LABEL_W: i32 = 80;
const FIELD_X: i32 = 104;
/// Ends at `WIDTH - MARGIN`, so no field or hint can run under the frame.
const FIELD_W: i32 = WIDTH - FIELD_X - MARGIN;
const EDIT_H: i32 = 22;
const BUTTON_H: i32 = EDIT_H + 4;
const BUTTON_W: i32 = 84;
const ROW: i32 = 30;
const HINT_H: i32 = 18;
/// Keeps a full-width hint directly under its edit box.
const HINT_Y: i32 = EDIT_H + 2;
/// Keeps an inline hint on the baseline of the text inside an edit.
const HINT_DY: i32 = 3;
const FIRST_ROW: i32 = 18;
const GAP: i32 = 8;

const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;

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
struct BrowseInfoW {
    hwnd_owner: HWND,
    pidl_root: *mut c_void,
    display_name: *mut u16,
    title: LPCWSTR,
    flags: u32,
    callback: *mut c_void,
    param: LPARAM,
    image: i32,
}

#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(class: *const WndClassW) -> u16;
    #[allow(clippy::too_many_arguments)]
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
        menu: *mut c_void,
        instance: HINSTANCE,
        param: *mut c_void,
    ) -> HWND;
    fn DefWindowProcW(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT;
    fn DestroyWindow(hwnd: HWND) -> i32;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    fn PostQuitMessage(code: i32);
    fn SendMessageW(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT;
    fn GetWindowTextW(hwnd: HWND, text: *mut u16, max: i32) -> i32;
    fn GetWindowTextLengthW(hwnd: HWND) -> i32;
    fn SetWindowTextW(hwnd: HWND, text: LPCWSTR) -> i32;
    fn SetForegroundWindow(hwnd: HWND) -> i32;
    fn MessageBoxW(window: HWND, text: LPCWSTR, caption: LPCWSTR, flags: u32) -> i32;
    fn LoadCursorW(instance: HINSTANCE, name: LPCWSTR) -> HCURSOR;
    fn AdjustWindowRectEx(rect: *mut Rect, style: u32, menu: i32, ex_style: u32) -> i32;
    fn SetWindowPos(
        hwnd: HWND,
        after: HWND,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn SHBrowseForFolderW(info: *mut BrowseInfoW) -> *mut c_void;
    fn SHGetPathFromIDListW(pidl: *const c_void, path: *mut u16) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, co_init: u32) -> i32;
    fn CoUninitialize();
    fn CoTaskMemFree(block: *mut c_void);
}

#[link(name = "gdi32")]
extern "system" {
    fn GetStockObject(index: i32) -> *mut c_void;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: LPCWSTR) -> HINSTANCE;
}

const IDC_ARROW: usize = 32512;

/// Controls of the form, plus what the save path needs to know about the file.
struct Form {
    path: PathBuf,
    /// Text as it is on disk, so directives the window does not show survive.
    original: String,
    server: Arc<Server>,
    owner: HWND,
    h_root: HWND,
    h_listen: HWND,
    h_level: HWND,
    h_max: HWND,
    h_guest: HWND,
    h_title: HWND,
    h_notice: HWND,
    h_logdir: HWND,
    h_trash: HWND,
    h_days: HWND,
}

/// Live server, installed by `after_start` so the tray menu can reach it.
static SERVER: Mutex<Option<Arc<Server>>> = Mutex::new(None);

/// Window handle of the open form, or 0. The tray thread reads it to focus an
/// existing window instead of opening a second one.
static OPEN: AtomicUsize = AtomicUsize::new(0);

/// Set while a handover to a fresh process is in progress.
static RESTARTING: AtomicBool = AtomicBool::new(false);

/// Window handles are only valid on the thread that created them, which is why
/// this lives in a `static mut` that only the window procedure touches.
static mut FORM: Option<Form> = None;

unsafe fn form<'a>() -> Option<&'a Form> {
    (*std::ptr::addr_of!(FORM)).as_ref()
}

unsafe fn form_mut<'a>() -> Option<&'a mut Form> {
    (*std::ptr::addr_of_mut!(FORM)).as_mut()
}

/// Remembers the server so the tray menu can reach it later. Called once from
/// `after_start`, before the tray icon appears.
pub fn install(server: Arc<Server>) {
    if let Ok(mut slot) = SERVER.lock() {
        *slot = Some(server);
    }
}

/// Opens the configuration window, or brings the existing one to the front.
pub unsafe fn open(owner: HWND) {
    let existing = OPEN.load(Ordering::Acquire);
    if existing != 0 {
        SetForegroundWindow(existing as HWND);
        return;
    }
    let server = match SERVER.lock() {
        Ok(slot) => slot.clone(),
        Err(e) => e.into_inner().clone(),
    };
    let Some(server) = server else { return };
    if OPEN.swap(1, Ordering::AcqRel) != 0 {
        return;
    }
    // A `HWND` is not `Send`, so the handle crosses the thread boundary as a
    // plain integer; it stays valid because the tray window outlives the form.
    let owner = owner as usize;
    let spawned = std::thread::Builder::new()
        .name("config".into())
        .spawn(move || unsafe {
            run(server, owner as HWND);
        });
    if spawned.is_err() {
        OPEN.store(0, Ordering::Release);
        crate::log::warn("config: cannot start the window thread");
    }
}

unsafe fn run(server: Arc<Server>, owner: HWND) {
    let cfg = server.cfg();
    let path = cfg.config_path.clone().unwrap_or_else(default_path);
    let original = std::fs::read_to_string(&path).unwrap_or_default();

    let instance = GetModuleHandleW(std::ptr::null());
    let class_name = wide("fern_config_window");
    let title = wide("fern 配置");
    let class = WndClassW {
        style: 0,
        wnd_proc: Some(wnd_proc),
        cls_extra: 0,
        wnd_extra: 0,
        instance,
        icon: std::ptr::null_mut(),
        cursor: LoadCursorW(std::ptr::null_mut(), id_resource(IDC_ARROW)),
        background: (COLOR_BTNFACE + 1) as HBRUSH,
        menu_name: std::ptr::null(),
        class_name: class_name.as_ptr(),
    };
    // Re-registering after the first window closed fails harmlessly; only a
    // missing window is worth reporting.
    RegisterClassW(&class);
    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        title.as_ptr(),
        STYLE,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        WIDTH,
        HEIGHT,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        instance,
        std::ptr::null_mut(),
    );
    if hwnd.is_null() {
        crate::log::warn("config: cannot create the window");
        OPEN.store(0, Ordering::Release);
        return;
    }

    let font = GetStockObject(DEFAULT_GUI_FONT as i32) as HFONT;
    let mut row = FIRST_ROW;
    // Right edge every control aligns to; `FIELD_W` already stops there.
    let right = FIELD_X + FIELD_W;

    label(hwnd, font, "共享目录", row);
    let h_root = edit(
        hwnd,
        font,
        &cfg.root.to_string_lossy(),
        FIELD_X,
        row,
        FIELD_W - 64 - GAP,
    );
    button(hwnd, font, "浏览…", ID_BROWSE, right - 64, row, 64);
    row += ROW;

    label(hwnd, font, "监听地址", row);
    let h_listen = edit(hwnd, font, &cfg.listen.join(" "), FIELD_X, row, FIELD_W);
    hint(
        hwnd,
        font,
        "例：4545、0.0.0.0:4545、[::]:4545,Dual；多个用空格分隔",
        FIELD_X,
        row + HINT_Y,
        FIELD_W,
    );
    row += HINT_Y + HINT_H + GAP;

    label(hwnd, font, "日志级别", row);
    let level_x = FIELD_X + 128;
    let h_level = combo(hwnd, font, FIELD_X, row, 120);
    for level in ["错误", "警告", "信息", "调试"] {
        add_item(h_level, level);
    }
    SendMessageW(h_level, CB_SETCURSEL, cfg.log_level.rank() as WPARAM, 0);
    hint(
        hwnd,
        font,
        "错误只记录失败，调试会记录每个请求",
        level_x,
        row + HINT_DY,
        right - level_x,
    );
    row += ROW;

    label(hwnd, font, "最大连接数", row);
    let hint_x = FIELD_X + 88;
    let h_max = edit(hwnd, font, &cfg.max_conns.to_string(), FIELD_X, row, 80);
    hint(
        hwnd,
        font,
        "同时处理的请求上限",
        hint_x,
        row + HINT_DY,
        right - hint_x,
    );
    row += ROW;

    label(hwnd, font, "访客权限", row);
    let h_guest = edit(hwnd, font, &perms_text(cfg.guest), FIELD_X, row, 80);
    hint(
        hwnd,
        font,
        "R L U M S T D 组合，0 表示禁止访问",
        hint_x,
        row + HINT_DY,
        right - hint_x,
    );
    row += ROW;

    label(hwnd, font, "页面标题", row);
    let h_title = edit(hwnd, font, &cfg.title, FIELD_X, row, FIELD_W);
    row += ROW;

    label(hwnd, font, "顶部公告", row);
    let h_notice = edit(hwnd, font, &cfg.notice, FIELD_X, row, FIELD_W);
    row += ROW;

    label(hwnd, font, "日志目录", row);
    let log_dir = cfg
        .log_dir
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let h_logdir = edit(hwnd, font, &log_dir, FIELD_X, row, FIELD_W);
    hint(
        hwnd,
        font,
        "留空表示不写日志文件",
        FIELD_X,
        row + HINT_Y,
        FIELD_W,
    );
    row += HINT_Y + HINT_H + GAP;

    let h_trash = checkbox(hwnd, font, "启用回收站", FIELD_X, row, 120);
    SendMessageW(
        h_trash,
        BM_SETCHECK,
        if cfg.trash_enabled { BST_CHECKED } else { 0 },
        0,
    );
    let days_x = FIELD_X + 136;
    label_at(hwnd, font, "保留天数", days_x, row, 68);
    let days_edit_x = days_x + 68;
    let h_days = edit(
        hwnd,
        font,
        &cfg.trash_max_days.to_string(),
        days_edit_x,
        row,
        56,
    );
    let days_hint_x = days_edit_x + 64;
    hint(hwnd, font, "天", days_hint_x, row + HINT_DY, right - days_hint_x);
    row += ROW;

    // The buttons sit below the last row with `GAP` of air above and `MARGIN`
    // below, instead of being pinned to the client edge.
    let buttons_y = row + GAP;
    button(
        hwnd,
        font,
        "保存",
        ID_SAVE,
        right - BUTTON_W * 2 - GAP,
        buttons_y,
        BUTTON_W,
    );
    button(hwnd, font, "取消", ID_CANCEL, right - BUTTON_W, buttons_y, BUTTON_W);
    // Controls are already placed, so the window only has to grow (or shrink)
    // around them and move to the middle of the screen.
    fit(hwnd, WIDTH, buttons_y + BUTTON_H + MARGIN);

    let slot = std::ptr::addr_of_mut!(FORM);
    *slot = Some(Form {
        path,
        original,
        server,
        owner,
        h_root,
        h_listen,
        h_level,
        h_max,
        h_guest,
        h_title,
        h_notice,
        h_logdir,
        h_trash,
        h_days,
    });

    ShowWindow(hwnd, SW_SHOW);
    SetForegroundWindow(hwnd);
    crate::log::info("config window opened");
    super::tray_win::run_message_loop();
    // The process may be handing itself over to a fresh copy, so the form is
    // only dropped when the loop ended for an ordinary reason.
    if (*std::ptr::addr_of!(FORM)).is_some() {
        *std::ptr::addr_of_mut!(FORM) = None;
        OPEN.store(0, Ordering::Release);
    }
}

/// Resizes the window so its client area is exactly `w` wide and `h` tall, and
/// centres it on the desktop. Called once the whole form is laid out, because
/// the needed height is only known after the rows have been placed.
unsafe fn fit(hwnd: HWND, w: i32, h: i32) {
    let mut rect = Rect {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    AdjustWindowRectEx(&mut rect, STYLE, 0, 0);
    let outer_w = rect.right - rect.left;
    let outer_h = rect.bottom - rect.top;
    let screen_w = GetSystemMetrics(SM_CXSCREEN).max(1);
    let screen_h = GetSystemMetrics(SM_CYSCREEN).max(1);
    let x = ((screen_w - outer_w) / 2).max(0);
    let y = ((screen_h - outer_h) / 2).max(0);
    SetWindowPos(
        hwnd,
        std::ptr::null_mut(),
        x,
        y,
        outer_w,
        outer_h,
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            match w & 0xffff {
                ID_SAVE => save(hwnd),
                ID_CANCEL => {
                    DestroyWindow(hwnd);
                }
                ID_BROWSE => browse(hwnd),
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            OPEN.store(0, Ordering::Release);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
unsafe fn child(
    class: &str,
    text: &str,
    style: u32,
    parent: HWND,
    font: HFONT,
    id: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> HWND {
    let class = wide(class);
    let text = wide(text);
    let hwnd = CreateWindowExW(
        0,
        class.as_ptr(),
        text.as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        x,
        y,
        w,
        h,
        parent,
        id as *mut c_void,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    if !hwnd.is_null() && !font.is_null() {
        SendMessageW(hwnd, WM_SETFONT, font as WPARAM, 1);
    }
    hwnd
}

/// Static text at an explicit position; labels keep id 0 because they never
/// send commands.
unsafe fn label_at(parent: HWND, font: HFONT, text: &str, x: i32, y: i32, w: i32) -> HWND {
    child("STATIC", text, SS_LEFT, parent, font, 0, x, y + 4, w, 16)
}

unsafe fn label(parent: HWND, font: HFONT, text: &str, y: i32) {
    label_at(parent, font, text, LABEL_X, y, LABEL_W);
}

unsafe fn hint(parent: HWND, font: HFONT, text: &str, x: i32, y: i32, w: i32) {
    child("STATIC", text, SS_LEFT, parent, font, 0, x, y, w, HINT_H);
}

unsafe fn edit(parent: HWND, font: HFONT, text: &str, x: i32, y: i32, w: i32) -> HWND {
    child(
        "EDIT",
        text,
        ES_AUTOHSCROLL | WS_BORDER | WS_TABSTOP,
        parent,
        font,
        0,
        x,
        y,
        w,
        EDIT_H,
    )
}

unsafe fn combo(parent: HWND, font: HFONT, x: i32, y: i32, w: i32) -> HWND {
    // The height is the dropped-down height; the closed box sizes itself.
    child(
        "COMBOBOX",
        "",
        CBS_DROPDOWNLIST | WS_VSCROLL | WS_TABSTOP,
        parent,
        font,
        0,
        x,
        y,
        w,
        200,
    )
}

unsafe fn checkbox(parent: HWND, font: HFONT, text: &str, x: i32, y: i32, w: i32) -> HWND {
    child(
        "BUTTON",
        text,
        BS_AUTOCHECKBOX | WS_TABSTOP,
        parent,
        font,
        0,
        x,
        y,
        w,
        EDIT_H,
    )
}

unsafe fn button(
    parent: HWND,
    font: HFONT,
    text: &str,
    id: usize,
    x: i32,
    y: i32,
    w: i32,
) -> HWND {
    let style = if id == ID_SAVE {
        BS_DEFPUSHBUTTON | WS_TABSTOP
    } else {
        BS_PUSHBUTTON | WS_TABSTOP
    };
    child("BUTTON", text, style, parent, font, id, x, y, w, EDIT_H + 4)
}

unsafe fn add_item(hwnd: HWND, text: &str) {
    let text = wide(text);
    SendMessageW(hwnd, CB_ADDSTRING, 0, text.as_ptr() as LPARAM);
}

unsafe fn get_text(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    SetWindowTextW(hwnd, text.as_ptr());
}

// ---------------------------------------------------------------------------
// Save path
// ---------------------------------------------------------------------------

unsafe fn save(hwnd: HWND) {
    let Some(form) = form_mut() else { return };
    let old = form.server.cfg();
    let Some(blocks) = collect(form, &old) else {
        return;
    };
    let merged = merge_directives(&form.original, &blocks);
    let fresh = match Config::from_text(&merged, &form.path) {
        Ok(c) => c,
        Err(e) => {
            message(hwnd, &format!("配置有误，未保存：\n{}", e), MB_ICONERROR);
            return;
        }
    };
    if let Err(e) = std::fs::write(&form.path, &merged) {
        message(
            hwnd,
            &format!("无法写入 {}：\n{}", form.path.display(), e),
            MB_ICONERROR,
        );
        return;
    }
    form.original = merged;
    crate::log::info(&format!("configuration saved: {}", form.path.display()));

    if fresh.listen == old.listen {
        apply(&form.server, &old, &fresh, &form.path);
        message(hwnd, "配置已保存，新设置已生效。", MB_ICONINFORMATION);
        DestroyWindow(hwnd);
        return;
    }
    // A listener cannot be woken once it is blocked in `accept`, so a port
    // change needs a fresh process: the same handover the tray uses.
    message(
        hwnd,
        "配置已保存。\n监听地址已改变，fern 将重启以使新设置生效。",
        MB_ICONINFORMATION,
    );
    restart(form.owner);
}

/// Turns the controls into the directive blocks the merge owns.
unsafe fn collect(form: &Form, old: &Config) -> Option<Vec<DirectiveBlock>> {
    let root = get_text(form.h_root).trim().to_string();
    if root.is_empty() {
        return fail("共享目录不能为空");
    }
    let listens: Vec<String> = get_text(form.h_listen)
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    if listens.is_empty() {
        return fail("监听地址不能为空，例如 4545 或 0.0.0.0:4545");
    }
    let max_conns = match get_text(form.h_max).trim().parse::<usize>() {
        Ok(n) if n > 0 => n,
        _ => return fail("最大连接数必须是大于 0 的整数"),
    };
    let guest = match parse_perms(&get_text(form.h_guest)) {
        Some(p) => perms_text(p),
        None => return fail("访客权限只能由 RLUMSTD 组成，或填 0"),
    };
    let days = match get_text(form.h_days).trim().parse::<u64>() {
        Ok(n) => n,
        Err(_) => return fail("回收站保留天数必须是整数，0 表示不按时间清理"),
    };
    let level = match SendMessageW(form.h_level, CB_GETCURSEL, 0, 0) {
        CB_ERR => old.log_level,
        idx => LogLevel::from_rank(idx as u8),
    };
    let log_dir = get_text(form.h_logdir).trim().to_string();
    let title = get_text(form.h_title);
    let notice = get_text(form.h_notice);
    let trash = SendMessageW(form.h_trash, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;

    let mut blocks = old.editable_blocks();
    set_block(
        &mut blocks,
        "fern.root",
        vec![format!("Fern.Root {}", quote_value(&root))],
    );
    set_block(
        &mut blocks,
        "fern.listen",
        listens
            .iter()
            .map(|l| format!("Fern.Listen {}", quote_value(l)))
            .collect(),
    );
    set_block(
        &mut blocks,
        "fern.loglevel",
        vec![format!("Fern.LogLevel {}", level.as_str())],
    );
    set_block(
        &mut blocks,
        "fern.maxconns",
        vec![format!("Fern.MaxConns {}", max_conns)],
    );
    set_block(
        &mut blocks,
        "fern.guest",
        vec![format!("Fern.Guest {}", guest)],
    );
    set_block(
        &mut blocks,
        "fern.title",
        vec![format!("Fern.Title {}", quote_value(&title))],
    );
    set_block(
        &mut blocks,
        "fern.notice",
        if notice.trim().is_empty() {
            Vec::new()
        } else {
            vec![format!("Fern.Notice {}", quote_value(&notice))]
        },
    );
    set_block(
        &mut blocks,
        "fern.logdir",
        if log_dir.is_empty() {
            Vec::new()
        } else {
            vec![format!("Fern.LogDir {}", quote_value(&log_dir))]
        },
    );
    set_block(
        &mut blocks,
        "trash.enabled",
        vec![format!("Trash.Enabled {}", if trash { "1" } else { "0" })],
    );
    set_block(
        &mut blocks,
        "trash.max.days",
        vec![format!("Trash.Max.Days {}", days)],
    );
    Some(blocks)
}

fn set_block(blocks: &mut [DirectiveBlock], name: &'static str, lines: Vec<String>) {
    if let Some(slot) = blocks.iter_mut().find(|b| b.0 == name) {
        slot.1 = lines;
    }
}

fn fail(text: &str) -> Option<Vec<DirectiveBlock>> {
    unsafe { message(std::ptr::null_mut(), text, MB_ICONERROR) };
    None
}

/// Copies the values the form owns into the running configuration. Everything
/// else — CLI options, environment overrides, users, chroot, `Trash.Max.Size` —
/// keeps the value it already had.
fn apply(server: &Arc<Server>, old: &Config, fresh: &Config, path: &Path) {
    let mut live = old.clone();
    live.root = fresh.root.clone();
    live.listen = fresh.listen.clone();
    live.guest = fresh.guest;
    live.title = fresh.title.clone();
    live.notice = fresh.notice.clone();
    live.log_dir = fresh.log_dir.clone();
    live.max_conns = fresh.max_conns;
    live.log_level = fresh.log_level;
    live.trash_enabled = fresh.trash_enabled;
    live.trash_max_days = fresh.trash_max_days;
    live.config_path = Some(path.to_path_buf());
    crate::log::init(&live);
    server.reload(live);
    crate::log::info("configuration reloaded");
}

unsafe fn message(hwnd: HWND, text: &str, flags: u32) -> i32 {
    let text = wide(text);
    let caption = wide("fern 配置");
    MessageBoxW(hwnd, text.as_ptr(), caption.as_ptr(), MB_OK | flags)
}

/// Folder picker for the share root.
unsafe fn browse(hwnd: HWND) {
    let Some(form) = form() else { return };
    let mut display = [0u16; MAX_PATH_CHARS];
    let title = wide("选择共享目录");
    // `SHBrowseForFolder` needs the calling thread in an apartment. A nested
    // call returns `S_FALSE` and must not be balanced by `CoUninitialize`.
    let uninit = matches!(
        CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED),
        S_OK | S_FALSE
    );
    let mut info = BrowseInfoW {
        hwnd_owner: hwnd,
        pidl_root: std::ptr::null_mut(),
        display_name: display.as_mut_ptr(),
        title: title.as_ptr(),
        flags: BIF_RETURNONLYFSDIRS | BIF_EDITBOX | BIF_NEWDIALOGSTYLE,
        callback: std::ptr::null_mut(),
        param: 0,
        image: 0,
    };
    let pidl = SHBrowseForFolderW(&mut info);
    if !pidl.is_null() {
        let mut path = [0u16; MAX_PATH_CHARS];
        if SHGetPathFromIDListW(pidl, path.as_mut_ptr()) != 0 {
            let len = path.iter().position(|c| *c == 0).unwrap_or(0);
            if len > 0 {
                set_text(form.h_root, &String::from_utf16_lossy(&path[..len]));
            }
        }
        CoTaskMemFree(pidl);
    }
    if uninit {
        CoUninitialize();
    }
}

/// Hands the process over to a fresh copy, the way the tray "restart" item does.
unsafe fn restart(owner: HWND) -> ! {
    if RESTARTING.swap(true, Ordering::AcqRel) {
        loop {
            std::thread::park();
        }
    }
    crate::log::info("restart requested from the config window");
    if !owner.is_null() {
        crate::platform::tray_win::take_down(owner);
    }
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

/// `fern.conf` next to the executable, used when no file chose the share.
fn default_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("fern.conf")))
        .unwrap_or_else(|| PathBuf::from("fern.conf"))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `MAKEINTRESOURCE`: a resource id smuggled through a pointer argument.
fn id_resource(id: usize) -> LPCWSTR {
    id as LPCWSTR
}
