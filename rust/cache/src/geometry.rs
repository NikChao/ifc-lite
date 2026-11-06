// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The v13 chunked geometry section, mirroring `sections/geometry-chunks.js`.
//!
//! Section layout, offsets relative to the section start:
//!   headLength u32            // bytes from here to the first chunk record
//!   meshCount u32, totalVertices u32, totalTriangles u32
//!   coordinateInfo            // variable length
//!   sourceIndexPool           // count u32, then length-prefixed u32 arrays
//!   chunkCount u32
//!   directory[chunkCount]     // 44 bytes each
//!   ...chunk records at their recorded offsets
//!
//! A directory entry is aabbMin f32x3, aabbMax f32x3, byteOffset u32,
//! byteLength u32 (as stored, so compressed when it is), uncompressedLength u32,
//! meshCount u32, flags u32.
//!
//! ## Why this streams through a scratch file
//!
//! The directory sits in the head, before the chunks, but a chunk's offset is
//! only known once every earlier chunk has been built -- and chunk boundaries
//! are only known once the meshes have arrived. Holding every mesh to resolve
//! that is the 6.2 GiB the native path exists to avoid.
//!
//! So chunks are written to a scratch file as they close, collecting directory
//! entries as they go. When the stream ends, the head is written to the real
//! section and the scratch file is copied in behind it. Peak memory is one
//! open chunk per spatial cell plus the directory, and Lambda's ephemeral disk
//! absorbs the rest.

use std::collections::HashMap;
use std::io::{Read, Result, Seek, SeekFrom, Write};

use flate2::write::DeflateEncoder;
use flate2::Compression;

use crate::buf::Buf;
use crate::coordinate_info::CoordinateInfo;
use crate::mesh_record::MeshRecord;

pub const CELL_SIZE: f64 = 32.0;
pub const SOFT_BYTES: usize = 8 * 1024 * 1024;
/// Below this a chunk is stored raw: deflate costs more time than it saves.
pub const COMPRESS_MIN_BYTES: usize = 64 * 1024;
const DIRECTORY_ENTRY_BYTES: usize = 44;

mod chunk_flags {
    pub const NONE: u32 = 0;
    pub const DEFLATE_RAW: u32 = 1;
}

struct DirectoryEntry {
    aabb_min: [f32; 3],
    aabb_max: [f32; 3],
    byte_offset: u32,
    byte_length: u32,
    uncompressed_length: u32,
    mesh_count: u32,
    flags: u32,
}

/// A chunk still accepting meshes, one per occupied spatial cell.
struct OpenChunk {
    bytes: Buf,
    mesh_count: u32,
    aabb_min: [f32; 3],
    aabb_max: [f32; 3],
}

impl OpenChunk {
    fn new() -> Self {
        OpenChunk {
            bytes: Buf::new(),
            mesh_count: 0,
            aabb_min: [f32::INFINITY; 3],
            aabb_max: [f32::NEG_INFINITY; 3],
        }
    }

    fn absorb(&mut self, mesh: &MeshRecord<'_>) {
        let (min, max) = mesh.world_bounds();
        for axis in 0..3 {
            if min[axis] < self.aabb_min[axis] {
                self.aabb_min[axis] = min[axis];
            }
            if max[axis] > self.aabb_max[axis] {
                self.aabb_max[axis] = max[axis];
            }
        }
        mesh.write(&mut self.bytes);
        self.mesh_count += 1;
    }
}

/// Accepts meshes as they stream out of the pipeline and writes closed chunks
/// to `scratch`, which the caller later copies in behind the head.
pub struct GeometryStream<W: Write> {
    scratch: W,
    scratch_len: u64,
    open: HashMap<(i64, i64, i64), OpenChunk>,
    directory: Vec<DirectoryEntry>,
    pub mesh_count: u64,
    pub total_vertices: u64,
    pub total_triangles: u64,
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
}

impl<W: Write> GeometryStream<W> {
    pub fn new(scratch: W) -> Self {
        GeometryStream {
            scratch,
            scratch_len: 0,
            open: HashMap::new(),
            directory: Vec::new(),
            mesh_count: 0,
            total_vertices: 0,
            total_triangles: 0,
            bounds_min: [f32::INFINITY; 3],
            bounds_max: [f32::NEG_INFINITY; 3],
        }
    }

    /// World bounds of everything seen so far, which the coordinate info
    /// reports. Tracked here rather than recomputed, because the meshes are
    /// gone by the time the section head is written.
    pub fn world_bounds(&self) -> ([f64; 3], [f64; 3]) {
        let min = finite_or_zero(self.bounds_min);
        let max = finite_or_zero(self.bounds_max);
        (
            [min[0] as f64, min[1] as f64, min[2] as f64],
            [max[0] as f64, max[1] as f64, max[2] as f64],
        )
    }

    pub fn push(&mut self, mesh: &MeshRecord<'_>) -> Result<()> {
        self.mesh_count += 1;
        self.total_vertices += (mesh.positions.len() / 3) as u64;
        self.total_triangles += (mesh.indices.len() / 3) as u64;

        let (min, max) = mesh.world_bounds();
        for axis in 0..3 {
            if min[axis] < self.bounds_min[axis] {
                self.bounds_min[axis] = min[axis];
            }
            if max[axis] > self.bounds_max[axis] {
                self.bounds_max[axis] = max[axis];
            }
        }

        let key = mesh.cell_key(CELL_SIZE);
        let incoming = mesh.byte_length();

        // Close the cell's chunk before it would exceed the cap, so a mesh is
        // never split across chunks.
        if let Some(open) = self.open.get(&key) {
            if open.bytes.len() > 0 && open.bytes.len() + incoming > SOFT_BYTES {
                let closing = self.open.remove(&key).expect("just checked");
                self.close(closing)?;
            }
        }

        self.open.entry(key).or_insert_with(OpenChunk::new).absorb(mesh);
        Ok(())
    }

    /// Closes every open chunk and returns the head, ready to be written before
    /// the scratch contents. `head_offset` is where the section begins, because
    /// directory offsets are relative to the section rather than the file.
    pub fn finish(mut self, coordinate_info: &CoordinateInfo) -> Result<(Vec<u8>, W)> {
        let keys: Vec<_> = self.open.keys().copied().collect();
        for key in keys {
            let closing = self.open.remove(&key).expect("key came from the map");
            if closing.mesh_count > 0 {
                self.close(closing)?;
            }
        }

        let head = self.build_head(coordinate_info);
        // Chunks were written to scratch at offsets relative to the scratch
        // file; the head's length shifts every one of them into section space.
        Ok((head, self.scratch))
    }

    fn close(&mut self, chunk: OpenChunk) -> Result<()> {
        let raw = chunk.bytes.into_vec();
        let uncompressed_length = raw.len() as u32;

        let (stored, flags) = if raw.len() >= COMPRESS_MIN_BYTES {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&raw)?;
            let deflated = encoder.finish()?;
            // Only keep the compressed form when it actually helps; a chunk of
            // incompressible float data can come out larger.
            if deflated.len() < raw.len() {
                (deflated, chunk_flags::DEFLATE_RAW)
            } else {
                (raw, chunk_flags::NONE)
            }
        } else {
            (raw, chunk_flags::NONE)
        };

        self.scratch.write_all(&stored)?;
        self.directory.push(DirectoryEntry {
            aabb_min: finite_or_zero(chunk.aabb_min),
            aabb_max: finite_or_zero(chunk.aabb_max),
            byte_offset: self.scratch_len as u32,
            byte_length: stored.len() as u32,
            uncompressed_length,
            mesh_count: chunk.mesh_count,
            flags,
        });
        self.scratch_len += stored.len() as u64;
        Ok(())
    }

    fn build_head(&self, coordinate_info: &CoordinateInfo) -> Vec<u8> {
        let mut body = Buf::new();
        body.u32(self.mesh_count as u32);
        body.u32(self.total_vertices as u32);
        body.u32(self.total_triangles as u32);
        coordinate_info.write(&mut body);
        // Empty appearance source pool: every mesh record writes provenance
        // mode 0, so no record refers to a pooled index array.
        body.u32(0);
        body.u32(self.directory.len() as u32);

        // headLength is everything BELOW the field itself -- the reader computes
        // `headBytes + directoryBytes` and places the first chunk at
        // `4 + headLength`. Counting the field here declares four bytes more
        // than the reader parses, and it rejects the section for the mismatch.
        let head_length = body.len() + self.directory.len() * DIRECTORY_ENTRY_BYTES;

        let mut head = Buf::with_capacity(4 + head_length);
        head.u32(head_length as u32);
        head.bytes(body.as_slice());
        for entry in &self.directory {
            for axis in entry.aabb_min {
                head.f32(axis);
            }
            for axis in entry.aabb_max {
                head.f32(axis);
            }
            head.u32(entry.byte_offset + 4 + head_length as u32);
            head.u32(entry.byte_length);
            head.u32(entry.uncompressed_length);
            head.u32(entry.mesh_count);
            head.u32(entry.flags);
        }

        debug_assert_eq!(head.len(), 4 + head_length);
        head.into_vec()
    }
}

/// A chunk whose meshes carried no vertices leaves the bounds at infinity,
/// which is not a value a reader can intersect against.
fn finite_or_zero(value: [f32; 3]) -> [f32; 3] {
    if value.iter().all(|axis| axis.is_finite()) {
        value
    } else {
        [0.0; 3]
    }
}

/// Copies a finished scratch file into the section behind its head.
pub fn append_scratch<R: Read + Seek, W: Write>(scratch: &mut R, out: &mut W) -> Result<u64> {
    scratch.seek(SeekFrom::Start(0))?;
    std::io::copy(scratch, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn mesh<'a>(express_id: u32, origin: [f64; 3], positions: &'a [f32], indices: &'a [u32]) -> MeshRecord<'a> {
        MeshRecord {
            express_id,
            ifc_type: "IfcWall",
            positions,
            normals: positions,
            indices,
            color: [1.0, 1.0, 1.0, 1.0],
            geometry_class: 0,
            geometry_item_id: None,
            material_id: None,
            origin,
            local_bounds: None,
            local_to_world: None,
        }
    }

    #[test]
    fn meshes_far_apart_land_in_separate_chunks() {
        let positions = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let indices = [0u32, 1, 2];
        let mut stream = GeometryStream::new(Cursor::new(Vec::new()));
        stream.push(&mesh(1, [0.0, 0.0, 0.0], &positions, &indices)).unwrap();
        // 100m away is a different 32m cell.
        stream.push(&mesh(2, [100.0, 0.0, 0.0], &positions, &indices)).unwrap();

        let (head, _) = stream.finish(&CoordinateInfo::default()).unwrap();
        let chunk_count_at = 4 + 12 + (24 + 48 + 48 + 1 + 5) + 4;
        let chunk_count = u32::from_le_bytes(head[chunk_count_at..chunk_count_at + 4].try_into().unwrap());
        assert_eq!(chunk_count, 2);
    }

    #[test]
    fn meshes_in_one_cell_share_a_chunk() {
        let positions = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let indices = [0u32, 1, 2];
        let mut stream = GeometryStream::new(Cursor::new(Vec::new()));
        stream.push(&mesh(1, [0.0, 0.0, 0.0], &positions, &indices)).unwrap();
        // IFC (1,0,1) is viewer (1,1,0): a positive IFC y would land at a
        // negative viewer z and so in the next cell down.
        stream.push(&mesh(2, [1.0, 0.0, 1.0], &positions, &indices)).unwrap();

        assert_eq!(stream.mesh_count, 2);
        assert_eq!(stream.total_vertices, 6);
        assert_eq!(stream.total_triangles, 2);

        let (head, _) = stream.finish(&CoordinateInfo::default()).unwrap();
        let chunk_count_at = 4 + 12 + (24 + 48 + 48 + 1 + 5) + 4;
        assert_eq!(
            u32::from_le_bytes(head[chunk_count_at..chunk_count_at + 4].try_into().unwrap()),
            1
        );
    }

    #[test]
    fn head_length_locates_the_first_chunk() {
        let positions = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let indices = [0u32, 1, 2];
        let mut stream = GeometryStream::new(Cursor::new(Vec::new()));
        stream.push(&mesh(1, [0.0, 0.0, 0.0], &positions, &indices)).unwrap();

        let (head, _) = stream.finish(&CoordinateInfo::default()).unwrap();
        let head_length = u32::from_le_bytes(head[0..4].try_into().unwrap()) as usize;
        // headLength excludes its own field: the reader adds the 4 back to find
        // the first chunk. Asserting equality with `head.len()` is what let a
        // four-byte overstatement ship and get rejected on load.
        assert_eq!(head_length + 4, head.len(), "headLength counts everything after itself");

        let entry_at = head.len() - DIRECTORY_ENTRY_BYTES;
        let byte_offset = u32::from_le_bytes(head[entry_at + 24..entry_at + 28].try_into().unwrap());
        assert_eq!(byte_offset as usize, head.len(), "the first chunk begins where the head ends");
    }

    #[test]
    fn small_chunks_are_stored_raw() {
        let positions = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let indices = [0u32, 1, 2];
        let mut stream = GeometryStream::new(Cursor::new(Vec::new()));
        stream.push(&mesh(1, [0.0, 0.0, 0.0], &positions, &indices)).unwrap();

        let (head, _) = stream.finish(&CoordinateInfo::default()).unwrap();
        let entry_at = head.len() - DIRECTORY_ENTRY_BYTES;
        let flags = u32::from_le_bytes(head[entry_at + 40..entry_at + 44].try_into().unwrap());
        assert_eq!(flags, chunk_flags::NONE);

        let stored = u32::from_le_bytes(head[entry_at + 28..entry_at + 32].try_into().unwrap());
        let uncompressed = u32::from_le_bytes(head[entry_at + 32..entry_at + 36].try_into().unwrap());
        assert_eq!(stored, uncompressed, "an uncompressed chunk stores its full length");
    }
}
