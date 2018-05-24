use future::Mx;
use huawei_modem::gsm_encoding::{DecodedMessage};
use gm::room::{RoomExt, Room};
use huawei_modem::pdu::{DeliverPdu};
use futures::prelude::*;
use std::collections::BTreeMap;
use gm::types::messages::Message;
use gm::errors::MatrixError;

#[derive(Serialize, Deserialize)]
pub struct CsmsData {
    pub parts: BTreeMap<u8, String>,
    pub total_parts: u8,
    pub finished: bool
}

#[async]
pub fn process_message(mut mx: Mx, room: Room<'static>, mid: i32, pdu: DeliverPdu) -> Result<(), ::failure::Error> {
    let DecodedMessage { mut text, udh } = match pdu.get_message_data().decode_message() {
        Ok(dm) => dm,
        Err(e) => {
            warn!("[M-{}] Error decoding message: {}", mid, e);
            DecodedMessage {
                text: format!("[Error decoding: {}]", e),
                udh: None
            }
        }
    };
    let csms_data = udh.as_ref().and_then(|x| x.get_concatenated_sms_data());
    if let Some(data) = csms_data {
        info!("[M-{}] Message is concatenated - data {:?}", mid, data);
        let sk = format!("ref-{}", data.reference);
        let state = await!(room.cli(&mut mx)
                           .get_typed_state::<CsmsData>
                           ("org.eu.theta.sms.concatenated", Some(&sk)));
        let mut state = match state {
            Ok(s) => s,
            Err(e) => {
                let mut val = None;
                if let MatrixError::BadRequest(ref e) = e {
                    if e.errcode == "M_NOT_FOUND" {
                        val = Some(CsmsData {
                            parts: BTreeMap::new(),
                            total_parts: data.parts,
                            finished: false
                        });
                    }
                }
                if let Some(val) = val {
                    val
                }
                else {
                    return Err(e.into());
                }
            }
        };
        if state.total_parts != data.parts || state.finished {
            info!("[M-{}] Remaking CsmsData", mid);
            state = CsmsData {
                parts: BTreeMap::new(),
                total_parts: data.parts,
                finished: false
            }
        }
        state.parts.insert(data.sequence,
                           ::std::mem::replace(&mut text, String::new()));
        if state.parts.len() >= state.total_parts as usize {
            info!("[M-{}] Concatenated SMS finished; sending real message", mid);
            let mut ret = String::new();
            for (_, text) in state.parts.iter() {
                ret += text;
            }
            state.finished = true;
            text = ret;
        }
        let finished = state.finished;
        await!(room.cli(&mut mx)
               .set_typed_state::<CsmsData>
               ("org.eu.theta.sms.concatenated", Some(&sk), state))?;
        if !finished {
            info!("[M-{}] Concatenated SMS incomplete; not continuing", mid);
            return Ok(());
        }
    }
    let msg = Message::Text {
        body: text,
        formatted_body: None,
        format: None
    };
    info!("[M-{}] Sending message to room {}", mid, room.id);
    debug!("[M-{}] Message data: {:?}", mid, msg);
    await!(room.cli(&mut mx).send(msg))?;
    Ok(())
}

