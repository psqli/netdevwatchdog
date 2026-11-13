//! Notification-area icon with a status menu. UI thread only.

use std::rc::Rc;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use super::ui::Tray as TrayComponent;
use crate::i18n::Localizer;
use crate::monitor::{Health, Snapshot};
use crate::msg;

/// What the user picked in the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    CheckNow,
    EditConfig,
    OpenLog,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shade {
    Neutral,
    Ok,
    Problem,
}

/// A filled circle; the colour shows the overall state at a glance.
pub fn circle(shade: Shade) -> Image {
    const N: u32 = 32;
    let (r, g, b) = match shade {
        Shade::Neutral => (0x60, 0x7d, 0x8b),
        Shade::Ok => (0x2e, 0x7d, 0x32),
        Shade::Problem => (0xc6, 0x28, 0x28),
    };
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(N, N);
    let c = (N as f32 - 1.0) / 2.0;
    for (i, px) in buffer.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((i as u32 % N) as f32, (i as u32 / N) as f32);
        let d = ((x - c).powi(2) + (y - c).powi(2)).sqrt();
        // Anti-aliased edge and a light ring, visible on dark and light taskbars.
        let a = ((c + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
        *px = if d > c - 2.5 { Rgba8Pixel { r: 0xff, g: 0xff, b: 0xff, a } } else { Rgba8Pixel { r, g, b, a } };
    }
    Image::from_rgba8(buffer)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_owned() } else { text.chars().take(max - 1).chain(['…']).collect() }
}

pub fn status_text(l: &Localizer, s: &Snapshot) -> String {
    match s.health {
        Health::Ok => l.get("status.ok").to_owned(),
        Health::Busy => l.get("status.busy").to_owned(),
        Health::Problem => s.failure.map(|f| l.format(&f.title())).unwrap_or_else(|| l.get("status.problem").to_owned()),
        Health::Unknown => l.get("status.unknown").to_owned(),
    }
}

/// Overall state and the one-line summary for the tooltip and menu.
pub fn summary(l: &Localizer, snapshots: &[Snapshot]) -> (Shade, String) {
    let problems = snapshots.iter().filter(|s| s.health == Health::Problem).count();
    let all_checked = !snapshots.is_empty() && snapshots.iter().all(|s| s.health != Health::Unknown);
    if problems > 0 {
        (Shade::Problem, l.format(&msg!("tray.problems", count = problems)))
    } else if all_checked {
        (Shade::Ok, l.get("tray.allOk").to_owned())
    } else {
        (Shade::Neutral, l.get("app.name").to_owned())
    }
}

pub struct Tray {
    component: TrayComponent,
    devices: Rc<VecModel<SharedString>>,
}

impl Tray {
    pub fn new(on_action: impl Fn(Action) + 'static) -> Result<Tray, slint::PlatformError> {
        let component = TrayComponent::new()?;
        let devices = Rc::new(VecModel::default());
        component.set_devices(devices.clone().into());
        let on_action = Rc::new(on_action);
        let f = on_action.clone();
        component.on_check_now(move || f(Action::CheckNow));
        let f = on_action.clone();
        component.on_edit_config(move || f(Action::EditConfig));
        let f = on_action.clone();
        component.on_open_log(move || f(Action::OpenLog));
        component.on_exit(move || on_action(Action::Exit));
        Ok(Tray { component, devices })
    }

    /// Updates the icon, tooltip and menu from the latest round.
    pub fn update(&self, l: &Localizer, snapshots: &[Snapshot]) {
        let c = &self.component;
        let (shade, summary) = summary(l, snapshots);
        c.set_status_icon(circle(shade));
        c.set_hint(truncate(&format!("{}\n{summary}", l.get("app.name")), 127).into());
        c.set_summary(summary.into());
        c.set_check_now_text(l.get("tray.checkNow").into());
        c.set_edit_config_text(l.get("tray.editConfig").into());
        c.set_open_log_text(l.get("tray.openLog").into());
        c.set_exit_text(l.get("tray.exit").into());
        let lines: Vec<SharedString> =
            snapshots.iter().map(|s| l.format(&msg!("tray.deviceLine", device = &s.device, status = status_text(l, s))).into()).collect();
        self.devices.set_vec(lines);
    }

    pub fn show(&self) -> Result<(), slint::PlatformError> {
        self.component.show()
    }
}
