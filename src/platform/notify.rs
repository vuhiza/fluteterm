//! Notification Center banners. Clicking a banner drops the panel into the tab
//! that produced it.

use std::sync::Once;

use mac_notification_sys::{Notification, NotificationResponse};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

static INIT: Once = Once::new();

pub fn send(title: String, subtitle: Option<String>, body: String, sound: bool, tab: Option<u64>, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        INIT.call_once(|| {
            // Unbundled binaries borrow Terminal's identity so banners are allowed;
            // the .app bundle (scripts/bundle.sh) uses its own.
            let id = super::bundle_id().unwrap_or_else(|| "com.apple.Terminal".into());
            let _ = mac_notification_sys::set_application(&id);
        });
        let mut n = Notification::new();
        n.title(&title).message(&body).maybe_subtitle(subtitle.as_deref()).wait_for_click(true);
        if sound {
            n.default_sound();
        }
        if let Ok(NotificationResponse::Click) = n.send() {
            let _ = proxy.send_event(UserEvent::NotificationClicked(tab));
        }
    });
}
