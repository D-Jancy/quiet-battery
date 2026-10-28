//! AJAZZ 2.4G dongle GET_DEVICE_INFO. The receiver ID is shared by several
//! OEM keyboards, so the returned model identity gates both the name and value.
use super::{
    Cancellation, DeviceError, DeviceKind, PowerState, Reading,
    diagnostics::Collection,
    vendor_hid::{Adapter, Profile, Transport, number},
};

const COMMAND: u8 = 0x10;
const CHUNK: usize = 24;
const LENGTH: usize = 48;
fn identify(c: &Collection) -> Option<Profile> {
    (number(c, "vid") == Some(0x0c45) && number(c, "pid") == Some(0xfefc)).then_some(Profile {
        name: "AJAZZ AK820 MAX",
        vid: 0x0c45,
        pid: 0xfefc,
        wireless: true,
    })
}
fn accepts(c: &Collection) -> bool {
    [
        ("usage_page", 0xff80),
        ("usage", 1),
        ("input_report_bytes", 33),
        ("output_report_bytes", 33),
    ]
    .iter()
    .all(|&(k, v)| number(c, k) == Some(v))
}
pub(super) const ADAPTER: Adapter = Adapter {
    name: "ajazz",
    kind: DeviceKind::Keyboard,
    unconfirmed_name: Some("未识别的2.4G键盘"),
    identify,
    accepts,
    feature_only: false,
    query,
};
// Windows report ID zero occupies byte 0; the vendor frame starts at byte 1.
fn request(address: u8, last: bool) -> [u8; 33] {
    let mut b = [0; 33];
    b[1] = 0xaa;
    b[2] = COMMAND;
    b[3] = CHUNK as u8;
    b[4] = address;
    b[5] = 0;
    b[6] = 0;
    b[7] = u8::from(last);
    b
}
fn chunk(b: &[u8], address: u8) -> Option<&[u8]> {
    if b.len() != 33 || b[1] != 0x55 || b[2] != COMMAND {
        return None;
    }
    // The vendor client ignores the echoed address. Require it when present,
    // so a late reply from another chunk cannot be stitched into this one.
    let echoed = u16::from(b[4]) | u16::from(b[5]) << 8;
    if echoed != 0 && echoed != u16::from(address) {
        return None;
    }
    b.get(9..9 + CHUNK)
}
fn decode(info: &[u8; LENGTH]) -> Result<(u8, PowerState), DeviceError> {
    let vid = u16::from_le_bytes([info[4], info[5]]);
    let pid = u16::from_le_bytes([info[6], info[7]]);
    // The receiver is shared; only the documented AK820 MAX model identity and
    // 2.4G work mode may claim this name and value.
    if vid != 0x0c45 || pid != 0x80a0 || info[16] != 2 {
        return Err(DeviceError::Unsupported(
            "AJAZZ接收器未确认目标键盘与2.4G路由".into(),
        ));
    }
    if info[17] > 100 {
        return Err(DeviceError::Protocol("AJAZZ电量越界".into()));
    }
    // The web client stores chargeStatus without defining it: keep it raw.
    Ok((info[17], PowerState::Unknown(info[18])))
}
fn query(p: Profile, io: &dyn Transport, cancel: &Cancellation) -> Result<Reading, DeviceError> {
    let mut info = [0; LENGTH];
    for (address, length) in [(0, 24), (24, 24)] {
        let mut content = None;
        for _ in 0..2 {
            cancel.check()?;
            let last = usize::from(address) + length == LENGTH;
            if let Some(b) = io.exchange(&request(address, last), 33, false)? {
                if let Some(data) = chunk(&b, address) {
                    content = Some(data.to_vec());
                    break;
                }
            }
        }
        let data = content
            .ok_or_else(|| DeviceError::Unavailable("AJAZZ未收到匹配的设备信息分块".into()))?;
        info[usize::from(address)..usize::from(address) + length].copy_from_slice(&data);
    }
    cancel.check()?;
    let (percent, power) = decode(&info)?;
    Reading::from_percent(p.name.into(), percent, power)
}
#[cfg(test)]
mod tests {
    use super::super::{
        diagnostics::Field,
        vendor_hid::tests::{Script, Step, collection},
    };
    use super::*;
    fn p() -> Profile {
        identify(&collection(0x0c45, 0xfefc, "fixture")).unwrap()
    }
    // Synthetic device info with the documented field offsets, not a hardware capture.
    fn sample() -> [u8; LENGTH] {
        let mut b = [0; LENGTH];
        b[4..8].copy_from_slice(&[0x45, 0x0c, 0xa0, 0x80]);
        b[16] = 2;
        b[17] = 56;
        b[30] = 1;
        b
    }
    fn response(info: &[u8; LENGTH], a: u8) -> Vec<u8> {
        let mut b = request(a, usize::from(a) + CHUNK == LENGTH).to_vec();
        b[1] = 0x55;
        b[9..9 + CHUNK].copy_from_slice(&info[usize::from(a)..usize::from(a) + CHUNK]);
        b
    }
    fn step(info: &[u8; LENGTH], a: u8) -> Step {
        Step::Exchange(
            request(a, usize::from(a) + CHUNK == LENGTH).to_vec(),
            false,
            Some(response(info, a)),
        )
    }
    #[test]
    fn only_the_documented_receiver_and_vendor_collection_are_eligible() {
        let mut c = collection(0x0c45, 0xfefc, "fixture");
        assert!(identify(&c).is_some());
        assert!(!accepts(&c));
        for (k, v) in [
            ("usage_page", 0xff80),
            ("usage", 1),
            ("input_report_bytes", 33),
            ("output_report_bytes", 33),
        ] {
            c.fields.insert(k, Field::number(v, "fixture"));
        }
        assert!(accepts(&c));
        for (k, v) in [
            ("usage_page", 0xff60),
            ("usage", 2),
            ("input_report_bytes", 32),
            ("output_report_bytes", 65),
        ] {
            let mut other = c.clone();
            other.fields.insert(k, Field::number(v, "fixture"));
            assert!(!accepts(&other));
        }
        assert!(identify(&collection(0x0c45, 0xfefe, "aula")).is_none());
        assert_eq!(ADAPTER.kind, DeviceKind::Keyboard);
    }
    #[test]
    fn complete_chunked_transaction_matches_command_and_device_identity() {
        let info = sample();
        let io = Script::new(vec![step(&info, 0), step(&info, 24)]);
        let r = query(p(), &io, &Cancellation::default()).unwrap();
        io.done();
        assert_eq!(
            (r.normalized_percent(), r.power_state),
            (56, PowerState::Unknown(0))
        );
        assert_eq!(&request(0, false)[..8], &[0, 0xaa, 0x10, 24, 0, 0, 0, 0]);
        assert_eq!(&request(24, true)[..8], &[0, 0xaa, 0x10, 24, 24, 0, 0, 1]);
        for (index, value) in [(4, 0x46), (6, 0), (16, 0), (17, 101)] {
            let mut bad = info;
            bad[index] = value;
            assert!(decode(&bad).is_err());
        }
        for percent in [0, 100] {
            let mut b = info;
            b[17] = percent;
            b[18] = 1;
            assert_eq!(decode(&b).unwrap(), (percent, PowerState::Unknown(1)));
        }
    }
    #[test]
    fn short_wrong_header_and_silence_never_update_percent() {
        let good = response(&sample(), 0);
        for index in [1, 2] {
            let mut b = good.clone();
            b[index] ^= 1;
            assert!(chunk(&b, 0).is_none());
        }
        for len in 0..33 {
            assert!(chunk(&good[..len], 0).is_none());
        }
        let mut other = good.clone();
        other[4] = 24;
        assert!(chunk(&other, 0).is_none());
        let io = Script::new(vec![
            Step::Exchange(request(0, false).to_vec(), false, None),
            Step::Exchange(request(0, false).to_vec(), false, None),
        ]);
        assert!(query(p(), &io, &Cancellation::default()).is_err());
        io.done();
        let c = Cancellation::default();
        c.cancel();
        let io = Script::new(vec![]);
        assert!(matches!(query(p(), &io, &c), Err(DeviceError::Cancelled)));
        io.done();
    }
}
