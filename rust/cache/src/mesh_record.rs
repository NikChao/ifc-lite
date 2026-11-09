// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! One mesh inside a geometry chunk, mirroring `writeMeshRecord` in
//! `sections/geometry.js` at format version 25.
//!
//! Layout, in order:
//!   expressId u32, vertexCount u32, indexCount u32,
//!   color f32x4,
//!   ifcType: length u32 + UTF-8 bytes,
//!   geometryClass u8,
//!   geometryItemId u32, materialId u32   (0xFFFFFFFF = absent)
//!   origin f64x3,
//!   positions f32[vertexCount*3], normals f32[vertexCount*3],
//!   indices u32[indexCount],
//!   provenance: mode u8 (+ payload when mode >= 2),
//!   finish: metallic f32, roughness f32   (NaN = absent)
//!   metadata: flags u8 (+ the fields the flags claim)
//!
//! The record is positional and fixed-shape up to the trailers: the v13->v14
//! note in the format history spells out that a reader which misses the two
//! source ids takes the origin's first f64 out of them and garbles every field
//! after. So every field is written unconditionally, with sentinels for absence
//! rather than omission.

use crate::buf::Buf;
use crate::frame;

/// Absent source id. Deliberately not 0: `#0` is not a STEP instance name, but
/// an unreferenced material layer can decode as id 0, so an absence marker the
/// domain can produce would be one upstream change from being wrong.
pub const ABSENT_SOURCE_ID: u32 = 0xFFFF_FFFF;

mod metadata_flags {
    pub const SHADING_COLOR: u8 = 1;
    pub const LOCAL_BOUNDS: u8 = 2;
    pub const LOCAL_TO_WORLD: u8 = 4;
}

/// The fields of a mesh this writer serialises. Borrowed rather than owned: the
/// record is written straight out of the pipeline's `MeshData` as each batch
/// arrives, so nothing is copied and nothing is retained.
pub struct MeshRecord<'a> {
    pub express_id: u32,
    pub ifc_type: &'a str,
    pub positions: &'a [f32],
    pub normals: &'a [f32],
    pub indices: &'a [u32],
    pub color: [f32; 4],
    pub geometry_class: u8,
    pub geometry_item_id: Option<u32>,
    pub material_id: Option<u32>,
    pub origin: [f64; 3],
    pub local_bounds: Option<[f32; 6]>,
    pub local_to_world: Option<[f64; 16]>,
}

impl MeshRecord<'_> {
    /// Exact serialised length, used to cap a chunk before building it so the
    /// cap is applied without first serialising the mesh twice.
    pub fn byte_length(&self) -> usize {
        let vertex_count = self.positions.len() / 3;
        4 + 4 + 4                       // expressId, vertexCount, indexCount
            + 16                        // color
            + 4 + self.ifc_type.len()   // ifcType
            + 1                         // geometryClass
            + 4 + 4                     // geometryItemId, materialId
            + 24                        // origin
            + vertex_count * 3 * 4 * 2  // positions + normals
            + self.indices.len() * 4
            + 1                         // provenance mode 0
            + 8                         // finish
            + 1                         // metadata flags
            + self.local_bounds.map_or(0, |_| 24)
            + self.local_to_world.map_or(0, |_| 128)
    }

    pub fn write(&self, buf: &mut Buf) {
        let vertex_count = (self.positions.len() / 3) as u32;

        buf.u32(self.express_id);
        buf.u32(vertex_count);
        buf.u32(self.indices.len() as u32);

        for channel in self.color {
            buf.f32(channel);
        }

        buf.u32(self.ifc_type.len() as u32);
        buf.bytes(self.ifc_type.as_bytes());

        buf.u8(self.geometry_class);
        buf.u32(self.geometry_item_id.unwrap_or(ABSENT_SOURCE_ID));
        buf.u32(self.material_id.unwrap_or(ABSENT_SOURCE_ID));

        // Every vertex, normal and origin leaves in the viewer's Y-up frame;
        // the pipeline produced them in IFC Z-up. See `frame`.
        for axis in frame::point_f64(self.origin) {
            buf.f64(axis);
        }

        for vertex in self.positions.chunks_exact(3) {
            for axis in frame::point_f32(vertex[0], vertex[1], vertex[2]) {
                buf.f32(axis);
            }
        }
        for normal in self.normals.chunks_exact(3) {
            for axis in frame::point_f32(normal[0], normal[1], normal[2]) {
                buf.f32(axis);
            }
        }
        for value in self.indices {
            buf.u32(*value);
        }

        // Appearance provenance mode 0: no per-corner source indices. The
        // reader stops at the mode byte for anything below 2, so no payload
        // follows. The native pipeline carries provenance on a separate field
        // this writer does not yet read.
        buf.u8(0);

        // IFC-authored finish (v22+). NaN means the model did not state one,
        // which is distinct from a stated zero -- a zero roughness is a mirror.
        buf.f32(f32::NAN);
        buf.f32(f32::NAN);

        self.write_metadata(buf);
    }

    /// The v25 trailer. Each field is announced by a flag and written only when
    /// present, so a mesh carrying none of them costs one byte.
    fn write_metadata(&self, buf: &mut Buf) {
        let mut flags = 0u8;
        if self.local_bounds.is_some() {
            flags |= metadata_flags::LOCAL_BOUNDS;
        }
        if self.local_to_world.is_some() {
            flags |= metadata_flags::LOCAL_TO_WORLD;
        }
        buf.u8(flags);

        // SHADING_COLOR is never set: the native pipeline reports one colour,
        // which is already written above. Claiming the flag without four more
        // floats would shift every following field.
        debug_assert_eq!(flags & metadata_flags::SHADING_COLOR, 0);

        if let Some(bounds) = self.local_bounds {
            let (min, max) = frame::aabb_f32(
                [bounds[0], bounds[1], bounds[2]],
                [bounds[3], bounds[4], bounds[5]],
            );
            for value in min.into_iter().chain(max) {
                buf.f32(value);
            }
        }
        if let Some(matrix) = self.local_to_world {
            for value in frame::matrix_f64(matrix) {
                buf.f64(value);
            }
        }
    }

    /// Grid cell of the mesh's world anchor, matching `cellKeyOf`. A grouping
    /// heuristic only -- readers trust the directory, never this -- but it is
    /// what makes a chunk spatially coherent.
    pub fn cell_key(&self, cell_size: f64) -> (i64, i64, i64) {
        let has_vertex = self.positions.len() >= 3;
        // In the viewer frame, because the directory's boxes are read there.
        let origin = frame::point_f64(self.origin);
        let first = if has_vertex {
            frame::point_f32(self.positions[0], self.positions[1], self.positions[2])
        } else {
            [0.0; 3]
        };
        let anchor = |axis: usize| origin[axis] + first[axis] as f64;
        let cell = |value: f64| {
            if value.is_finite() {
                (value / cell_size).floor() as i64
            } else {
                i64::MIN
            }
        };
        (cell(anchor(0)), cell(anchor(1)), cell(anchor(2)))
    }

    /// World-space bounds, which the chunk directory aggregates.
    pub fn world_bounds(&self) -> ([f32; 3], [f32; 3]) {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let origin = frame::point_f64(self.origin);
        for vertex in self.positions.chunks_exact(3) {
            let viewer = frame::point_f32(vertex[0], vertex[1], vertex[2]);
            for axis in 0..3 {
                let value = origin[axis] as f32 + viewer[axis];
                if value < min[axis] {
                    min[axis] = value;
                }
                if value > max[axis] {
                    max[axis] = value;
                }
            }
        }
        (min, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>(ifc_type: &'a str) -> MeshRecord<'a> {
        MeshRecord {
            express_id: 42,
            ifc_type,
            positions: &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            normals: &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            indices: &[0, 1, 2],
            color: [1.0, 0.5, 0.25, 1.0],
            geometry_class: 0,
            geometry_item_id: None,
            material_id: None,
            origin: [10.0, 20.0, 30.0],
            local_bounds: None,
            local_to_world: None,
        }
    }

    #[test]
    fn byte_length_matches_what_is_written() {
        let mesh = sample("IfcWall");
        let mut buf = Buf::new();
        mesh.write(&mut buf);
        assert_eq!(buf.len(), mesh.byte_length());
    }

    #[test]
    fn byte_length_still_matches_with_the_metadata_trailer() {
        let mut mesh = sample("IfcSlab");
        mesh.local_bounds = Some([0.0, 0.0, 0.0, 1.0, 1.0, 1.0]);
        mesh.local_to_world = Some([0.0; 16]);
        let mut buf = Buf::new();
        mesh.write(&mut buf);
        assert_eq!(buf.len(), mesh.byte_length());
    }

    #[test]
    fn absent_source_ids_use_the_sentinel_not_zero() {
        let mesh = sample("IfcWall");
        let mut buf = Buf::new();
        mesh.write(&mut buf);
        let bytes = buf.into_vec();
        // expressId(4) + counts(8) + color(16) + ifcType(4 + 7) + class(1) = 40
        let at = 40;
        assert_eq!(u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()), ABSENT_SOURCE_ID);
        assert_eq!(
            u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()),
            ABSENT_SOURCE_ID
        );
    }

    #[test]
    fn cell_key_anchors_on_origin_plus_first_vertex() {
        let mesh = sample("IfcWall");
        // IFC origin (10,20,30) is viewer (10,30,-20): the anchor is in the
        // frame the chunk directory is read in, so -20 falls in cell -1.
        assert_eq!(mesh.cell_key(32.0), (0, 0, -1));
    }

    #[test]
    fn world_bounds_are_in_the_viewer_frame() {
        let mesh = sample("IfcWall");
        let (min, max) = mesh.world_bounds();
        // IFC origin (10,20,30) -> (10,30,-20); the 1m extents along IFC x and y
        // become x and -z.
        assert_eq!(min, [10.0, 30.0, -21.0]);
        assert_eq!(max, [11.0, 30.0, -20.0]);
    }
}
