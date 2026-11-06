use super::{EventBus, Received};
use crate::{Event, OperationId, OperationOutcome};

fn finished(op: OperationId) -> Event {
    Event::OperationFinished {
        op,
        outcome: OperationOutcome::Succeeded,
    }
}

#[tokio::test]
async fn delivers_to_every_subscriber() {
    let bus = EventBus::new(4);
    let mut first = bus.subscribe();
    let mut second = bus.clone().subscribe();
    let op = OperationId::random();
    bus.publish(finished(op));
    assert_eq!(first.recv().await, Some(Received::Event(finished(op))));
    assert_eq!(second.recv().await, Some(Received::Event(finished(op))));
}

#[tokio::test]
async fn reports_missed_events_and_closing() {
    let bus = EventBus::new(2);
    let mut slow = bus.subscribe();
    let op = OperationId::random();
    for _ in 0..5 {
        bus.publish(finished(op));
    }
    assert_eq!(slow.recv().await, Some(Received::Missed(3)));
    assert_eq!(slow.recv().await, Some(Received::Event(finished(op))));
    assert_eq!(slow.recv().await, Some(Received::Event(finished(op))));
    drop(bus);
    assert_eq!(slow.recv().await, None);
}

#[test]
fn publishing_without_subscribers_is_fine() {
    EventBus::new(1).publish(finished(OperationId::random()));
}
