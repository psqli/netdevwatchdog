//! Network Device Watchdog: monitors network printers and devices and raises
//! alerts that say what kind of failure it is.

pub mod checks;
pub mod config;
pub mod i18n;
pub mod platform;

/// TLS uses rustls with the `ring` provider; install it once per process.
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
