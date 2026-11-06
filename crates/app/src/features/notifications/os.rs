//! System toasts, click → back to the terminal. Windows: notification center. Linux: the
//! desktop's notification daemon (freedesktop D-Bus). Elsewhere: nothing, the in-app toasts
//! are enough.

use std::sync::mpsc::Sender;

use crate::shell::world::CardId;

pub struct OsToaster {
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    ctx: egui::Context,
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    clicked: Sender<CardId>,
    enabled: bool,
}

impl OsToaster {
    pub fn new(ctx: egui::Context, clicked: Sender<CardId>, enabled: bool) -> Self {
        Self { ctx, clicked, enabled }
    }

    pub fn show(&self, title: &str, body: &str, card: &CardId) {
        if !self.enabled {
            return;
        }
        #[cfg(windows)]
        {
            use tauri_winrt_notification::Toast;
            let (ctx, tx, card) = (self.ctx.clone(), self.clicked.clone(), card.clone());
            let result = Toast::new(Toast::POWERSHELL_APP_ID)
                .title(title)
                .text1(body)
                .on_activated(move |_| {
                    let _ = tx.send(card.clone());
                    ctx.request_repaint();
                    Ok(())
                })
                .show();
            if let Err(e) = result {
                eprintln!("Windows toast failed: {e}");
            }
        }
        #[cfg(target_os = "linux")]
        {
            // The language is per thread: read the label here.
            let (title, body, label) = (title.to_owned(), body.to_owned(), crate::shell::i18n::t().show_terminal);
            let (ctx, tx, card) = (self.ctx.clone(), self.clicked.clone(), card.clone());
            // Waiting for the click blocks: one short-lived thread per notification.
            std::thread::spawn(move || {
                let shown = notify_rust::Notification::new().appname("tcanvas").summary(&title).body(&body).action("default", label).show();
                match shown {
                    Ok(handle) => handle.wait_for_action(|action| {
                        if action == "default" {
                            let _ = tx.send(card);
                            ctx.request_repaint();
                        }
                    }),
                    Err(e) => eprintln!("desktop notification failed: {e}"),
                }
            });
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = (title, body, card);
    }
}
