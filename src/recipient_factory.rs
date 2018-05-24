use futures::prelude::*;
use store::Store;
use models::NewRecipient;
use huawei_modem::pdu::PduAddress;
use gm::types::content::room::PowerLevels;
use gm::room::{RoomExt, Room, NewRoom};
use gm::types::replies::{RoomCreationOptions, RoomPreset, RoomVisibility};
use gm::errors::MatrixError;
use gm::MatrixClient;
use tokio_core::reactor::Handle;
use futures::sync::mpsc;
use users::{UserHandle, UserManager};
use std::rc::Rc;
use std::cell::RefCell;
use util;

pub struct RecipientFactory {
    pub(crate) store: Store,
    pub(crate) hdl: Handle,
    pub(crate) addr: PduAddress,
    pub(crate) mx: MatrixClient,
    pub(crate) admin: String,
    pub(crate) hs_localpart: String,
}
impl RecipientFactory {
    #[async]
    pub fn run(mut self) -> Result<UserHandle, ::failure::Error> {
        let recip = match self.store.get_recipient_for_address(&self.addr)? {
            Some(r) => r,
            None => {
                let addr = util::normalize_address(&self.addr);
                let localpart = format!("_sms_{}", addr);
                let mxid = format!("@{}:{}", localpart, self.hs_localpart);
                info!("[C-{}] Registering new user", mxid);
                if let Err(e) = await!(self.mx.as_register_user(localpart.clone())) {
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
                info!("[C-{}] Creating new room", mxid);
                let opts = RoomCreationOptions {
                    preset: Some(RoomPreset::TrustedPrivateChat),
                    is_direct: true,
                    invite: vec![mxid.clone()],
                    room_alias_name: Some(format!("_sms_{}", addr)),
                    name: Some(format!("{} (SMS)", self.addr)),
                    visibility: Some(RoomVisibility::Private),
                    ..Default::default()
                };
                let rpl = await!(NewRoom::create(&mut self.mx, opts))?;
                info!("[C-{}] Joining new room", mxid);
                self.mx.as_alter_user_id(mxid.clone());
                await!(NewRoom::join(&mut self.mx, &rpl.id))?;
                let new_recipient = NewRecipient {
                    phone_number: &addr,
                    user_id: &localpart,
                    room_id: &rpl.id
                };
                self.store.create_recipient(new_recipient)?
            }
        };
        let room = Room::from_id(recip.room_id);
        self.mx.as_alter_user_id(format!("@{}:{}", recip.user_id, self.hs_localpart));
        let jm = await!(room.cli(&mut self.mx).get_joined_members())?;
        if jm.joined.get(&self.admin).is_none() {
            if let Err(e) = await!(room.cli(&mut self.mx).invite_user(&self.admin)) {
                error!("[C-{}] Error inviting user {} to room {}: {}", recip.id, self.admin, room.id, e);
            }
        }
        let pl = await!(room.cli(&mut self.mx).get_user_power_level(self.admin.clone()))?;
        if pl < 100 {
            if let Ok(mut pl) = await!(room.cli(&mut self.mx).get_typed_state::<PowerLevels>("m.room.power_levels", None)) {
                pl.users.insert(self.admin.clone(), 100);
                if let Err(e) = await!(room.cli(&mut self.mx).set_typed_state::<PowerLevels>("m.room.power_levels", None, pl)) {
                    error!("[C-{}] Error setting powerlevels of user {} in room {}: {}", recip.id, self.admin, room.id, e);
                }
            }
        }
        let (ctl_tx, ctl_rx) = mpsc::unbounded();
        let um = UserManager {
            mx: Rc::new(RefCell::new(self.mx)),
            recipient_id: recip.id,
            store: self.store,
            hdl: self.hdl.clone(),
            room: room,
            ctl_tx: ctl_tx.clone()
        };
        let fut = um.future(ctl_rx);
        self.hdl.spawn(fut);
        Ok(UserHandle { ctl: ctl_tx })
    }
}
