//! ROG PELTA control-output queries, independently implemented from Issue #1's
//! corrected hardware trace. No use of the earlier speculative commands.
use super::{
    Cancellation, DeviceError, DeviceKind, PowerState, Reading,
    diagnostics::Collection,
    vendor_hid::{Adapter, Profile, Transport, number},
};
fn identify(c: &Collection) -> Option<Profile> {
    (number(c, "vid") == Some(0x0b05) && number(c, "pid") == Some(0x1b84)).then_some(Profile {
        name: "ROG PELTA",
        vid: 0x0b05,
        pid: 0x1b84,
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
    name: "rog-pelta",
    kind: DeviceKind::Headset,
    unconfirmed_name: None,
    identify,
    accepts,
    feature_only: false,
    query,
};
fn request(command: u8) -> [u8; 64] {
    let mut b = [0; 64];
    b[..3].copy_from_slice(&[0xcc, 0x12, command]);
    b
}
fn parse(b: &[u8], command: u8) -> Option<u8> {
    if b.len() != 64 || b[..3] != [0xcc, 0x12, command] {
        return None;
    }
    Some(b[if command == 7 { 6 } else { 5 }])
}
fn value(io: &dyn Transport, command: u8, cancel: &Cancellation) -> Result<u8, DeviceError> {
    for _ in 0..2 {
        cancel.check()?;
        if let Some(b) = io.exchange(&request(command), 64, true)? {
            if let Some(v) = parse(&b, command) {
                return Ok(v);
            }
        }
    }
    Err(DeviceError::Unavailable("ROG PELTA未收到匹配回复".into()))
}
fn query(p: Profile, io: &dyn Transport, cancel: &Cancellation) -> Result<Reading, DeviceError> {
    let percent = value(io, 7, cancel)?;
    if percent > 100 {
        return Err(DeviceError::Protocol("ROG PELTA电量越界".into()));
    }
    let power = match value(io, 8, cancel) {
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
        identify(&collection(0x0b05, 0x1b84, "receiver")).unwrap()
    }
    fn reply(cmd: u8, value: u8) -> Vec<u8> {
        let mut b = request(cmd).to_vec();
        b[if cmd == 7 { 6 } else { 5 }] = value;
        b
    }
    fn x(cmd: u8, b: Option<Vec<u8>>) -> Step {
        Step::Exchange(request(cmd).to_vec(), true, b)
    }
    #[test]
    fn pelta_targets_only_the_corrected_collection_not_bluetooth_or_guesses() {
        let mut c = collection(0x0b05, 0x1b84, "fixture");
        assert!(identify(&c).is_some());
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
            ("usage_page", 0xff07),
            ("usage", 2),
            ("input_report_bytes", 63),
            ("output_report_bytes", 65),
        ] {
            let mut bad = c.clone();
            bad.fields.insert(k, Field::number(v, "fixture"));
            assert!(!accepts(&bad));
        }
        assert!(identify(&collection(0x0b05, 0x1b86, "bluetooth")).is_none());
        assert_eq!(ADAPTER.kind, DeviceKind::Headset);
    }
    #[test]
    fn hardware_trace_percent_and_optional_charge_with_late_reply_are_independent() {
        for percent in [0, 68, 69, 70, 71, 74, 100] {
            let mut b = reply(7, percent);
            b[7] = 0x14;
            b[8] = 1;
            let io = Script::new(vec![
                x(7, Some(reply(8, 0))),
                x(7, Some(b)),
                x(8, Some(reply(7, 0))),
                x(8, Some(reply(8, 1))),
            ]);
            let r = query(p(), &io, &Cancellation::default()).unwrap();
            io.done();
            assert_eq!(
                (r.normalized_percent(), r.power_state),
                (percent, PowerState::Charging)
            );
            assert!(!r.metadata_known);
        }
        let io = Script::new(vec![x(7, Some(reply(7, 74))), Step::Failure]);
        let r = query(p(), &io, &Cancellation::default()).unwrap();
        io.done();
        assert_eq!(
            (r.normalized_percent(), r.power_state),
            (74, PowerState::Unknown(255))
        );
    }
    #[test]
    fn malformed_silent_overflow_and_cancel_do_not_create_a_reading() {
        for mut b in [reply(7, 90), vec![0; 63]] {
            b[0] = 0;
            let io = Script::new(vec![x(7, Some(b)), x(7, None)]);
            assert!(query(p(), &io, &Cancellation::default()).is_err());
            io.done();
        }
        let io = Script::new(vec![x(7, Some(reply(7, 101)))]);
        assert!(query(p(), &io, &Cancellation::default()).is_err());
        io.done();
        let c = Cancellation::default();
        c.cancel();
        let io = Script::new(vec![]);
        assert!(matches!(query(p(), &io, &c), Err(DeviceError::Cancelled)));
        io.done();
    }
}
