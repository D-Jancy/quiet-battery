//! SONiX GET_DEVICE_INFO only. The receiver ID is shared; returned VID/PID and
//! wireless workMode must also identify the documented MINI 60 HE PRO route.
use super::{
    Cancellation, DeviceError, DeviceKind, PowerState, Reading,
    diagnostics::Collection,
    vendor_hid::{Adapter, Profile, Transport, number},
};
fn identify(c: &Collection) -> Option<Profile> {
    (number(c, "vid") == Some(0x0c45) && number(c, "pid") == Some(0xfefe)).then_some(Profile {
        name: "AULA MINI 60 HE PRO",
        vid: 0x0c45,
        pid: 0xfefe,
        wireless: true,
    })
}
fn accepts(c: &Collection) -> bool {
    [
        ("usage_page", 0xff60),
        ("usage", 0x61),
        ("input_report_bytes", 33),
        ("output_report_bytes", 33),
    ]
    .iter()
    .all(|&(k, v)| number(c, k) == Some(v))
}
pub(super) const ADAPTER: Adapter = Adapter {
    name: "aula",
    kind: DeviceKind::Keyboard,
    unconfirmed_name: Some("未识别的 SONiX HID 设备"),
    identify,
    accepts,
    feature_only: false,
    query,
};
fn request(address: u8, length: u8) -> [u8; 33] {
    let mut b = [0; 33];
    b[1] = 0xaa;
    b[2] = 0x10;
    b[3] = length;
    b[4] = address;
    b[7] = u8::from(usize::from(address) + usize::from(length) == 56);
    b
}
fn chunk(b: &[u8], address: u8, length: u8) -> Option<&[u8]> {
    if b.len() != 33 || b[..6] != [0, 0x55, 0x10, length, address, 0] {
        return None;
    }
    b.get(9..9 + usize::from(length))
}
fn decode(info: &[u8; 56]) -> Result<(u8, PowerState), DeviceError> {
    let vid = u16::from_le_bytes([info[4], info[5]]);
    let pid = u16::from_le_bytes([info[6], info[7]]);
    if vid != 0x0c45 || !matches!(pid, 0x80a2 | 0x80b2) || info[16] != 2 {
        return Err(DeviceError::Unsupported(
            "AULA接收器未确认目标键盘与2.4G路由".into(),
        ));
    }
    if info[17] > 100 {
        return Err(DeviceError::Protocol("AULA电量越界".into()));
    }
    // chargeStatus meaning has not been confirmed. Preserve the raw byte.
    Ok((info[17], PowerState::Unknown(info[18])))
}
fn query(p: Profile, io: &dyn Transport, cancel: &Cancellation) -> Result<Reading, DeviceError> {
    let mut info = [0; 56];
    for (address, length) in [(0, 24), (24, 24), (48, 8)] {
        let mut content = None;
        for _ in 0..2 {
            cancel.check()?;
            if let Some(b) = io.exchange(&request(address, length), 33, false)? {
                if let Some(data) = chunk(&b, address, length) {
                    content = Some(data.to_vec());
                    break;
                }
            }
        }
        let data = content
            .ok_or_else(|| DeviceError::Unavailable("AULA未收到匹配的设备信息分块".into()))?;
        info[usize::from(address)..usize::from(address + length)].copy_from_slice(&data);
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
    // Issue #2's reported firmware 1.55 content, not a local hardware result.
    fn sample() -> [u8; 56] {
        let mut b = [0; 56];
        b[..19].copy_from_slice(&[
            0, 0, 0, 0x92, 0x45, 0x0c, 0xa2, 0x80, 0x55, 1, 0, 0, 0x66, 1, 8, 0x0a, 2, 0x38, 0,
        ]);
        b
    }
    fn p() -> Profile {
        identify(&collection(0x0c45, 0xfefe, "fixture")).unwrap()
    }
    fn response(info: &[u8; 56], a: u8, n: u8) -> Vec<u8> {
        let mut b = request(a, n).to_vec();
        b[1] = 0x55;
        b[9..9 + usize::from(n)].copy_from_slice(&info[usize::from(a)..usize::from(a + n)]);
        b
    }
    fn step(info: &[u8; 56], a: u8, n: u8) -> Step {
        Step::Exchange(request(a, n).to_vec(), false, Some(response(info, a, n)))
    }
    #[test]
    fn only_documented_receiver_vendor_reports_are_eligible() {
        let mut c = collection(0x0c45, 0xfefe, "fixture");
        assert!(identify(&c).is_some());
        assert!(!accepts(&c));
        for (k, v) in [
            ("usage_page", 0xff60),
            ("usage", 0x61),
            ("input_report_bytes", 33),
            ("output_report_bytes", 33),
        ] {
            c.fields.insert(k, Field::number(v, "fixture"));
        }
        assert!(accepts(&c));
        for (k, v) in [
            ("usage_page", 0xff68),
            ("usage", 1),
            ("input_report_bytes", 32),
            ("output_report_bytes", 65),
        ] {
            let mut other = c.clone();
            other.fields.insert(k, Field::number(v, "fixture"));
            assert!(!accepts(&other));
        }
        assert!(identify(&collection(0x0c45, 0x80a2, "wired")).is_none());
        assert_eq!(ADAPTER.kind, DeviceKind::Keyboard);
    }
    #[test]
    fn complete_chunked_transaction_matches_addresses_and_device_identity() {
        let info = sample();
        let io = Script::new(vec![
            step(&info, 0, 24),
            Step::Exchange(
                request(24, 24).to_vec(),
                false,
                Some(response(&info, 0, 24)),
            ),
            step(&info, 24, 24),
            step(&info, 48, 8),
        ]);
        let r = query(p(), &io, &Cancellation::default()).unwrap();
        io.done();
        assert_eq!(
            (r.normalized_percent(), r.power_state),
            (56, PowerState::Unknown(0))
        );
        assert_eq!(&request(48, 8)[..8], &[0, 0xaa, 0x10, 8, 48, 0, 0, 1]);
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
    fn short_wrong_id_header_and_silence_never_update_percent() {
        let good = response(&sample(), 0, 24);
        for index in [0, 1, 2, 3, 4, 5] {
            let mut b = good.clone();
            b[index] ^= 1;
            assert!(chunk(&b, 0, 24).is_none());
        }
        for len in 0..33 {
            assert!(chunk(&good[..len], 0, 24).is_none());
        }
        let io = Script::new(vec![
            Step::Exchange(request(0, 24).to_vec(), false, None),
            Step::Exchange(request(0, 24).to_vec(), false, None),
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
