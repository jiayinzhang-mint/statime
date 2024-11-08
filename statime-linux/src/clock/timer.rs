use std::{future::Future, pin::Pin};
use tokio::time::Sleep;

pin_project_lite::pin_project! {
    pub struct Timer {
        #[pin]
        timer: Sleep,
        running: bool,
    }
}
pub struct Timers<'a> {
    pub port_sync_timer: Pin<&'a mut Timer>,
    pub port_announce_timer: Pin<&'a mut Timer>,
    pub port_announce_timeout_timer: Pin<&'a mut Timer>,
    pub delay_request_timer: Pin<&'a mut Timer>,
    pub filter_update_timer: Pin<&'a mut Timer>,
}

impl Timer {
    pub fn new() -> Self {
        Timer {
            timer: tokio::time::sleep(std::time::Duration::from_secs(0)),
            running: false,
        }
    }

    pub fn reset(self: Pin<&mut Self>, duration: std::time::Duration) {
        let this = self.project();
        this.timer.reset(tokio::time::Instant::now() + duration);
        *this.running = true;
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        if *this.running {
            let result = this.timer.poll(cx);
            if result != std::task::Poll::Pending {
                *this.running = false;
            }
            result
        } else {
            std::task::Poll::Pending
        }
    }
}
