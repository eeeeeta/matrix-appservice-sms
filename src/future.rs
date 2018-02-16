use tokio_core::reactor::{Core, Remote, Handle};
use rocket::State;
use rocket::Rocket;
use huawei_modem::HuaweiModem;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver, self};
use gm::MatrixClient;
use gm::types::messages::Message;
use gm::room::{RoomExt, Room};
use gm::errors::MatrixErrorKind;
use huawei_modem::at::AtResponse;
use huawei_modem::cmd;
use huawei_modem::cmd::sms::{SmsMessage, MessageStatus, DeletionOptions};
use huawei_modem::pdu::{DecodedMessage, PduAddress};
use huawei_modem::errors::HuaweiError;
use futures::{Future, Stream, Poll, Async, self};
use futures::prelude::*;
use diesel::prelude::*;
use failure::{SyncFailure, Error};
use pool::Pool;
use std::sync::Arc;
use std::rc::Rc;
use std::cell::RefCell;
use std::panic;

pub fn normalize_address(addr: &PduAddress) -> String {
    let ton: u8 = addr.type_addr.into();
    let mut ret = format!("{:02X}", ton);
    for b in addr.number.0.iter() {
        ret += &format!("{}", b);
    }
    ret
}
pub struct SendRequest {
    pub message: Message,
    pub room: String,
    pub event_id: String
}
pub enum IntMessage {
    CmglComplete(Vec<SmsMessage>),
    CmglFailed(HuaweiError),
    SendRequest(SendRequest)
}
pub type Mx = Rc<RefCell<MatrixClient>>;
pub struct MessagingFuture {
    modem: HuaweiModem,
    int_tx: UnboundedSender<IntMessage>,
    int_rx: UnboundedReceiver<IntMessage>,
    urc_rx: UnboundedReceiver<AtResponse>,
    handle: Handle,
    db: Pool,
    admin: String,
    hs_localpart: String,
    mx: Mx
}
pub type MessagingHandle<'a> = State<'a, Arc<MessagingData>>;
pub struct MessagingData {
    pub rem: Remote,
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
    let hs_url = rocket.config().get_str("hs_url")
        .expect("'hs_url' in config")
        .to_string();

    let (int_tx, int_rx) = mpsc::unbounded();
    let (rtx, rrx) = ::std::sync::mpsc::channel();
    let itx2 = int_tx.clone();
    ::std::thread::spawn(move || {
        let mut core = Core::new()
            .expect("tokio reactor core");
        let mut modem = HuaweiModem::new_from_path(modem, &core.handle())
            .expect("modem initialization");
        let mx = MatrixClient::new_appservice(
            hs_url.into(),
            format!("@_sms_bot:{}", hs_localpart),
            as_token.into(),
            &core.handle()
        ).expect("matrix client initialization");
        let urc_rx = modem.take_urc_rx().unwrap();
        let mfut = MessagingFuture {
            modem, int_tx, int_rx, urc_rx, db,
            handle: core.handle(),
            admin: admin.into(),
            hs_localpart: hs_localpart.into(),
            mx: Rc::new(RefCell::new(mx))
        };
        rtx.send(core.remote())
            .expect("sending remote");
        error!("MessagingFuture exited: {:?}", core.run(mfut));
        panic!("MessagingFuture exited; the end is nigh!");
    });
    let remote = rrx.recv()
        .expect("receiving remote from futures thread");
    let md = MessagingData {
        rem: remote,
        tx: itx2
    };
    rocket.manage(Arc::new(md))
}
impl MessagingFuture {
    pub fn process_received_message(&self, msg: SmsMessage) -> Box<Future<Item = (), Error = Error>> {
        use models::{Recipient, NewRecipient};
        use gm::types::replies::{RoomCreationOptions, RoomPreset, RoomVisibility};

        let mx = self.mx.clone();
        let hsl = self.hs_localpart.clone();
        let admin = self.admin.clone();

        let addr = normalize_address(&msg.pdu.originating_address);  
        let conn = self.db.get().expect("couldn't get a db connection");
        let recv = {
            use schema::recipients::dsl::*;

            recipients.filter(phone_number.eq(&addr))
                .first::<Recipient>(&*conn)
                .optional()
        };
        let recv = match recv {
            Ok(x) => x,
            Err(e) => return Box::new(futures::future::err(e.into()))
        };
        Box::new(async_block! {
            let room;
            let user_id;
            if let Some(recv) = recv {
                room = Room::from_id(recv.room_id);
                user_id = format!("@{}:{}", recv.user_id, hsl);
                info!("Using existing user {} in room {}", user_id, room.id);
            } else {
                let localpart = format!("_sms_{}", addr);
                let mxid = format!("@{}:{}", localpart, hsl);
                info!("Registering new user {}", mxid);
                if let Err(e) = await!(mx.borrow_mut().as_register_user(localpart.clone())) {
                    let mut good = false;
                    if let &MatrixErrorKind::BadRequest(ref e) = e.kind() {
                        if e.errcode == "M_USER_IN_USE" {
                            // all good!
                            good = true;
                        }
                    }
                    if !good {
                        return Err(SyncFailure::new(e))?;
                    }
                }
                mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
                info!("Creating new room");
                let opts = RoomCreationOptions {
                    preset: Some(RoomPreset::TrustedPrivateChat),
                    is_direct: true,
                    invite: vec![admin, mxid.clone()],
                    room_alias_name: Some(format!("_sms_{}", addr)),
                    name: Some(format!("{} (SMS)", addr)),
                    visibility: Some(RoomVisibility::Private),
                    ..Default::default()
                };
                let rpl = await!(mx.borrow_mut().create_room(opts))
                    .map_err(|e| SyncFailure::new(e))?;
                info!("Joining new room");
                mx.borrow_mut().alter_user_id(mxid.clone());
                await!(mx.borrow_mut().join(&rpl.room.id))
                    .map_err(|e| SyncFailure::new(e))?;
                {
                    use schema::recipients;

                    let new_recipient = NewRecipient {
                        phone_number: &addr,
                        user_id: &localpart,
                        room_id: &rpl.room.id
                    };
                    ::diesel::insert_into(recipients::table)
                        .values(&new_recipient)
                        .execute(&*conn)?;
                }
                user_id = mxid;
                room = rpl.room;
            }
            mx.borrow_mut().alter_user_id(user_id);
            let text = match msg.pdu.get_message_data().decode_message() {
                Ok(DecodedMessage { text, .. }) => text,
                Err(e) => format!("[failed to decode: {}]", e)
            };
            let msg = Message::Text {
                body: text,
                formatted_body: None,
                format: None
            };
            info!("Sending message {:?} to room {}", msg, room.id);
            await!(room.cli(&mut mx.borrow_mut())
                   .send(msg))
                .map_err(|e| SyncFailure::new(e))?;
            Ok(())
        })
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
                    info!("Received +CMTI indication");
                    let tx = self.int_tx.clone();
                    let fut = cmd::sms::list_sms_pdu(&mut self.modem,
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
                            Ok(())
                        });
                    self.handle.spawn(fut);
                }
            }
        }
        while let Async::Ready(x) = self.int_rx.poll().unwrap() {
            let x = x.expect("int_rx stopped producing");
            match x {
                IntMessage::CmglComplete(res) => {
                    info!("+CMGL complete");
                    for msg in res {
                        info!("Processing message: {:?}", msg);
                        if msg.status != MessageStatus::ReceivedUnread {
                            continue;
                        }
                        let fut = self.process_received_message(msg)
                            .map_err(|e| {
                                error!("Failed to process received message: {}", e);
                            });
                        self.handle.spawn(fut);
                    }
                    info!("Deleting read messages");
                    let fut = cmd::sms::del_sms_pdu(&mut self.modem,
                                                    DeletionOptions::DeleteReadAndOutgoing)
                        .map_err(|e| {
                            error!("Failed to delete messages: {}", e);
                        });
                    self.handle.spawn(fut);
                },
                IntMessage::CmglFailed(e) => {
                    error!("Message listing failed: {}", e);
                },
                IntMessage::SendRequest(_) => {}
            }
        }
        Ok(Async::NotReady)
    }
}

