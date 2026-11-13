use async_trait::async_trait;

use crate::notify::{Notification, Notifier};

/// Desktop notification: a Windows toast, or the freedesktop notification
/// service on Linux.
pub struct DesktopNotifier {
    pub app_name: String,
}

#[async_trait]
impl Notifier for DesktopNotifier {
    fn name(&self) -> &str {
        "desktop notification"
    }

    async fn notify(&self, n: &Notification) -> Result<(), String> {
        let (title, body, app) = (n.title.clone(), n.body.clone(), self.app_name.clone());
        tokio::task::spawn_blocking(move || {
            let mut toast = notify_rust::Notification::new();
            toast.summary(&title).body(&body).appname(&app);
            #[cfg(all(unix, not(target_os = "macos")))]
            toast.timeout(notify_rust::Timeout::Milliseconds(15_000));
            toast.show().map(|_| ()).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}
