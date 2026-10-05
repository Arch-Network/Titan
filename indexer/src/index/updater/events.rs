use arch_titan_types_core::Event;
use tokio::sync::mpsc;

use crate::index::updater::transaction::TransactionEventMgr;

pub struct Events {
    events: Vec<Event>,
}

impl Events {
    pub fn new() -> Self {
        Self { events: vec![] }
    }

    pub fn add_event(&mut self, event: Event) {
        self.events.push(event);
    }

    pub fn send_events(
        &mut self,
        event_sender: &Option<mpsc::Sender<Event>>,
    ) -> std::result::Result<(), Box<mpsc::error::SendError<Event>>> {
        if let Some(sender) = event_sender {
            for event in self.events.drain(..) {
                sender.blocking_send(event).map_err(Box::new)?;
            }
        } else {
            self.events.clear();
        }

        Ok(())
    }

    pub fn take_all(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
}

impl TransactionEventMgr for Events {
    fn add_event(&mut self, event: Event) {
        self.add_event(event);
    }
}
