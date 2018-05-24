use futures::*;
use future::Mx;
use store::Store;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use gm::room::Room;
use huawei_modem::pdu::{DeliverPdu, HexData};
use std::time::{Duration, Instant};
use tokio_timer::Delay;
use futures::prelude::*;
use tokio_core::reactor::Handle;
use huawei_modem::convert::TryFrom;
use sms_processor;

pub struct UserHandle {
    pub(crate) ctl: UnboundedSender<UserMessage>
}
pub enum UserMessage {
    ProcessNew,
    ProcessFailed(i32),
    NotifyComplete(i32),
    NotifyFailed(i32, ::failure::Error)
}
pub struct UserManager {
    pub(crate) mx: Mx,
    pub(crate) recipient_id: i32,
    pub(crate) store: Store,
    pub(crate) hdl: Handle,
    pub(crate) room: Room<'static>,
    pub(crate) ctl_tx: UnboundedSender<UserMessage>
}
impl UserManager {
    fn process_message(&self, mid: i32, pdu: DeliverPdu) -> impl Future<Item = (), Error = ::failure::Error> {
        let room = self.room.clone();
        let mx = self.mx.clone();
        sms_processor::process_message(mx, room, mid, pdu)
    }
    fn notify_complete(&mut self, id: i32) -> Result<(), ::failure::Error> {
        debug!("[M-{}] Deleting message", id);
        self.store.delete_message(id)?;
        info!("[M-{}] Message successfully delivered!", id);
        Ok(())
    }
    fn notify_failed(&mut self, id: i32, e: ::failure::Error) -> Result<(), ::failure::Error> {
        warn!("[M-{}] Failed to process message: {}", id, e);
        let failures = self.store.mark_message_failed(id)?;
        let tx = self.ctl_tx.clone();
        let now = Instant::now();
        let secs = 2u64.pow(failures as _);
        let dur = Duration::new(secs, 0);
        info!("[M-{}] Message has {} failures - retrying in {}s", id, failures, secs);
        let fut = Delay::new(now + dur)
            .map(move |_| {
                let _ = tx.unbounded_send(UserMessage::ProcessFailed(id));
            })
            .map_err(move |e| {
                error!("Timer failed: {}", e);
            });
        self.hdl.spawn(fut);
        Ok(())
    }
    fn schedule_message(&mut self, id: i32, pdu: DeliverPdu) -> Result<(), ::failure::Error> {
        let tx = self.ctl_tx.clone();
        let fut = self.process_message(id, pdu)
            .then(move |res| {
                let _ = match res {
                    Ok(_) => tx.unbounded_send(UserMessage::NotifyComplete(id)),
                    Err(e) => tx.unbounded_send(UserMessage::NotifyFailed(id, e))
                };
                Ok(())
            });
        self.hdl.spawn(fut);
        self.store.mark_message_processing(id, true)?;
        Ok(())
    }
    fn process_failed(&mut self, id: i32) -> Result<(), ::failure::Error> {
        info!("[M-{}] Retrying failed message", id);
        let msg = self.store.get_message_by_id(id)?;
        let pdu = DeliverPdu::try_from(&msg.pdu)?;
        self.schedule_message(id, pdu)?;
        Ok(())
    }
    fn process_new(&mut self) -> Result<(), ::failure::Error> {
        let messages = self.store.get_messages_for_recipient(self.recipient_id)?;
        for msg in messages {
            if msg.processing || msg.failures > 0 {
                continue;
            }
            match DeliverPdu::try_from(&msg.pdu) {
                Ok(pdu) => {
                    info!("[M-{}] Scheduling message from {}", msg.id, pdu.originating_address);
                    self.schedule_message(msg.id, pdu)?;
                },
                Err(e) => {
                    error!("[M-{}] Dropping malformed message: {}", msg.id, HexData(&msg.pdu));
                    error!("[M-{}] Parse error was: {}", msg.id, e);
                    self.store.delete_message(msg.id)?;
                }
            }
        }
        Ok(())
    }
    #[async]
    pub fn future(mut self, ctl: UnboundedReceiver<UserMessage>) -> Result<(), ()> {
        #[async]
        for msg in ctl {
            let res = match msg {
                UserMessage::ProcessNew => self.process_new(),
                UserMessage::ProcessFailed(id) => self.process_failed(id),
                UserMessage::NotifyComplete(id) => self.notify_complete(id),
                UserMessage::NotifyFailed(id, e) => self.notify_failed(id, e)
            };
            if let Err(e) = res {
                error!("UserMessage handler failed: {}", e);
            }
        }
        Ok(())
    }
}
