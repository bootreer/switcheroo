//! Synthetic Dock swipe gestures for instant Space switching.
//!
//! Ported from github.com/jurplel/InstantSpaceSwitcher
//! (MIT, Copyright (c) 2026 jurplel):
//! - legacy path: `iss_post_dock_swipe` in Sources/ISS/ISS.c
//! - macOS 27 path: `iss_post_modern_dock_swipe` in Sources/ISS/ISS.c and
//!   `iss_augment_dock_swipe_event` in Sources/ISS/event_serialize.c
//!
//! On macOS 27 the Dock ignores dock-swipe CGEvents that only carry the public
//! gesture fields. The event must also embed the raw IOHID payload a real
//! trackpad would produce, be paired with a companion gesture event, and
//! commit via a fling velocity on the Ended phase.

use std::sync::OnceLock;

use objc2_core_foundation::{
    CFData, CFPreferencesAppSynchronize, CFPreferencesGetAppBooleanValue, CFRetained, CFString,
    kCFPreferencesAnyApplication,
};
use objc2_core_graphics::{CGEvent, CGEventField, CGEventTapLocation, CGEventType};
use objc2_foundation::NSProcessInfo;

const FIELD_CGS_EVENT_TYPE: CGEventField = CGEventField(55);
const FIELD_GESTURE_HID_TYPE: CGEventField = CGEventField(110);
const FIELD_GESTURE_SWIPE_MOTION: CGEventField = CGEventField(123);
const FIELD_GESTURE_SWIPE_PROGRESS: CGEventField = CGEventField(124);
const FIELD_GESTURE_SWIPE_POSITION_X: CGEventField = CGEventField(125);
const FIELD_GESTURE_SWIPE_VELOCITY_X: CGEventField = CGEventField(129);
const FIELD_GESTURE_SWIPE_VELOCITY_Y: CGEventField = CGEventField(130);
const FIELD_GESTURE_PHASE: CGEventField = CGEventField(132);
const FIELD_GESTURE_PHASE_ALIAS: CGEventField = CGEventField(134);
const FIELD_GESTURE_ZOOM_DELTA_Y: CGEventField = CGEventField(138);
const FIELD_SOURCE_PID_ALIAS: CGEventField = CGEventField(169);

const CGS_EVENT_GESTURE: i64 = 29;
const CGS_EVENT_DOCK_CONTROL: i64 = 30;
const IO_HID_EVENT_TYPE_DOCK_SWIPE: i64 = 23;
const CGS_GESTURE_MOTION_HORIZONTAL: u16 = 1;
const PHASE_BEGAN: i64 = 1;
const PHASE_CHANGED: i64 = 2;
const PHASE_ENDED: i64 = 4;
const PHASES: [i64; 3] = [PHASE_BEGAN, PHASE_CHANGED, PHASE_ENDED];

const LEGACY_GESTURE_SPEED: f64 = 2000.0;
// Smallest non-zero 16.16 value, so nothing visibly animates (as ISS 6bad72e).
// Full ±1.0 progress on Changed/Ended avoids Spaces whose windows sometimes
// stay blank on macOS 26 (ISS #58), but makes the switch visibly animate.
const SWIPE_PROGRESS: f64 = 0.000016;
// The IOHID velocity is 16.16 fixed point (max ~32767), so the legacy
// speed, which scales with hop count, can't be reused.
const MODERN_FLING_VELOCITY: f64 = 9999.0;
// From the FasterSwiper trackpad traces ISS copied.
const MODERN_POSITION_X: f64 = 0.1;

const IOHID_EVENT_TYPE_VELOCITY: u32 = 9;
const IOHID_EVENT_TYPE_FLUID_TOUCH_GESTURE: u32 = 23;
const IOHID_GESTURE_FLAVOR_DOCK_PRIMARY: u16 = 3;
const IOHID_PAYLOAD_TAG_FIELD: u16 = 4205;
const FLUID_EVENT_LEN: u32 = 40;
const VELOCITY_EVENT_LEN: u32 = 28;

unsafe extern "C" {
    // SAFETY: takes nothing and returns an integer.
    safe fn mach_absolute_time() -> u64;
}

/// Post `hops` swipe gestures toward the right (`is_right`) or left.
pub fn switch_spaces(is_right: bool, hops: usize) {
    if requires_event_augmentation() {
        let sign = modern_swipe_sign(is_right);
        for _ in 0..hops {
            for phase in PHASES {
                if !post_modern(phase, sign) {
                    log::warn!("failed to post augmented dock swipe (phase {phase})");
                    return;
                }
            }
        }
    } else {
        // Scaling velocity by hop count keeps multi-Space jumps from undershooting
        let velocity = LEGACY_GESTURE_SPEED * hops as f64;
        for _ in 0..hops {
            for phase in PHASES {
                post_legacy(phase, is_right, velocity);
            }
        }
    }
}

/// macOS 27+ requires the IOHID-augmented event path.
/// `SWITCHEROO_FORCE_EVENT_AUGMENTATION=1|0` overrides detection for testing.
fn requires_event_augmentation() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        if let Ok(v) = std::env::var("SWITCHEROO_FORCE_EVENT_AUGMENTATION") {
            return v == "1";
        }
        NSProcessInfo::processInfo()
            .operatingSystemVersion()
            .majorVersion
            >= 27
    })
}

/// On macOS 27 "right" is negative, and the Dock interprets the posted sign
/// through the natural scrolling preference, so it flips again when natural
/// scrolling is off. Read on every switch so a settings change applies
/// without a notification observer.
fn modern_swipe_sign(is_right: bool) -> f64 {
    // SAFETY: immutable CFString constant exported by CoreFoundation.
    let any_app = unsafe { kCFPreferencesAnyApplication };
    CFPreferencesAppSynchronize(any_app);
    let mut valid = 0u8;
    // SAFETY: `valid` is a live local, so the out-pointer is valid for the call.
    let value = unsafe {
        CFPreferencesGetAppBooleanValue(
            &CFString::from_static_str("com.apple.swipescrolldirection"),
            any_app,
            &mut valid,
        )
    };
    // Natural scrolling is the macOS default when the key is unset
    let natural = valid == 0 || value;
    let sign = if is_right { -1.0 } else { 1.0 };
    if natural { sign } else { -sign }
}

fn post_legacy(phase: i64, is_right: bool, velocity: f64) {
    let sign = if is_right { 1.0 } else { -1.0 };
    let Some(ev) = CGEvent::new(None) else { return };
    let e = Some(&*ev);
    CGEvent::set_type(e, CGEventType(CGS_EVENT_DOCK_CONTROL as u32));
    CGEvent::set_integer_value_field(e, FIELD_GESTURE_HID_TYPE, IO_HID_EVENT_TYPE_DOCK_SWIPE);
    CGEvent::set_integer_value_field(e, FIELD_GESTURE_PHASE, phase);
    CGEvent::set_double_value_field(e, FIELD_GESTURE_SWIPE_PROGRESS, sign * SWIPE_PROGRESS);
    CGEvent::set_integer_value_field(
        e,
        FIELD_GESTURE_SWIPE_MOTION,
        CGS_GESTURE_MOTION_HORIZONTAL.into(),
    );
    CGEvent::set_double_value_field(e, FIELD_GESTURE_SWIPE_VELOCITY_X, sign * velocity);
    CGEvent::set_double_value_field(e, FIELD_GESTURE_SWIPE_VELOCITY_Y, sign * velocity);
    CGEvent::post(CGEventTapLocation::SessionEventTap, e);
}

/// (progress, velocity_x) for one phase of a macOS 27 swipe.
fn modern_params(phase: i64, sign: f64) -> (f64, f64) {
    // Only the Ended phase flings, as a real trackpad swipe does
    let velocity = if phase == PHASE_ENDED {
        sign * MODERN_FLING_VELOCITY
    } else {
        0.0
    };
    (sign * SWIPE_PROGRESS, velocity)
}

fn post_modern(phase: i64, sign: f64) -> bool {
    let (progress, velocity) = modern_params(phase, sign);

    let Some(ev) = CGEvent::new(None) else {
        return false;
    };
    let e = Some(&*ev);
    CGEvent::set_integer_value_field(e, FIELD_CGS_EVENT_TYPE, CGS_EVENT_DOCK_CONTROL);
    CGEvent::set_integer_value_field(e, FIELD_GESTURE_HID_TYPE, IO_HID_EVENT_TYPE_DOCK_SWIPE);
    CGEvent::set_integer_value_field(e, FIELD_GESTURE_PHASE, phase);
    CGEvent::set_integer_value_field(
        e,
        FIELD_GESTURE_SWIPE_MOTION,
        CGS_GESTURE_MOTION_HORIZONTAL.into(),
    );
    CGEvent::set_double_value_field(e, FIELD_GESTURE_SWIPE_PROGRESS, progress);
    CGEvent::set_integer_value_field(e, FIELD_GESTURE_PHASE_ALIAS, phase);
    CGEvent::set_double_value_field(e, FIELD_GESTURE_ZOOM_DELTA_Y, 3.0);
    CGEvent::set_double_value_field(e, FIELD_SOURCE_PID_ALIAS, mach_absolute_time() as f64);
    CGEvent::set_double_value_field(e, FIELD_GESTURE_SWIPE_POSITION_X, MODERN_POSITION_X);
    if velocity != 0.0 {
        CGEvent::set_double_value_field(e, FIELD_GESTURE_SWIPE_VELOCITY_X, velocity);
    }

    let mut timestamp = CGEvent::timestamp(e);
    if timestamp == 0 {
        timestamp = mach_absolute_time();
    }
    let payload = iohid_payload(phase, progress, velocity, timestamp);
    let Some(augmented) = augment(&ev, &payload) else {
        return false;
    };

    // The Dock ignores a DockControl event without a companion gesture event
    let Some(companion) = CGEvent::new(None) else {
        return false;
    };
    CGEvent::set_integer_value_field(Some(&companion), FIELD_CGS_EVENT_TYPE, CGS_EVENT_GESTURE);

    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&augmented));
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&companion));
    true
}

/// Re-create `event` from its serialized form with `payload` appended under
/// tag field 4205, which is where WindowServer looks for the raw IOHID data.
fn augment(event: &CGEvent, payload: &[u8]) -> Option<CFRetained<CGEvent>> {
    let bytes = CGEvent::new_data(None, Some(event))?.to_vec();
    if bytes.get(..4) != Some(&[0, 0, 0, 2]) {
        log::warn!("unexpected CGEvent serialization format; cannot augment dock swipe");
        return None;
    }
    let len = u16::try_from(payload.len()).expect("IOHID payload is under 100 bytes");

    let mut out = bytes;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&IOHID_PAYLOAD_TAG_FIELD.to_be_bytes());
    out.extend_from_slice(payload);
    CGEvent::from_data(None, Some(&CFData::from_bytes(&out)))
}

fn to_fixed_16_16(v: f64) -> i32 {
    let fixed = (v * 65536.0) as i32;
    if fixed == 0 && v != 0.0 {
        if v > 0.0 { 1 } else { -1 }
    } else {
        fixed
    }
}

/// An IOHIDSystemQueueElement: header, a FluidTouchGesture event and, on the
/// Ended phase, a Velocity event. Packed, little-endian.
fn iohid_payload(phase: i64, progress: f64, velocity_x: f64, timestamp: u64) -> Vec<u8> {
    let with_velocity = phase == PHASE_ENDED;
    let mut p = Vec::with_capacity(96);

    // IOHIDSystemQueueElementHeader
    p.extend_from_slice(&timestamp.to_le_bytes());
    p.extend_from_slice(&0u64.to_le_bytes()); // sender_id
    p.extend_from_slice(&0u32.to_le_bytes()); // options
    p.extend_from_slice(&0u32.to_le_bytes()); // attribute_length
    p.extend_from_slice(&(if with_velocity { 2u32 } else { 1 }).to_le_bytes());

    // IOHIDFluidTouchGestureData
    p.extend_from_slice(&FLUID_EVENT_LEN.to_le_bytes());
    p.extend_from_slice(&IOHID_EVENT_TYPE_FLUID_TOUCH_GESTURE.to_le_bytes());
    p.extend_from_slice(&(((phase & 0xFF) as u32) << 24).to_le_bytes()); // options
    p.extend_from_slice(&[0u8; 4]); // depth 0, reserved
    p.extend_from_slice(&to_fixed_16_16(MODERN_POSITION_X).to_le_bytes());
    p.extend_from_slice(&[0u8; 12]); // position_y, position_z, swipe_mask
    p.extend_from_slice(&CGS_GESTURE_MOTION_HORIZONTAL.to_le_bytes());
    p.extend_from_slice(&IOHID_GESTURE_FLAVOR_DOCK_PRIMARY.to_le_bytes());
    p.extend_from_slice(&to_fixed_16_16(progress).to_le_bytes());

    if with_velocity {
        // IOHIDVelocityEventData
        p.extend_from_slice(&VELOCITY_EVENT_LEN.to_le_bytes());
        p.extend_from_slice(&IOHID_EVENT_TYPE_VELOCITY.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes()); // options
        p.extend_from_slice(&[1u8, 0, 0, 0]); // depth 1, reserved
        p.extend_from_slice(&to_fixed_16_16(velocity_x).to_le_bytes());
        p.extend_from_slice(&[0u8; 8]); // velocity_y, velocity_z
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes from ISS's `iss_generate_iohid_payload` (C) at timestamp
    /// 123456789, for each sign and phase.
    #[test]
    fn modern_swipe_payload_matches_iss() {
        let cases = [
            (
                -1.0,
                [
                    "15cd5b07000000000000000000000000000000000000000001000000280000001700000000000001000000009919000000000000000000000000000001000300ffffffff",
                    "15cd5b07000000000000000000000000000000000000000001000000280000001700000000000002000000009919000000000000000000000000000001000300ffffffff",
                    "15cd5b07000000000000000000000000000000000000000002000000280000001700000000000004000000009919000000000000000000000000000001000300ffffffff1c0000000900000000000000010000000000f1d80000000000000000",
                ],
            ),
            (
                1.0,
                [
                    "15cd5b0700000000000000000000000000000000000000000100000028000000170000000000000100000000991900000000000000000000000000000100030001000000",
                    "15cd5b0700000000000000000000000000000000000000000100000028000000170000000000000200000000991900000000000000000000000000000100030001000000",
                    "15cd5b07000000000000000000000000000000000000000002000000280000001700000000000004000000009919000000000000000000000000000001000300010000001c00000009000000000000000100000000000f270000000000000000",
                ],
            ),
        ];
        let ev = CGEvent::new(None).unwrap();
        for (sign, expected) in cases {
            for (phase, want) in PHASES.into_iter().zip(expected) {
                let (progress, velocity) = modern_params(phase, sign);
                let payload = iohid_payload(phase, progress, velocity, 123456789);
                let got: String = payload.iter().map(|b| format!("{b:02x}")).collect();
                assert_eq!(got, want, "sign {sign} phase {phase}");
                assert!(
                    augment(&ev, &payload).is_some(),
                    "sign {sign} phase {phase}"
                );
            }
        }
    }
}
