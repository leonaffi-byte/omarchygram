//! Deliver every backend event in order without starving GTK during catch-up.
use std::time::{Duration, Instant};

pub(super) async fn dispatch<T>(events: async_channel::Receiver<T>, mut apply: impl FnMut(T)) {
    let mut batch_started = Instant::now();
    while let Ok(event) = events.recv().await {
        apply(event);
        if batch_started.elapsed() >= Duration::from_millis(4) {
            // recv() is immediately ready for a backlog. A short timer gives
            // GTK input, layout and drawing a turn instead of draining it all.
            gtk4::glib::timeout_future(Duration::from_millis(1)).await;
            batch_started = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn queued_updates_preserve_order_and_let_ui_work_run() {
        let context = gtk4::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let (send, receive) = async_channel::unbounded();
                    for event in 0..12 {
                        send.try_send(event).unwrap();
                    }
                    drop(send);
                    let handled = Rc::new(Cell::new(0));
                    let observed = Rc::new(Cell::new(None));
                    let counter = handled.clone();
                    let sample = observed.clone();
                    let observer = context.spawn_local(async move {
                        gtk4::glib::timeout_future(Duration::from_millis(1)).await;
                        sample.set(Some(counter.get()));
                    });
                    dispatch(receive, |event| {
                        assert_eq!(event, handled.get(), "events remain in order");
                        let started = Instant::now();
                        while started.elapsed() < Duration::from_millis(2) {
                            std::hint::spin_loop();
                        }
                        handled.set(event + 1);
                    })
                    .await;
                    observer.await.unwrap();
                    assert_eq!(handled.get(), 12, "no event is dropped");
                    assert!(
                        observed.get().is_some_and(|count| count > 0 && count < 12),
                        "UI work must run before the backlog finishes"
                    );
                })
            })
            .unwrap();
    }
}
