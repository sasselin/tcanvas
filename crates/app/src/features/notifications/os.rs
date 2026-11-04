//! Toasts du système. Windows : centre de notifications, clic → retour sur le terminal.
//! Ailleurs (développement sous Linux) : rien, les toasts de l'application suffisent.

use std::sync::mpsc::Sender;

use crate::shell::world::CardId;

pub struct OsToaster {
    #[cfg_attr(not(windows), allow(dead_code))]
    ctx: egui::Context,
    #[cfg_attr(not(windows), allow(dead_code))]
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
        #[cfg(not(windows))]
        let _ = (title, body, card);
    }
}
