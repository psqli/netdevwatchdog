//! Renders the alert window offscreen in every built-in language and checks
//! the layout. The PNGs are left in the target folder for a visual review.
#![cfg(feature = "gui")]

use std::rc::Rc;
use std::time::SystemTime;

use netdevwatchdog::checks::FailureCode;
use netdevwatchdog::gui::{AlertStyle, build_alert};
use netdevwatchdog::i18n::{Locale, Localizer, Msg};
use netdevwatchdog::msg;
use netdevwatchdog::notify::{Kind, Notification};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, Rgb8Pixel};

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

const W: u32 = 1000;
const H: u32 = 520;

fn notification(l: &Localizer) -> Notification {
    let failure = FailureCode::DeviceError;
    Notification {
        kind: Kind::Problem,
        device: "Caixa 1".into(),
        host: "192.168.10.21".into(),
        failure: Some(failure),
        title: l.format(&msg!("alert.title.problem", device = "Caixa 1", failure = failure.title())),
        body: l.format(&msg!("msg.deviceError", reasons = vec![Msg::key("reason.paperEnd"), Msg::key("reason.coverOpen")])),
        time: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_790_000_000),
    }
}

fn save_png(path: &std::path::Path, pixels: &[Rgb8Pixel]) {
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), W, H);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    encoder.write_header().unwrap().write_image_data(&bytes).unwrap();
}

/// Mean x of the icon's red pixels (#c62828).
fn icon_x(pixels: &[Rgb8Pixel]) -> Option<f64> {
    let xs: Vec<u32> = pixels
        .iter()
        .enumerate()
        .filter(|(_, p)| p.r.abs_diff(0xc6) < 6 && p.g.abs_diff(0x28) < 6 && p.b.abs_diff(0x28) < 6)
        .map(|(i, _)| i as u32 % W)
        .collect();
    (!xs.is_empty()).then(|| xs.iter().map(|&x| x as f64).sum::<f64>() / xs.len() as f64)
}

#[test]
fn alert_window_renders_in_every_language() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone()))).unwrap();
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));

    for code in Locale::built_in_codes() {
        let l = Localizer::new(Some(code), None);
        let mut style = AlertStyle::new(&l, 18);
        // Fonts that exist on Linux CI; on Windows the locale's own font is used.
        style.font_family = String::new();
        let alert = build_alert(&notification(&l), &style).unwrap();
        alert.show().unwrap();
        window.set_size(PhysicalSize::new(W, H));
        let mut pixels = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (W * H) as usize];
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, W as usize);
        });
        save_png(&out.join(format!("alert-{code}.png")), &pixels);

        let dark = pixels.iter().filter(|p| p.r < 100 && p.g < 100 && p.b < 100).count();
        assert!(dark > 300, "{code}: no text rendered");
        let x = icon_x(&pixels).unwrap_or_else(|| panic!("{code}: no error icon"));
        if l.is_right_to_left() {
            assert!(x > W as f64 * 0.75, "{code}: icon should be on the right, mean x {x}");
        } else {
            assert!(x < W as f64 * 0.25, "{code}: icon should be on the left, mean x {x}");
        }
        alert.hide().unwrap();
    }
}
