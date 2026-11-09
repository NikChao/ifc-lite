// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Header and section table, mirroring `sections/header.js`.
//!
//! Layout: magic u32, version u16, flags u16, sourceHash u64, schema u8,
//! entityCount u32, totalVertices u32, totalTriangles u32, sectionCount u16 --
//! 31 bytes -- zero-padded to 64, then one 16-byte entry per section of
//! type u16, flags u16, offset u32, size u32, compressedSize u32.
//!
//! The 31 is load-bearing: the reader skips `HEADER_SIZE - 31` and then reads
//! the table, so a field added without adjusting the padding shifts the whole
//! table.

use crate::buf::Buf;
use crate::{Header, SectionEntry, FORMAT_VERSION, HEADER_SIZE, MAGIC, SECTION_ENTRY_SIZE};

const WRITTEN_BEFORE_PADDING: usize = 31;

pub fn write_header(header: &Header, sections: &[SectionEntry]) -> Vec<u8> {
    let mut buf = Buf::with_capacity(HEADER_SIZE + sections.len() * SECTION_ENTRY_SIZE);

    buf.u32(MAGIC);
    buf.u16(FORMAT_VERSION);
    buf.u16(header.flags);
    buf.u64(header.source_hash);
    buf.u8(header.schema);
    buf.u32(header.entity_count);
    buf.u32(header.total_vertices);
    buf.u32(header.total_triangles);
    buf.u16(sections.len() as u16);
    debug_assert_eq!(buf.len(), WRITTEN_BEFORE_PADDING);
    buf.zeros(HEADER_SIZE - WRITTEN_BEFORE_PADDING);

    for section in sections {
        buf.u16(section.section_type as u16);
        // SectionFlags::None. Per-section compression is expressed inside the
        // geometry section's own chunk records, never here.
        buf.u16(0);
        buf.u32(section.offset);
        buf.u32(section.size);
        // compressedSize, which the writer leaves at 0 for an uncompressed section.
        buf.u32(0);
    }

    buf.into_vec()
}
