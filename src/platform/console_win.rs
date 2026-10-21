//! Console handling for the windowless release build.
//!
//! The release binary links as a GUI-subsystem program so double clicking it
//! never opens a console window. Started from a terminal instead, the caller's
//! console has to be adopted by hand: without that `--help`, `--check` and the
//! log lines would be written into nothing.

#![allow(non_snake_case)]

use std::ffi::c_void;

type HANDLE = *mut c_void;
type HWND = *mut c_void;
type LPCWSTR = *const u16;

const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const STD_ERROR_HANDLE: u32 = -12i32 as u32;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
const INVALID_HANDLE_VALUE: isize = -1;

/// `CREATE_NO_WINDOW`, for helper processes spawned without a console.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const MB_OK: u32 = 0x0000_0000;
const MB_ICONERROR: u32 = 0x0000_0010;

#[link(name = "kernel32")]
extern "system" {
    fn AttachConsole(process_id: u32) -> i32;
    fn GetConsoleWindow() -> HWND;
    fn GetStdHandle(which: u32) -> HANDLE;
    fn SetStdHandle(which: u32, handle: HANDLE) -> i32;
    fn CreateFileW(
        name: LPCWSTR,
        access: u32,
        share: u32,
        security: *const c_void,
        creation: u32,
        flags: u32,
        template: HANDLE,
    ) -> HANDLE;
}

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(window: HWND, text: LPCWSTR, caption: LPCWSTR, flags: u32) -> i32;
}

fn usable(handle: HANDLE) -> bool {
    !handle.is_null() && handle as isize != INVALID_HANDLE_VALUE
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Points stdout/stderr at the caller's console when there is one. A process
/// started from Explorer keeps no console at all, which is the point.
pub fn attach() {
    unsafe {
        if !GetConsoleWindow().is_null() {
            return; // console build, or a console was already inherited
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return; // launched from the shell: stay windowless
        }
        let name = wide("CONOUT$");
        let mut console: HANDLE = std::ptr::null_mut();
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if usable(GetStdHandle(which)) {
                continue; // keep the pipes and files the caller redirected
            }
            if !usable(console) {
                console = CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                );
            }
            if usable(console) {
                SetStdHandle(which, console);
            }
        }
    }
}

/// Reports a fatal startup error. Without a console the message would be lost,
/// so it falls back to a message box (a second instance failing to bind the
/// port is the common case).
pub fn report(message: &str) {
    let text = format!("fern: {}", message);
    eprintln!("{}", text);
    unsafe {
        if !GetConsoleWindow().is_null() {
            return;
        }
        let body = wide(&text);
        let caption = wide("fern");
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
