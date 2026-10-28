//! BlackShark V2 Pro 2023 (0555) read-only 64-byte vendor protocol.
//! Packet facts: Issue #2 and pinned Ferris Pulse source, independently rewritten.
use super::{
    Cancellation, DeviceError, DeviceKind, PowerState, Reading,
    diagnostics::Collection,
    vendor_hid::{Adapter, Profile, Transport, number},
};
fn identify(c: &Collection) -> Option<Profile> {
    (number(c, "vid") == Some(0x1532) && number(c, "pid") == Some(0x0555)).then_some(Profile {
        name: "Razer BlackShark V2 Pro (2023)",
        vid: 0x1532,
        pid: 0x0555,
        wireless: true,
    })
}
fn accepts(c: &Collection) -> bool {
    [
        ("interface", 3),
        ("usage_page", 0xff00),
        ("usage", 1),
        ("input_report_bytes", 64),
        ("output_report_bytes", 64),
    ]
    .iter()
    .all(|&(k, v)| number(c, k) == Some(v))
}
pub(super) const ADAPTER: Adapter = Adapter {
    name: "razer-headset",
    kind: DeviceKind::Headset,
    unconfirmed_name: None,
    identify,
    accepts,
    feature_only: false,
    query,
};
fn request(cmd: u8) -> [u8; 64] {
    let mut b = [0; 64];
    b[..11].copy_from_slice(&[2, 0x80, 8, 0, 0, 0x50, 0x41, 8, 0, 3, cmd]);
    b
}
fn parse(b: &[u8], cmd: u8) -> Option<u8> {
    if b.len() != 64 || b[0] != 2 || b[2..5] != [0x50, 0x49, 8] || b[10..15] != [4, 0, cmd, 1, 1] {
        return None;
    }
    Some(b[15])
}
fn value(io: &dyn Transport, cmd: u8, cancel: &Cancellation) -> Result<u8, DeviceError> {
    for _ in 0..2 {
        cancel.check()?;
        if let Some(b) = io.exchange(&request(cmd), 64, false)? {
            if let Some(value) = parse(&b, cmd) {
                return Ok(value);
            }
        }
    }
    Err(DeviceError::Unavailable("Razer耳机未收到匹配回复".into()))
}
fn connected(io: &dyn Transport, cancel: &Cancellation) -> Result<(), DeviceError> {
    if value(io, 0x20, cancel)? != 0 {
        Ok(())
    } else {
        Err(DeviceError::Unavailable("Razer耳机未确认连接".into()))
    }
}
fn query(p: Profile, io: &dyn Transport, cancel: &Cancellation) -> Result<Reading, DeviceError> {
    // The USB receiver remains present while the headset is off. Do not let a
    // disconnected receiver's zero erase a previous successful observation.
    connected(io, cancel)?;
    let percent = value(io, 0x21, cancel)?;
    if percent > 100 {
        return Err(DeviceError::Protocol("Razer耳机电量越界".into()));
    }
    // Confirm the target stayed connected while obtaining the battery, before
    // spending any remaining budget on optional charging information.
    connected(io, cancel)?;
    let power = match value(io, 0x2a, cancel) {
        Ok(0) => PowerState::Discharging,
        Ok(1) => PowerState::Charging,
        Ok(v) => PowerState::Unknown(v),
        Err(DeviceError::Cancelled) => return Err(DeviceError::Cancelled),
        Err(_) => PowerState::Unknown(255),
    };
    cancel.check()?;
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
        identify(&collection(0x1532, 0x0555, "fixture")).unwrap()
    }
    fn response(cmd: u8, value: u8) -> Vec<u8> {
        // Issue #2 includes a Synapse trace with 0x5f=95% for 0x21.
        let mut b = vec![0; 64];
        b[..16].copy_from_slice(&[
            2, 0x0e, 0x50, 0x49, 8, 0xdd, 0xd2, 0x64, 0x18, 0, 4, 0, cmd, 1, 1, value,
        ]);
        b
    }
    fn x(cmd: u8, value: u8) -> Step {
        Step::Exchange(request(cmd).to_vec(), false, Some(response(cmd, value)))
    }
    #[test]
    fn only_2023_headset_interface_and_sized_reports_are_eligible() {
        let mut c = collection(0x1532, 0x0555, "fixture");
        assert!(!accepts(&c));
        for (k, v) in [
            ("interface", 3),
            ("usage_page", 0xff00),
            ("usage", 1),
            ("input_report_bytes", 64),
            ("output_report_bytes", 64),
        ] {
            c.fields.insert(k, Field::number(v, "fixture"));
        }
        assert!(accepts(&c));
        for (k, v) in [
            ("interface", 0),
            ("usage_page", 1),
            ("usage", 2),
            ("input_report_bytes", 63),
            ("output_report_bytes", 65),
        ] {
            let mut other = c.clone();
            other.fields.insert(k, Field::number(v, "fixture"));
            assert!(!accepts(&other));
        }
        assert!(identify(&collection(0x1532, 0x0528, "older")).is_none());
        assert_eq!(ADAPTER.kind, DeviceKind::Headset);
    }
    #[test]
    fn complete_query_keeps_battery_when_charge_query_fails() {
        for percent in [0, 90, 95, 100] {
            let io = Script::new(vec![
                x(0x20, 1),
                x(0x21, percent),
                x(0x20, 1),
                Step::Failure,
            ]);
            let r = query(p(), &io, &Cancellation::default()).unwrap();
            io.done();
            assert_eq!(
                (r.normalized_percent(), r.power_state),
                (percent, PowerState::Unknown(255))
            );
        }
        for (value, state) in [
            (0, PowerState::Discharging),
            (1, PowerState::Charging),
            (8, PowerState::Unknown(8)),
        ] {
            let io = Script::new(vec![x(0x20, 1), x(0x21, 95), x(0x20, 1), x(0x2a, value)]);
            let r = query(p(), &io, &Cancellation::default()).unwrap();
            io.done();
            assert_eq!(r.power_state, state);
        }
    }
    #[test]
    fn disconnect_and_overflow_cannot_replace_cached_battery() {
        let io = Script::new(vec![x(0x20, 0)]);
        assert!(query(p(), &io, &Cancellation::default()).is_err());
        io.done();
        let io = Script::new(vec![x(0x20, 1), x(0x21, 0), x(0x20, 0)]);
        assert!(query(p(), &io, &Cancellation::default()).is_err());
        io.done();
        let io = Script::new(vec![x(0x20, 1), x(0x21, 101)]);
        assert!(query(p(), &io, &Cancellation::default()).is_err());
        io.done();
    }
    #[test]
    fn unrelated_truncated_and_silent_reports_are_bounded() {
        let good = response(0x21, 95);
        assert_eq!(parse(&good, 0x21), Some(95));
        for i in [0, 2, 3, 4, 10, 11, 12, 13, 14] {
            let mut bad = good.clone();
            bad[i] ^= 1;
            assert_eq!(parse(&bad, 0x21), None);
        }
        for len in 0..64 {
            assert_eq!(parse(&good[..len], 0x21), None);
        }
        let io = Script::new(vec![
            Step::Exchange(request(0x20).to_vec(), false, Some(good)),
            Step::Exchange(request(0x20).to_vec(), false, None),
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
