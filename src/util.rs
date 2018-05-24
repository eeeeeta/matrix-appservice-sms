use huawei_modem::pdu::{PduAddress, AddressType};
use huawei_modem::convert::TryFrom;

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

