// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A streaming writer for the `.ifclite` binary cache, format version 25.
//!
//! `@ifc-lite/cache` builds every section in memory and concatenates them. On a
//! federated model the mesh set alone is 6.2 GiB, which is the whole Lambda
//! budget before a byte is written, so this writer never holds the file: each
//! section goes to disk as it is produced and the header and section table are
//! backfilled once the sizes are known.
//!
//! The format is positional and the viewer's reader is strict -- a field written
//! at the wrong offset is read as garbage rather than rejected -- so each
//! section module mirrors its counterpart in `@ifc-lite/cache/dist/sections`
//! field for field, and the layout each one implements is stated above it.

pub mod buf;
pub mod build;
pub mod entities;
pub mod coordinate_info;
pub mod frame;
pub mod geometry;
pub mod header;
pub mod index_map;
pub mod properties;
pub mod mesh_record;
pub mod quantities;
pub mod relationships;
pub mod strings;
pub mod tables;
pub mod type_enum;

use std::fs::File;
use std::io::{Result, Seek, SeekFrom, Write};

/// "IFCL", little-endian.
pub const MAGIC: u32 = 0x4C43_4649;
pub const FORMAT_VERSION: u16 = 25;
pub const HEADER_SIZE: usize = 64;
pub const SECTION_ENTRY_SIZE: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u16)]
pub enum SectionType {
    Strings = 1,
    Entities = 2,
    Properties = 3,
    Quantities = 4,
    Relationships = 5,
    Geometry = 6,
    EntityIndex = 9,
    InstancedShards = 10,
}

pub mod header_flags {
    /// Every value here is read by `packages/cache/src/types.ts`'s
    /// `HeaderFlags`, so the two must name the same bit. They did not: this
    /// module had `SOURCE_HASH_UNSET` at bit 4 while TS has it at bit 3, so a
    /// natively built cache told the reader its zeroed `sourceHash` was a real
    /// hash and `read({ sourceBuffer })` threw "source file has changed" on a
    /// perfectly good cache.
    pub const HAS_GEOMETRY: u16 = 1 << 1;
    /// `sourceHash` is unset rather than a real xxhash64 of the source. The
    /// reader skips validation instead of fail-closing on a hash of zero.
    pub const SOURCE_HASH_UNSET: u16 = 1 << 3;
}

#[derive(Clone, Copy)]
pub struct SectionEntry {
    pub section_type: SectionType,
    pub offset: u32,
    pub size: u32,
}

/// Writes sections to the file in order, remembering where each one landed.
///
/// The section table sits between the header and the first section, so its size
/// depends on how many sections there will be. The caller declares that count up
/// front; the space is reserved, and `finish` seeks back to fill it in.
pub struct CacheWriter {
    file: File,
    offset: u64,
    sections: Vec<SectionEntry>,
    declared_sections: usize,
}

impl CacheWriter {
    pub fn create(file: File, declared_sections: usize) -> Result<Self> {
        let reserved = HEADER_SIZE + declared_sections * SECTION_ENTRY_SIZE;
        let mut writer = CacheWriter {
            file,
            offset: reserved as u64,
            sections: Vec::with_capacity(declared_sections),
            declared_sections,
        };
        // The reserved span is rewritten by `finish`; writing it now puts the
        // file position at the first section without a seek.
        writer.file.write_all(&vec![0u8; reserved])?;
        Ok(writer)
    }

    /// Appends one whole section. Used for the sections small enough to build in
    /// memory -- everything except geometry, which streams through `section_stream`.
    pub fn section(&mut self, section_type: SectionType, bytes: &[u8]) -> Result<()> {
        self.file.write_all(bytes)?;
        self.sections.push(SectionEntry {
            section_type,
            offset: self.offset as u32,
            size: bytes.len() as u32,
        });
        self.offset += bytes.len() as u64;
        Ok(())
    }

    /// Opens a section the caller fills incrementally. The section's size is
    /// whatever was written by the time the returned guard is finished.
    pub fn section_stream(&mut self, section_type: SectionType) -> SectionStream<'_> {
        SectionStream {
            start: self.offset,
            section_type,
            writer: self,
        }
    }

    pub fn finish(mut self, header: Header) -> Result<u64> {
        assert_eq!(
            self.sections.len(),
            self.declared_sections,
            "declared {} sections, wrote {}: the reserved section table would not match",
            self.declared_sections,
            self.sections.len()
        );
        let total = self.offset;
        let table = header::write_header(&header, &self.sections);
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&table)?;
        self.file.flush()?;
        Ok(total)
    }
}

pub struct SectionStream<'a> {
    writer: &'a mut CacheWriter,
    section_type: SectionType,
    start: u64,
}

/// `Write` so a section can be filled by anything that writes bytes -- the
/// geometry scratch copy among them -- while the writer keeps counting.
impl Write for SectionStream<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<usize> {
        let written = self.writer.file.write(bytes)?;
        self.writer.offset += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.file.flush()
    }
}

impl SectionStream<'_> {

    /// Byte position within the section, which the geometry directory records.
    pub fn position(&self) -> u64 {
        self.writer.offset - self.start
    }

    pub fn finish(self) {
        let size = self.writer.offset - self.start;
        self.writer.sections.push(SectionEntry {
            section_type: self.section_type,
            offset: self.start as u32,
            size: size as u32,
        });
    }
}

pub struct Header {
    pub flags: u16,
    pub source_hash: u64,
    pub schema: u8,
    pub entity_count: u32,
    pub total_vertices: u32,
    pub total_triangles: u32,
}
