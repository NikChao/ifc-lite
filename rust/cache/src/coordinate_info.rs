// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The coordinate frame the meshes are expressed in, mirroring
//! `sections/coordinate-info.js`.
//!
//! Layout: originShift f64x3, originalBounds (min f64x3, max f64x3),
//! shiftedBounds (min f64x3, max f64x3), hasLargeCoordinates u8,
//! then four optional tails, each a presence byte followed by its payload only
//! when present:
//!   wasmRtcOffset  -> f64x3
//!   buildingRotation -> f64
//!   wasmRtcFrame (v20+) -> f64x3 + needsShift u8
//!   boundsRecoveryFallbackCount (v24+) -> f64
//!   lengthUnitScale (v24+) -> f64
//!
//! The presence bytes are what make this section variable-length, so the head
//! that contains it cannot be sized in advance -- which is why the geometry
//! section writes `headLength` first rather than assuming a fixed offset.
//!
//! `wasmRtcFrame` is deliberately distinct from `wasmRtcOffset`: absent means
//! unknown, while present-with-a-zero-frame is a statement that the producer
//! applied no re-base. Writing one for the other loses that distinction.

use crate::buf::Buf;

#[derive(Clone, Copy, Default)]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

#[derive(Clone, Default)]
pub struct CoordinateInfo {
    pub origin_shift: [f64; 3],
    pub original_bounds: Aabb,
    pub shifted_bounds: Aabb,
    pub has_large_coordinates: bool,
    pub wasm_rtc_offset: Option<[f64; 3]>,
    pub building_rotation: Option<f64>,
    pub length_unit_scale: Option<f64>,
}

impl CoordinateInfo {
    pub fn write(&self, buf: &mut Buf) {
        write_vec3(buf, &self.origin_shift);
        write_aabb(buf, &self.original_bounds);
        write_aabb(buf, &self.shifted_bounds);
        buf.u8(u8::from(self.has_large_coordinates));

        match self.wasm_rtc_offset {
            Some(offset) => {
                buf.u8(1);
                write_vec3(buf, &offset);
            }
            None => buf.u8(0),
        }

        match self.building_rotation {
            Some(rotation) => {
                buf.u8(1);
                buf.f64(rotation);
            }
            None => buf.u8(0),
        }

        // wasmRtcFrame is left absent rather than synthesised from
        // `wasm_rtc_offset`: the two mean different things, and a fabricated
        // frame would assert producer provenance this writer cannot vouch for.
        buf.u8(0);

        // boundsRecoveryFallbackCount: absent is "not recorded", which is the
        // honest answer -- a written zero would claim no fallbacks occurred.
        buf.u8(0);

        write_optional_number(buf, self.length_unit_scale);
    }
}

fn write_vec3(buf: &mut Buf, value: &[f64; 3]) {
    for axis in value {
        buf.f64(*axis);
    }
}

fn write_aabb(buf: &mut Buf, value: &Aabb) {
    write_vec3(buf, &value.min);
    write_vec3(buf, &value.max);
}

fn write_optional_number(buf: &mut Buf, value: Option<f64>) {
    match value {
        Some(value) => {
            buf.u8(1);
            buf.f64(value);
        }
        None => buf.u8(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED: usize = 24 + 48 + 48 + 1;

    #[test]
    fn an_empty_frame_writes_only_presence_bytes_after_the_fixed_part() {
        let mut buf = Buf::new();
        CoordinateInfo::default().write(&mut buf);
        // Five optional tails, all absent: rtcOffset, rotation, rtcFrame,
        // recoveryCount, unitScale.
        assert_eq!(buf.len(), FIXED + 5);
    }

    #[test]
    fn present_tails_carry_their_payload() {
        let info = CoordinateInfo {
            wasm_rtc_offset: Some([1.0, 2.0, 3.0]),
            building_rotation: Some(0.5),
            length_unit_scale: Some(0.001),
            ..CoordinateInfo::default()
        };
        let mut buf = Buf::new();
        info.write(&mut buf);
        assert_eq!(buf.len(), FIXED + 5 + 24 + 8 + 8);
    }

    #[test]
    fn large_coordinate_flag_is_a_single_byte() {
        let info = CoordinateInfo {
            has_large_coordinates: true,
            ..CoordinateInfo::default()
        };
        let mut buf = Buf::new();
        info.write(&mut buf);
        assert_eq!(buf.as_slice()[24 + 48 + 48], 1);
    }
}
