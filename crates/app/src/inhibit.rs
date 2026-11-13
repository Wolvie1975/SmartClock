//! Keep the screen awake while the app runs: org.gnome.SessionManager.Inhibit.
//!
//! The inhibit lasts as long as the D-Bus connection stays open, so a background
//! thread owns the connection for the app's lifetime. Dropping the guard tells the
//! thread to call Uninhibit and exit.

use std::{sync::mpsc, thread, time::Duration};
use tokio::sync::oneshot;
use zbus::{Connection, Proxy};

const INHIBIT_IDLE: u32 = 8;

pub struct IdleInhibit {
    stop: Option<oneshot::Sender<()>>,
    done: mpsc::Receiver<()>,
}

impl IdleInhibit {
    /// Blocks until the inhibit is in place (or fails).
    pub fn acquire() -> anyhow::Result<Self> {
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();

        thread::Builder::new()
            .name("idle-inhibit".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => return drop(ready_tx.send(Err(e.into()))),
                };
                rt.block_on(async move {
                    let held = async {
                        let conn = Connection::session().await?;
                        let proxy = session_manager(&conn).await?;
                        let cookie: u32 = proxy
                            .call(
                                "Inhibit",
                                &("smart-clock", 0u32, "Clock display", INHIBIT_IDLE),
                            )
                            .await?;
                        anyhow::Ok((conn, cookie))
                    }
                    .await;

                    let (conn, cookie) = match held {
                        Ok(v) => v,
                        Err(e) => return drop(ready_tx.send(Err(e))),
                    };
                    tracing::info!(cookie, "GNOME idle inhibited");
                    let _ = ready_tx.send(Ok(()));

                    let _ = stop_rx.await; // sender dropped or signalled → release
                    let res: zbus::Result<()> = async {
                        session_manager(&conn)
                            .await?
                            .call("Uninhibit", &(cookie,))
                            .await
                    }
                    .await;
                    match res {
                        Ok(()) => tracing::info!("GNOME idle inhibit released"),
                        Err(e) => tracing::warn!("uninhibit failed: {e}"),
                    }
                });
                let _ = done_tx.send(());
            })?;

        ready_rx.recv()??;
        Ok(Self {
            stop: Some(stop_tx),
            done: done_rx,
        })
    }
}

async fn session_manager(conn: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new(
        conn,
        "org.gnome.SessionManager",
        "/org/gnome/SessionManager",
        "org.gnome.SessionManager",
    )
    .await
}

impl Drop for IdleInhibit {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // Give Uninhibit a moment; the session manager also drops it when we disconnect.
        let _ = self.done.recv_timeout(Duration::from_secs(1));
    }
}
