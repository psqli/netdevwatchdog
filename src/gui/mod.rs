//! Desktop user interface: the alert window (every platform), desktop
//! notifications and the notification-area icon.

mod alert;
mod desktop;
pub mod tray;

mod ui {
    slint::include_modules!();
}

pub use alert::{AlertStyle, AlertWindowNotifier, build_alert, open_alerts, show_alert};
pub use desktop::DesktopNotifier;
pub use ui::AlertWindow;

/// Plays the system error sound.
pub fn error_sound() {
    #[cfg(windows)]
    // SAFETY: MessageBeep has no preconditions.
    unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::MessageBeep(windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONHAND);
    }
}
