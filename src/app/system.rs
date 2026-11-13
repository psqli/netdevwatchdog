//! Console and message boxes (Windows); no-ops elsewhere.

/// The Windows GUI build starts without a console. Attach to the parent's
/// console (cmd, PowerShell) so command-line output is visible.
pub fn attach_console() {
    #[cfg(all(windows, feature = "gui"))]
    // SAFETY: plain Win32 calls without pointers.
    unsafe {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE};
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        // Output redirected to a pipe or file: nothing to attach.
        if out.is_null() || out as isize == -1 {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Whether error output goes somewhere: a console, pipe or file. A Windows GUI
/// program started from the Run key or Explorer has none.
#[cfg(windows)]
fn has_stderr() -> bool {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    // SAFETY: GetStdHandle has no preconditions.
    let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    !handle.is_null() && handle as isize != -1
}

/// A message box, only when there is no console to print to (so scripts and
/// tests never wait for someone to press OK).
#[cfg(windows)]
fn message_box(text: &str, style: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW};
    if has_stderr() {
        return;
    }
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, caption) = (wide(text), wide("Network Device Watchdog"));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call.
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_OK | MB_TOPMOST | MB_SETFOREGROUND | style);
    }
}

/// Shows a startup error where there may be no console to print it to.
pub fn error_dialog(text: &str) {
    #[cfg(windows)]
    message_box(text, windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR);
    #[cfg(not(windows))]
    let _ = text;
}

#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn info_dialog(text: &str) {
    #[cfg(windows)]
    message_box(text, windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION);
    #[cfg(not(windows))]
    let _ = text;
}

/// Opens a file in a text editor or a folder in the file manager.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn open(path: &std::path::Path, editor: bool) {
    let program = if cfg!(windows) {
        if editor { "notepad.exe" } else { "explorer.exe" }
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(path).spawn();
}
