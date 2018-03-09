use tokio_core::reactor::{Core, Remote, Handle};
use rocket::State;
use rocket::Rocket;
use huawei_modem::HuaweiModem;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver, self};
use gm::MatrixClient;
use gm::types::messages::Message;
use gm::types::events::MetaFull;
use gm::types::content::Content;
use gm::types::content::room::types::Membership;
use gm::types::content::room::PowerLevels;
use gm::room::{RoomExt, Room};
use gm::errors::MatrixError;
use huawei_modem::at::AtResponse;
use huawei_modem::cmd;
use huawei_modem::cmd::sms::{SmsMessage, MessageStatus, DeletionOptions};
use huawei_modem::pdu::{PduAddress, AddressType, Pdu};
use huawei_modem::gsm_encoding::{DecodedMessage, GsmMessageData};
use huawei_modem::errors::HuaweiError;
use futures::{Future, Stream, Poll, Async};
use futures::prelude::*;
use diesel::prelude::*;
use models::{Recipient, NewRecipient};
use failure::Error;
use pool::Pool;
use std::sync::Arc;
use std::rc::Rc;
use std::cell::RefCell;
use std::convert::TryFrom;

pub fn normalize_address(addr: &PduAddress) -> String {
    let ton: u8 = addr.type_addr.into();
    let mut ret = format!("{:02X}", ton);
    for b in addr.number.0.iter() {
        ret += &format!("{}", b);
    }
    ret
}
pub fn un_normalize_address(addr: &str) -> Option<PduAddress> {
    if addr.len() < 3 {
        return None;
    }
    let toa = u8::from_str_radix(&addr[0..2], 16).ok()?;
    let toa = AddressType::try_from(toa).ok()?;
    let mut addr: PduAddress = addr.parse().unwrap();
    addr.number.0.remove(0);
    addr.number.0.remove(0);
    addr.type_addr = toa;
    Some(addr)
}

pub enum IntMessage {
    CmglComplete(Vec<SmsMessage>),
    CmglFailed(HuaweiError),
    MatrixEvent(MetaFull, Content)
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
    handle: Handle,
    db: Pool,
    admin: String,
    hs_localpart: String,
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

        let fut = cmd::sms::set_sms_textmode(&mut modem, false);
        core.run(fut)
            .expect("setting sms textmode");
        let fut = cmd::sms::set_new_message_indications(&mut modem,
                                                        cmd::sms::NewMessageNotification::SendDirectlyOrBuffer,
                                                        cmd::sms::NewMessageStorage::StoreAndNotify);
        core.run(fut)
            .expect("setting sms new message indications");
        let mfut = MessagingFuture {
            int_tx, int_rx, urc_rx, db,
            modem: Rc::new(RefCell::new(modem)),
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
        hs_token,
        tx: itx2
    };
    rocket.manage(Arc::new(md))
}
struct UserAndRoomDetails {
    user_id: String,
    room: Room<'static>
}
impl MessagingFuture {
    fn get_user_and_room(&self, addr_orig: PduAddress) -> impl Future<Item = UserAndRoomDetails, Error = Error> {
        let details = self.get_user_and_room_simple(addr_orig);
        let mx = self.mx.clone();
        let admin = self.admin.clone();

        async_block! {
            let UserAndRoomDetails { user_id, room } = await!(details)?;
            mx.borrow_mut().alter_user_id(user_id.clone());
            let jm = await!(room.cli(&mut mx.borrow_mut())
                            .get_joined_members())?;
            if jm.joined.get(&admin).is_none() {
                mx.borrow_mut().alter_user_id(user_id.clone());
                if let Err(e) = await!(room.cli(&mut mx.borrow_mut())
                                       .invite_user(&admin)) {
                    error!("Error inviting user {} to room {}: {}", admin, room.id, e);
                }
            }
            let pl = await!(room.cli(&mut mx.borrow_mut())
                            .get_user_power_level(admin.clone()))?;
            if pl < 100 {
                mx.borrow_mut().alter_user_id(user_id.clone());
                if let Ok(mut pl) = await!(room.cli(&mut mx.borrow_mut())
                                       .get_typed_state::<PowerLevels>("m.room.power_levels", None)) {
                    pl.users.insert(admin.clone(), 100);
                    mx.borrow_mut().alter_user_id(user_id.clone());
                    if let Err(e) = await!(room.cli(&mut mx.borrow_mut())
                                           .set_typed_state::<PowerLevels>("m.room.power_levels", None, pl)) {
                        error!("Error setting powerlevels of user {} in room {}: {}", admin, room.id, e);
                    }
                }
            }
            Ok(UserAndRoomDetails { user_id, room })
        }

    }
    fn get_user_and_room_simple(&self, addr_orig: PduAddress) -> impl Future<Item = UserAndRoomDetails, Error = Error> {
        use gm::types::replies::{RoomCreationOptions, RoomPreset, RoomVisibility};

        let mx = self.mx.clone();
        let hsl = self.hs_localpart.clone();

        let addr = normalize_address(&addr_orig);
        let conn = self.db.get().expect("couldn't get a db connection");
        let recv = {
            use schema::recipients::dsl::*;

            recipients.filter(phone_number.eq(&addr))
                .first::<Recipient>(&*conn)
                .optional()
        };
        async_block! {
            if let Some(recv) = recv? {
                Ok(UserAndRoomDetails {
                    user_id: format!("@{}:{}", recv.user_id, hsl),
                    room: Room::from_id(recv.room_id)
                })
            } else {
                let localpart = format!("_sms_{}", addr);
                let mxid = format!("@{}:{}", localpart, hsl);
                info!("Registering new user {}", mxid);
                if let Err(e) = await!(mx.borrow_mut().as_register_user(localpart.clone())) {
                    let mut good = false;
                    if let MatrixError::BadRequest(ref e) = e {
                        if e.errcode == "M_USER_IN_USE" {
                            // probably already registered it
                            good = true;
                        }
                    }
                    if !good {
                        return Err(e)?;
                    }
                }
                mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
                info!("Creating new room");
                let opts = RoomCreationOptions {
                    preset: Some(RoomPreset::TrustedPrivateChat),
                    is_direct: true,
                    invite: vec![mxid.clone()],
                    room_alias_name: Some(format!("_sms_{}", addr)),
                    name: Some(format!("{} (SMS)", addr_orig)),
                    visibility: Some(RoomVisibility::Private),
                    ..Default::default()
                };
                let rpl = await!(mx.borrow_mut().create_room(opts))?;
                info!("Joining new room");
                mx.borrow_mut().alter_user_id(mxid.clone());
                await!(mx.borrow_mut().join(&rpl.room.id))?;
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
                Ok(UserAndRoomDetails {
                    user_id: mxid,
                    room: rpl.room
                })
            }
        }
    }
    fn process_received_message(&self, msg: SmsMessage) -> impl Future<Item = (), Error = Error> {
        let mx = self.mx.clone();

        info!("Processing message received from {}", msg.pdu.originating_address);
        let fut = self.get_user_and_room(msg.pdu.originating_address.clone());
        async_block! {
            let UserAndRoomDetails { room, user_id } = await!(fut)?;

            let text = match msg.pdu.get_message_data().decode_message() {
                Ok(DecodedMessage { text, .. }) => text,
                Err(e) => format!("[failed to decode: {}]", e)
            };
            if text.starts_with("DISPLAYNAME ") {
                let disp = text.replace("DISPLAYNAME ", "");
                info!("User requested displayname change to {}", disp);
                mx.borrow_mut().alter_user_id(user_id);
                await!(mx.borrow_mut().set_displayname(disp))?;
                return Ok(())
            }
            let msg = Message::Text {
                body: text,
                formatted_body: None,
                format: None
            };
            debug!("Sending message {:?} to room {}", msg, room.id);
            mx.borrow_mut().alter_user_id(user_id);
            await!(room.cli(&mut mx.borrow_mut())
                   .send(msg))?;
            Ok(())
        }
    }
    fn process_invite(&self, room: Room<'static>) -> impl Future<Item = (), Error = Error> {
        let mx = self.mx.clone();
        let hsl = self.hs_localpart.clone();
        async_block! {
            mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
            await!(mx.borrow_mut().join(&room.id))?;
            Ok(())
        }
    }
    fn process_admin_command(&self, sender: String, room: Room<'static>, text: &str) -> Box<Future<Item = (), Error = Error>> {
        let mx = self.mx.clone();
        let modem = self.modem.clone();
        let hsl = self.hs_localpart.clone();
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
                let fut = self.get_user_and_room(addr.clone());
                Box::new(async_block! {
                    let _ = await!(fut)?;
                    mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
                    await!(room.cli(&mut mx.borrow_mut())
                           .send_simple(format!("New conversation with {} created.", addr)))?;
                    Ok(())
                })
            },
            AdminCommand::GetRegistration => {
                info!("Getting registration status");
                Box::new(async_block! {
                    let regst = await!(cmd::network::get_registration(&mut modem.borrow_mut()))?;
                    mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
                    await!(room.cli(&mut mx.borrow_mut())
                           .send_simple(format!("Registration status: {}", regst)))?;
                    Ok(())
                })
            },
            AdminCommand::GetSignal => {
                info!("Getting signal status");
                Box::new(async_block! {
                    let sq = await!(cmd::network::get_signal_quality(&mut modem.borrow_mut()))?;
                    mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
                    await!(room.cli(&mut mx.borrow_mut())
                           .send_simple(format!("RSSI: {}\nBER: {}", sq.rssi, sq.ber)))?;
                    Ok(())
                })
            },
            AdminCommand::Help => {
                info!("Help requested.");
                Box::new(async_block! {
                    mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
                    await!(room.cli(&mut mx.borrow_mut())
                           .send_simple(r#"This is an instance of matrix-appservice-sms (https://github.com/eeeeeta/matrix-appservice-sms/).
Currently supported commands:
- !sms <num>: start or resume an SMS conversation with a given phone number
- !help: display this text"#))?;
                    Ok(())
                })
            }
            AdminCommand::Unrecognized => {
                info!("Unrecognized admin command.");
                Box::new(async_block! {
                    mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
                    await!(room.cli(&mut mx.borrow_mut())
                           .send_simple("Unrecognized command. Try !help for more information."))?;
                    Ok(())
                })
            }
        }
    }
    fn process_sending_message(&self, recv: Recipient, sender: String, rm: Message) -> impl Future<Item = (), Error = Error> {
        let mx = self.mx.clone();
        let modem = self.modem.clone();

        async_block! {
            let num = un_normalize_address(&recv.phone_number)
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
                    let disp = await!(mx.borrow_mut().get_displayname(&sender))?;
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
                        debug!("Processing message: {:?}", msg);
                        if msg.status != MessageStatus::ReceivedUnread {
                            continue;
                        }
                        let fut = self.process_received_message(msg)
                            .map_err(|e| {
                                error!("Failed to process received message: {}", e);
                            });
                        self.handle.spawn(fut);
                    }
                    let fut = cmd::sms::del_sms_pdu(&mut self.modem.borrow_mut(),
                                                    DeletionOptions::DeleteReadAndOutgoing)
                        .map_err(|e| {
                            error!("Failed to delete messages: {}", e);
                        });
                    self.handle.spawn(fut);
                },
                IntMessage::CmglFailed(e) => {
                    error!("Message listing failed: {}", e);
                },
                IntMessage::MatrixEvent(meta, content) => {
                    if meta.sender.starts_with("@_sms") {
                        continue;
                    }
                    match content {
                        Content::RoomMember(m) => {
                            if let Some(invitee) = meta.state_key {
                                if let Some(room) = meta.room {
                                    if let Membership::Invite = m.membership {
                                        if invitee == format!("@_sms_bot:{}", self.hs_localpart) {
                                            info!("Invited by {} to {}", meta.sender, room.id);
                                            let fut = self.process_invite(room)
                                                .map_err(|e| error!("Failed to process invite: {}", e));
                                            self.handle.spawn(fut);
                                        }
                                    }
                                }
                            }
                        },
                        Content::RoomMessage(m) => {
                            if let Some(room) = meta.room {
                                let conn = self.db.get().expect("couldn't get a db connection");
                                let recv = {
                                    use schema::recipients::dsl::*;

                                    recipients.filter(room_id.eq(&room.id))
                                        .first::<Recipient>(&*conn)
                                        .optional()?
                                };
                                if let Some(recv) = recv {
                                    let mx = self.mx.clone();
                                    let sender = meta.sender.clone();
                                    let hsl = self.hs_localpart.clone();
                                    let ei = meta.event_id;
                                    debug!("Sending message from {}: {:?}", meta.sender, m);
                                    let fut = self.process_sending_message(recv, meta.sender, m)
                                        .then(move |res| {
                                            async_block! {
                                                if let Err(e) = res {
                                                    warn!("Error sending message: {}", e);
                                                    mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
                                                    let disp = await!(mx.borrow_mut().get_displayname(&sender));
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
                                                                                     sender,
                                                                                     disp.displayname,
                                                                                     e))
                                                    };
                                                    mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
                                                    let res = await!(room.cli(&mut mx.borrow_mut())
                                                                     .send(message));
                                                    if let Err(e) = res {
                                                        error!("Error sending 'error sending message' message: {}", e);
                                                    }
                                                    else {
                                                        debug!("Sent error notif.");
                                                    }
                                                }
                                                else {
                                                    mx.borrow_mut().alter_user_id(format!("@_sms_bot:{}", hsl));
                                                    let res = await!(room.cli(&mut mx.borrow_mut())
                                                                     .read_receipt(&ei));
                                                    if let Err(e) = res {
                                                        warn!("Error sending read receipt: {}", e);
                                                    }
                                                    else {
                                                        debug!("Sent read receipt.");
                                                    }
                                                }
                                                let res: Result<(), ()> = Ok(());
                                                res
                                            }
                                        });
                                    self.handle.spawn(fut);
                                }
                                else {
                                    if let Message::Text { ref body, .. } = m {
                                        if meta.sender == self.admin && body.starts_with("!") {
                                            info!("Processing admin message: {}", body);
                                            let fut = self.process_admin_command(meta.sender, room, body)
                                                .map_err(|e| error!("Failed to process admin message: {}", e));
                                            self.handle.spawn(fut);
                                            continue;
                                        }
                                    }
                                    warn!("No recipient for event {}: {:?}", meta.event_id, m);
                                }
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

