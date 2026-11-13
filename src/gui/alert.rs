use std::cell::{Cell, RefCell};

use async_trait::async_trait;
use slint::{CloseRequestResponse, ComponentHandle};

use crate::i18n::Localizer;
use crate::notify::{Kind, Notification, Notifier, timestamp};

use super::ui::AlertWindow;

/// How alert windows look for one language and font size.
#[derive(Clone, Debug)]
pub struct AlertStyle {
    pub font_family: String,
    pub right_to_left: bool,
    pub ok_text: String,
    /// Body text size in points (as in the configuration file).
    pub font_size: u32,
}

impl AlertStyle {
    pub fn new(localizer: &Localizer, font_size: u32) -> Self {
        AlertStyle {
            font_family: if cfg!(windows) { localizer.font_family().to_owned() } else { String::new() },
            right_to_left: localizer.is_right_to_left(),
            ok_text: localizer.get("alert.ok").to_owned(),
            font_size,
        }
    }
}

thread_local! {
    // Open windows live here until OK is pressed (UI thread only).
    static OPEN: RefCell<Vec<(u64, AlertWindow)>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };
}

/// Number of alert windows on screen. UI thread only.
pub fn open_alerts() -> usize {
    OPEN.with(|o| o.borrow().len())
}

/// Builds an alert window without showing it.
pub fn build_alert(n: &Notification, style: &AlertStyle) -> Result<AlertWindow, slint::PlatformError> {
    let w = AlertWindow::new()?;
    // A right-to-left mark makes the paragraph right-to-left even when it
    // starts with a Latin device name.
    let dir = if style.right_to_left { "\u{200F}" } else { "" };
    w.set_heading(format!("{dir}{}", n.title).into());
    w.set_body(format!("{dir}{}", n.body).into());
    w.set_when(timestamp(n.time).into());
    w.set_ok_text(style.ok_text.as_str().into());
    w.set_rtl(style.right_to_left);
    w.set_font_family(style.font_family.as_str().into());
    // Points to logical pixels.
    w.set_base(style.font_size as f32 * 4.0 / 3.0);
    Ok(w)
}

/// Shows an alert window. UI thread only.
pub fn show_alert(n: &Notification, style: &AlertStyle) -> Result<(), slint::PlatformError> {
    let w = build_alert(n, style)?;
    w.window().on_close_requested(|| CloseRequestResponse::KeepWindowShown);
    let id = NEXT_ID.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    let weak = w.as_weak();
    w.on_ok_clicked(move || {
        if let Some(w) = weak.upgrade() {
            let _ = w.hide();
        }
        // Drop the component outside its own callback.
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            OPEN.with(|o| o.borrow_mut().retain(|(x, _)| *x != id));
        });
    });
    w.show()?;
    super::error_sound();
    OPEN.with(|o| o.borrow_mut().push((id, w)));
    Ok(())
}

/// Opens an alert window for problems and test alerts (not for recoveries).
pub struct AlertWindowNotifier {
    style: AlertStyle,
}

impl AlertWindowNotifier {
    pub fn new(style: AlertStyle) -> Self {
        AlertWindowNotifier { style }
    }
}

#[async_trait]
impl Notifier for AlertWindowNotifier {
    fn name(&self) -> &str {
        "alert window"
    }

    async fn notify(&self, n: &Notification) -> Result<(), String> {
        if n.kind == Kind::Recovery {
            return Ok(());
        }
        let (n, style) = (n.clone(), self.style.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        slint::invoke_from_event_loop(move || {
            let _ = tx.send(show_alert(&n, &style).map_err(|e| e.to_string()));
        })
        .map_err(|e| e.to_string())?;
        rx.await.map_err(|_| "the user interface has stopped".to_owned())?
    }
}
