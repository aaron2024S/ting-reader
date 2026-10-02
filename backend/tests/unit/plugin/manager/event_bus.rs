use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn publish_delivers_once_and_unsubscribe_stops_delivery() {
    let bus = DefaultPluginEventBus::new();
    let count = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&count);
    let id = bus
        .subscribe(
            "books.changed",
            Box::new(move |_| {
                observed.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .unwrap();
    bus.publish("books.changed", serde_json::json!({"book": 1}))
        .unwrap();
    bus.publish("books.deleted", Value::Null).unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 1);
    bus.unsubscribe(&id).unwrap();
    bus.publish("books.changed", Value::Null).unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 1);
    assert!(bus.unsubscribe(&id).is_err());
}
