use tokio_core::reactor::{Core, Remote, Handle};
use rocket::State;
use rocket::Rocket;
use huawei_modem::HuaweiModem;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver, self};
use gm::MatrixClient;
use gm::types::messages::Message;
use gm::types::content::Content;
use gm::types::events::{RoomEventData, Event};
use gm::types::content::room::types::Membership;
use gm::profile::Profile;
use gm::room::{RoomExt, Room, NewRoom};
use huawei_modem::at::AtResponse;
use huawei_modem::cmd;
use huawei_modem::cmd::sms::{SmsMessage, MessageStatus, DeletionOptions};
use huawei_modem::pdu::{PduAddress, Pdu, DeliverPdu, HexData};
use huawei_modem::gsm_encoding::{GsmMessageData};
use huawei_modem::errors::HuaweiError;
use huawei_modem::convert::TryFrom;
use tokio_timer::{Interval, Delay};
use std::time::{Duration, Instant};
use futures::{Future, Stream, Poll, Async};
use futures::prelude::*;
use models::{Recipient, NewMessage};
use recipient_factory::RecipientFactory;
use failure::Error;
use pool::Pool;
use users::{UserHandle, UserMessage};
use std::sync::Arc;
use std::rc::Rc;
use std::cell::RefCell;
use store::Store;
use std::collections::HashMap;
use util;

pub enum IntMessage {
    CmglRequest,
    CmglComplete(Vec<SmsMessage>),
    CmglFailed(HuaweiError),
    MatrixEvent(Event),
    RegisterHandle(PduAddress, UserHandle),
    RegisterFailed(PduAddress, ::failure::Error),
    SendSucceeded(RoomEventData),
    SendFailed(RoomEventData, Error)
}
enum AdminCommand {
    NewConversation(PduAddress),
    GetRegistration,
    GetSignal,
    Help,
    Unrecognized
}
pub type Mx = Rc<RefCell<MatrixClient>>;
pub struct MessagingFuture {
    modem: Rc<RefCell<HuaweiModem>>,
    int_tx: UnboundedSender<IntMessage>,
    int_rx: UnboundedReceiver<IntMessage>,
    urc_rx: UnboundedReceiver<AtResponse>,
    recipients: HashMap<PduAddress, UserHandle>,
    recipients_inflight: HashMap<PduAddress, Vec<i32>>,
    handle: Handle,
    store: Store,
    admin: String,
    hs_localpart: String,
    register_failures: usize,
    mx: Mx
}
pub type MessagingHandle<'a> = State<'a, Arc<MessagingData>>;
pub struct MessagingData {
    pub rem: Remote,
    pub hs_token: String,
    pub tx: UnboundedSender<IntMessage>
}
pub fn attach_tokio(rocket: Rocket) -> Rocket {
    let db = rocket.state::<Pool>()
        .expect("attach the db fairing before the tokio fairing!")
        .clone();
    let modem = rocket.config().get_str("modem_path")
        .expect("'modem_path' in config")
        .to_string();
    let admin = rocket.config().get_str("admin")
        .expect("'admin' in config")
        .to_string();
    let hs_localpart = rocket.config().get_str("hs_localpart")
        .expect("'hs_localpart' in config")
        .to_string();
    let as_token = rocket.config().get_str("as_token")
        .expect("'as_token' in config")
        .to_string();
    let hs_token = rocket.config().get_str("hs_token")
        .expect("'hs_token' in config")
        .to_string();
    let hs_url = rocket.config().get_str("hs_url")
        .expect("'hs_url' in config")
        .to_string();
    let cmgl_secs = rocket.config().get_int("cmgl_secs")
        .unwrap_or(0);

    let (int_tx, int_rx) = mpsc::unbounded();
    let (rtx, rrx) = ::std::sync::mpsc::channel();
    let itx2 = int_tx.clone();
    ::std::thread::spawn(move || {
        let mut core = Core::new()
            .expect("tokio reactor core");
        let mut modem = HuaweiModem::new_from_path(modem, &core.handle())
            .expect("modem initialization");
        let mx = MatrixClient::as_new(
            hs_url.into(),
            format!("@_sms_bot:{}", hs_localpart),
            as_token.into(),
            &core.handle()
        ).expect("matrix client initialization");
        let urc_rx = modem.take_urc_rx().unwrap();

        let fut = cmd::sms::set_sms_textmode(&mut modem, false);
        core.run(fut)
            .expect("setting sms textmode");
        let fut = cmd::sms::set_new_message_indications(&mut modem,
                                                        cmd::sms::NewMessageNotification::SendDirectlyOrBuffer,
                                                        cmd::sms::NewMessageStorage::StoreAndNotify);
        match core.run(fut) {
            Ok(_) => {},
            Err(e) => {
                warn!("Failed setting CNMI settings: {}", e);
                warn!("Your modem may not support the +CNMI setting.");
                if cmgl_secs < 0 {
                    error!("You MUST configure a value for cmgl_secs for the bridge to work properly!");
                    panic!("You need a cmgl_secs value in the config.");
                }
            }
        }
        let tx = int_tx.clone();
        if cmgl_secs > 0 {
            info!("Triggering a CMGL every {} secs", cmgl_secs);
            let fut = Interval::new(Instant::now(), Duration::new(cmgl_secs as _, 0))
                .map_err(|e| {
                    panic!("Timer failed: {}", e);
                })
            .for_each(move |_| {
                tx.unbounded_send(IntMessage::CmglRequest).unwrap();
                Ok(())
            });
            core.handle().spawn(fut);
        }
        let store = Store {
            inner: db
        };
        let mut mfut = MessagingFuture {
            int_tx, int_rx, urc_rx, store,
            modem: Rc::new(RefCell::new(modem)),
            recipients: HashMap::new(),
            recipients_inflight: HashMap::new(),
            handle: core.handle(),
            admin: admin.into(),
            hs_localpart: hs_localpart.into(),
            register_failures: 0,
            mx: Rc::new(RefCell::new(mx))
        };
        rtx.send(core.remote())
            .expect("sending remote");
        mfut.cmgl();
        error!("MessagingFuture exited: {:?}", core.run(mfut));
        panic!("MessagingFuture exited; the end is nigh!");
    });
    let remote = rrx.recv()
        .expect("receiving remote from futures thread");
    let md = MessagingData {
        rem: remote,
        hs_token,
        tx: itx2
    };
    rocket.manage(Arc::new(md))
}
impl MessagingFuture {
    fn cmgl(&mut self) {
        let tx = self.int_tx.clone();
        let fut = cmd::sms::list_sms_pdu(&mut self.modem.borrow_mut(),
                                         MessageStatus::All)
            .then(move |results| {
                match results {
                    Ok(results) => {
                        tx.unbounded_send(
                            IntMessage::CmglComplete(results)).unwrap();
                    },
                    Err(e) => {
                        tx.unbounded_send(
                            IntMessage::CmglFailed(e)).unwrap();
                    }
                }
                let res: Result<(), ()> = Ok(());
                res
            });
        self.handle.spawn(fut);
    }
    fn prod_recipient(&mut self, addr: PduAddress, mid: Option<i32>) {
        if let Some(ids) = self.recipients_inflight.get_mut(&addr) {
            if let Some(mid) = mid {
                ids.push(mid);
            }
            return;
        }
        if let Some(hdl) = self.recipients.get_mut(&addr) {
            hdl.ctl.unbounded_send(UserMessage::ProcessNew).unwrap();
        }
        else {
            let factory = RecipientFactory {
                store: self.store.clone(),
                hdl: self.handle.clone(),
                addr: addr.clone(),
                mx: self.mx.borrow().clone(),
                admin: self.admin.clone(),
                hs_localpart: self.hs_localpart.clone()
            };
            let tx = self.int_tx.clone();
            let mut ids = vec![];
            if let Some(id) = mid {
                ids.push(id);
            }
            self.recipients_inflight.insert(addr.clone(), ids);
            let fut = factory.run()
                .then(move |res| {
                    let _ = match res {
                        Ok(hdl) => tx.unbounded_send(IntMessage::RegisterHandle(addr, hdl)),
                        Err(e) => tx.unbounded_send(IntMessage::RegisterFailed(addr, e))
                    };
                    Ok(())
                });
            self.handle.spawn(fut);
        }
    }
    fn process_invite(&self, room: Room<'static>) -> impl Future<Item = (), Error = Error> {
        let mut mx = self.mx.clone();
        async_block! {
            await!(NewRoom::join(&mut mx, &room.id))?;
            Ok(())
        }
    }
    fn process_admin_command(&mut self, sender: String, room: Room<'static>, text: &str) -> Box<Future<Item = (), Error = Error>> {
        let mut mx = self.mx.clone();
        let modem = self.modem.clone();
        info!("Processing admin command {} from {}", text, sender);
        let text = text.split(" ").collect::<Vec<_>>();
        let cmd = match &text as &[&str] {
            &["!sms", recipient] => {
                AdminCommand::NewConversation(recipient.parse().unwrap())
            },
            &["!help"] => AdminCommand::Help,
            &["!reg"] => AdminCommand::GetRegistration,
            &["!csq"] => AdminCommand::GetSignal,
            _ => AdminCommand::Unrecognized
        };
        match cmd {
            AdminCommand::NewConversation(addr) => {
                info!("Creating new conversation with {}", addr);
                self.prod_recipient(addr, None);
                Box::new(async_block! {
                    Ok(())
                })
            },
            AdminCommand::GetRegistration => {
                info!("Getting registration status");
                Box::new(async_block! {
                    let regst = await!(cmd::network::get_registration(&mut modem.borrow_mut()))?;
                    await!(room.cli(&mut mx).send_simple(format!("Registration status: {}", regst)))?;
                    Ok(())
                })
            },
            AdminCommand::GetSignal => {
                info!("Getting signal status");
                Box::new(async_block! {
                    let sq = await!(cmd::network::get_signal_quality(&mut modem.borrow_mut()))?;
                    await!(room.cli(&mut mx).send_simple(format!("RSSI: {}\nBER: {}", sq.rssi, sq.ber)))?;
                    Ok(())
                })
            },
            AdminCommand::Help => {
                info!("Help requested.");
                Box::new(async_block! {
                    await!(room.cli(&mut mx).send_simple(r#"This is an instance of matrix-appservice-sms (https://github.com/eeeeeta/matrix-appservice-sms/).
Currently supported commands:
- !sms <num>: start or resume an SMS conversation with a given phone number
- !csq: check signal quality
- !reg: get the modem's network registration status
- !help: display this text"#))?;
                    Ok(())
                })
            }
            AdminCommand::Unrecognized => {
                info!("Unrecognized admin command.");
                Box::new(async_block! {
                    await!(room.cli(&mut mx).send_simple("Unrecognized command. Try !help for more information."))?;
                    Ok(())
                })
            }
        }
    }
    fn process_sending_message(&self, recv: Recipient, sender: String, rm: Message) -> impl Future<Item = (), Error = Error> {
        let mut mx = self.mx.clone();
        let modem = self.modem.clone();

        async_block! {
            let num = util::un_normalize_address(&recv.phone_number)
                .ok_or(format_err!("Invalid address in database - this shouldn't really ever happen"))?;
            let text = match rm {
                Message::Text { body, .. } => body,
                Message::Notice { body, .. } => body,
                Message::Image { body, url, .. } => format!("[Image '{}' - download at {}]", body, url),
                Message::File { body, url, .. } => format!("[File '{}' - download at {}]", body, url),
                Message::Location { body, geo_uri } => format!("[Location {} - geo URI {}]", body, geo_uri),
                Message::Audio { body, url, .. } => format!("[Audio file '{}' - download at {}]", body, url),
                Message::Video { body, url, .. } => format!("[Video file '{}' - download at {}]", body, url),
                Message::Emote { body } => {
                    let disp = await!(Profile::get_displayname(&mut mx, &sender))?;
                    format!("* {} {}", disp.displayname, body)
                }
            };
            info!("Sending message to {}...", num);
            debug!("Message content: \"{}\"", text);
            let data = GsmMessageData::encode_message(&text);
            if data.len() > 3 {
                Err(format_err!("Message is greater than 3 SMSes in length; refusing to transmit such a long message."))?;
            }
            let parts = data.len();
            for (i, part) in data.into_iter().enumerate() {
                info!("Sending part {}/{}...", i+1, parts);
                let pdu = Pdu::make_simple_message(num.clone(), part);
                debug!("PDU: {:?}", pdu);
                await!(cmd::sms::send_sms_pdu(&mut modem.borrow_mut(), &pdu))?;
            }
            info!("Sent!");
            Ok(())
        }
    }
}
impl Future for MessagingFuture {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> {
        while let Async::Ready(x) = self.urc_rx.poll().unwrap() {
            let x = x.expect("urc_rx stopped producing");
            if let AtResponse::InformationResponse { param, .. } = x {
                if param == "+CMTI" {
                    debug!("Received +CMTI indication");
                    self.cmgl();
                }
            }
        }
        while let Async::Ready(x) = self.int_rx.poll().unwrap() {
            let x = x.expect("int_rx stopped producing");
            match x {
                IntMessage::CmglRequest => {
                    self.cmgl();
                },
                IntMessage::CmglComplete(res) => {
                    debug!("+CMGL complete");
                    for msg in res {
                        debug!("Processing message: {:?}", msg);
                        if msg.status != MessageStatus::ReceivedUnread {
                            continue;
                        }
                        let recv = self.store.get_recipient_for_address(&msg.pdu.originating_address)?;
                        if let Some(recv) = recv {
                            let nm = NewMessage {
                                recipient_id: Some(recv.id),
                                pdu: &msg.raw_pdu
                            };
                            let m = self.store.create_message(nm)?;
                            info!("[M-{}] Message created from {} with recipient", m.id, msg.pdu.originating_address);
                            self.prod_recipient(msg.pdu.originating_address, None);
                        }
                        else {
                            let nm = NewMessage {
                                recipient_id: None,
                                pdu: &msg.raw_pdu
                            };
                            let m = self.store.create_message(nm)?;
                            info!("[M-{}] Message created from {} WITHOUT recipient", m.id, msg.pdu.originating_address);
                            self.prod_recipient(msg.pdu.originating_address, Some(m.id));
                        }
                    }
                    let recips = self.store.get_recipients_with_messages()?;
                    for recip in recips {
                        let recip = self.store.get_recipient_by_id(recip)?;
                        let num = util::un_normalize_address(&recip.phone_number)
                            .ok_or(format_err!("Invalid address in database - this shouldn't really ever happen"))?;
                        info!("[C-{}] Prodding recipient for {}", recip.id, num);
                        self.prod_recipient(num, None);
                    }
                    let orphaned = self.store.get_orphaned_messages()?;
                    for orphan in orphaned {
                        info!("[M-{}] Recovering orphan", orphan.id);
                        match DeliverPdu::try_from(&orphan.pdu) {
                            Ok(pdu) => {
                                info!("[M-{}] Prodding recipient {} for orphan", orphan.id, pdu.originating_address);
                                self.prod_recipient(pdu.originating_address, Some(orphan.id));
                            },
                            Err(e) => {
                                error!("[M-{}] Dropping malformed message: {}", orphan.id, HexData(&orphan.pdu));
                                error!("[M-{}] Parse error was: {}", orphan.id, e);
                                self.store.delete_message(orphan.id)?;
                            }
                        }
                    }
                    let fut = cmd::sms::del_sms_pdu(&mut self.modem.borrow_mut(),
                                                    DeletionOptions::DeleteReadAndOutgoing)
                        .map_err(|e| {
                            error!("Failed to delete messages: {}", e);
                        });
                    self.handle.spawn(fut);
                },
                IntMessage::RegisterHandle(addr, hdl) => {
                    self.register_failures += 0;
                    let recv = self.store.get_recipient_for_address(&addr)?.unwrap();
                    info!("[C-{}] Recipient registered for {}", recv.id, addr);
                    self.recipients.insert(addr.clone(), hdl);
                    let mids = self.recipients_inflight.remove(&addr).unwrap();
                    info!("[C-{}] Parenting messages {:?} to recipient", recv.id, mids);
                    self.store.set_recipient_for_messages(recv.id, &mids)?;
                    self.prod_recipient(addr.clone(), None);
                },
                IntMessage::RegisterFailed(addr, e) => {
                    self.recipients_inflight.remove(&addr);
                    warn!("Creating recipient failed: {}", e);
                    let tx = self.int_tx.clone();
                    let now = Instant::now();
                    self.register_failures += 1;
                    let secs = 2u64.pow(self.register_failures as _);
                    let dur = Duration::new(secs, 0);
                    warn!("{} failure(s) to register - retrying in {} secs", self.register_failures, secs);
                    let fut = Delay::new(now + dur)
                        .map(move |_| {
                            let _ = tx.unbounded_send(IntMessage::CmglRequest);
                        })
                        .map_err(move |e| {
                            panic!("Timer failed: {}", e);
                        });
                    self.handle.spawn(fut);
                },
                IntMessage::CmglFailed(e) => {
                    error!("Message listing failed: {}", e);
                },
                IntMessage::SendSucceeded(rd) => {
                    let mut mx = self.mx.clone();
                    let fut = async_block! {
                        let res = await!(rd.room.as_ref().unwrap().cli(&mut mx).read_receipt(&rd.event_id));
                        if let Err(e) = res {
                            warn!("Error sending read receipt: {}", e);
                        }
                        else {
                            debug!("Sent read receipt.");
                        }
                        let res: Result<(), ()> = Ok(());
                        res
                    };
                    self.handle.spawn(fut);
                },
                IntMessage::SendFailed(rd, e) => {
                    let mut mx = self.mx.clone();
                    let fut = async_block! {
                        warn!("Error sending message: {}", e);
                        let disp = await!(Profile::get_displayname(&mut mx, &rd.sender));
                        if let Err(e) = disp {
                            error!("Error sending 'error sending message' message (getting displayname): {}", e);
                            let res: Result<(), ()> = Ok(());
                            return res;
                        }
                        let disp = disp.unwrap();
                        let message = Message::Text {
                            body: format!("{}: error sending message: {}", disp.displayname, e),
                            format: Some("org.matrix.custom.html".into()),
                            formatted_body: Some(format!("<a href=\"https://matrix.to/#/{}\">{}</a>: error sending message: <pre>{}</pre>",
                                                         rd.sender,
                                                         disp.displayname,
                                                         e))
                        };
                        let res = await!(rd.room.as_ref().unwrap().cli(&mut mx).send(message));
                        if let Err(e) = res {
                            error!("Error sending 'error sending message' message: {}", e);
                        }
                        else {
                            debug!("Sent error notif.");
                        }
                        Ok(())
                    };
                    self.handle.spawn(fut);
                },
                IntMessage::MatrixEvent(evt) => {
                    let rd = match evt.room_data {
                        Some(ref rd) => rd,
                        None => {
                            error!("Sent event with no room data!");
                            continue;
                        }
                    };
                    let room = match rd.room {
                        Some(ref r) => r,
                        None => {
                            error!("Sent event with no room!");
                            continue;
                        }
                    };
                    if rd.sender.starts_with("@_sms") {
                        continue;
                    }
                    match evt.content {
                        Content::RoomMember(m) => {
                            if let Some(ref sd) = evt.state_data {
                                if let Membership::Invite = m.membership {
                                    if sd.state_key == format!("@_sms_bot:{}", self.hs_localpart) {
                                        info!("Invited by {} to {}", rd.sender, room.id);
                                        let fut = self.process_invite(room.clone())
                                            .map_err(|e| error!("Failed to process invite: {}", e));
                                        self.handle.spawn(fut);
                                    }
                                }
                            }
                        },
                        Content::RoomMessage(m) => {
                            let recv = self.store.get_recipient_for_room(&room.id)?;
                            let rd = rd.clone();
                            if let Some(recv) = recv {
                                let tx = self.int_tx.clone();
                                debug!("Sending message from {}: {:?}", rd.sender, m);
                                let fut = self.process_sending_message(recv, rd.sender.clone(), m)
                                    .then(move |res| {
                                        let res = match res {
                                            Ok(_) => tx.unbounded_send(IntMessage::SendSucceeded(rd)),
                                            Err(e) => tx.unbounded_send(IntMessage::SendFailed(rd, e))
                                        };
                                        res.unwrap();
                                        Ok(())
                                    });
                                self.handle.spawn(fut);
                            }
                            else {
                                if let Message::Text { ref body, .. } = m {
                                    if rd.sender == self.admin && body.starts_with("!") {
                                        info!("Processing admin message: {}", body);
                                        let fut = self.process_admin_command(rd.sender, room.clone(), body)
                                            .map_err(|e| error!("Failed to process admin message: {}", e));
                                        self.handle.spawn(fut);
                                        continue;
                                    }
                                }
                                warn!("No recipient for event {}: {:?}", rd.event_id, m);
                            }
                        },
                        x => debug!("Discarding event: {:?}", x)
                    }
                }
            }
        }
        Ok(Async::NotReady)
    }
}

