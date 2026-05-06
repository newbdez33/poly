use crate::domain::AppEvent;
use crossterm::event::{self, Event};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Reads crossterm events on a blocking thread and forwards them as `AppEvent::Key`.
/// Exits on shutdown.
pub async fn run(tx: mpsc::Sender<AppEvent>, shutdown: CancellationToken) {
    let tx_clone = tx.clone();
    let shutdown_clone = shutdown.clone();

    tokio::task::spawn_blocking(move || {
        loop {
            if shutdown_clone.is_cancelled() { break; }
            // poll with short timeout so we can observe shutdown
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => {
                    match event::read() {
                        Ok(Event::Key(k)) => {
                            if tx_clone.blocking_send(AppEvent::Key(k)).is_err() { break; }
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                Ok(false) => {}
                Err(_) => break,
            }
        }
    }).await.ok();
}

#[cfg(test)]
mod tests {
    // Crossterm input is hard to unit-test because it reads from stdin TTY state.
    // Coverage is provided by the BDD step "I press 'q'" which constructs KeyEvent
    // values directly (bypassing crossterm), and by the e2e quit-key scenario.
}
