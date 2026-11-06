// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The entity table, mirroring `sections/entities.js`.
//!
//! Layout: count u32, then one column at a time --
//!   expressId u32[count], typeEnum u16[count], globalId u32[count],
//!   name u32[count], description u32[count], objectType u32[count],
//!   flags u8[count], containedInStorey i32[count], definedByType i32[count],
//!   geometryIndex i32[count],
//!   typeRangeCount u16, then [type u16, start u32, end u32] triples,
//!   rawTypeName u32[count]  (v15+, appended AFTER the triples)
//!
//! The string columns hold indices into the Strings section. The triples are
//! vestigial -- `readEntities` consumes them to stay aligned and then derives
//! the spans from the typeEnum column, because two incompatible vintages of the
//! field exist under the same format version -- but they must still be written
//! or every byte after them lands at the wrong offset.

use crate::buf::Buf;
use crate::strings::StringTable;
use crate::type_enum::type_ordinal;

/// Flags byte, matching `EntityFlags` in @ifc-lite/data.
pub mod flags {
    pub const HAS_GEOMETRY: u8 = 1;
    pub const HAS_PROPERTIES: u8 = 2;
    pub const HAS_QUANTITIES: u8 = 4;
    pub const IS_TYPE: u8 = 8;
}

/// One row, with strings already interned.
pub struct EntityRow {
    pub express_id: u32,
    pub type_enum: u16,
    pub global_id: u32,
    pub name: u32,
    pub description: u32,
    pub object_type: u32,
    pub flags: u8,
    pub raw_type_name: u32,
}

impl EntityRow {
    /// Builds a row from the fields `extract_data_model` provides, interning the
    /// strings as it goes.
    pub fn from_metadata(
        strings: &mut StringTable,
        express_id: u32,
        type_name: &str,
        global_id: Option<&str>,
        name: Option<&str>,
        description: Option<&str>,
        object_type: Option<&str>,
        flags: u8,
    ) -> Self {
        EntityRow {
            express_id,
            type_enum: type_ordinal(type_name),
            // An absent attribute interns as the empty string rather than a
            // sentinel: the reader indexes the Strings section with these
            // directly, so every value has to be a valid index.
            global_id: strings.intern(global_id.unwrap_or("")),
            name: strings.intern(name.unwrap_or("")),
            description: strings.intern(description.unwrap_or("")),
            object_type: strings.intern(object_type.unwrap_or("")),
            flags,
            raw_type_name: strings.intern(type_name),
        }
    }
}

pub fn write_entities(rows: &[EntityRow]) -> Vec<u8> {
    let count = rows.len();
    // 4 + count * (4+2+4+4+4+4+1+4+4+4+4) = 4 + count * 39, plus the triples.
    let mut buf = Buf::with_capacity(4 + count * 39 + 2);

    buf.u32(count as u32);

    for row in rows {
        buf.u32(row.express_id);
    }
    for row in rows {
        buf.u16(row.type_enum);
    }
    for row in rows {
        buf.u32(row.global_id);
    }
    for row in rows {
        buf.u32(row.name);
    }
    for row in rows {
        buf.u32(row.description);
    }
    for row in rows {
        buf.u32(row.object_type);
    }
    for row in rows {
        buf.u8(row.flags);
    }
    // containedInStorey, definedByType and geometryIndex are all written absent.
    // The viewer rebuilds the spatial hierarchy from the Relationships section
    // on load regardless -- the cache writer's own note says so -- and -1 is the
    // value `EntityTable` initialises these columns to, so an absent column
    // reads as "not recorded" rather than as row 0.
    for _ in 0..3 {
        for _ in rows {
            buf.i32(-1);
        }
    }

    write_type_ranges(&mut buf, rows);

    for row in rows {
        buf.u32(row.raw_type_name);
    }

    buf.into_vec()
}

/// The vestigial span triples. Rows are not sorted by type, so a contiguous span
/// per type does not exist in general; the reader ignores these values and
/// derives spans from the typeEnum column, so what matters is that the right
/// number of bytes is present. One triple per distinct type keeps the section
/// the shape a reader expects.
fn write_type_ranges(buf: &mut Buf, rows: &[EntityRow]) {
    let mut seen: Vec<(u16, u32, u32)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let at = index as u32;
        match seen.iter_mut().find(|(type_enum, _, _)| *type_enum == row.type_enum) {
            Some((_, _, end)) => *end = at + 1,
            None => seen.push((row.type_enum, at, at + 1)),
        }
    }

    buf.u16(seen.len() as u16);
    for (type_enum, start, end) in seen {
        buf.u16(type_enum);
        buf.u32(start);
        buf.u32(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(strings: &mut StringTable, id: u32, type_name: &str) -> EntityRow {
        EntityRow::from_metadata(
            strings,
            id,
            type_name,
            Some("0abcdefghijklmnopqrstu"),
            Some("Wall-1"),
            None,
            None,
            flags::HAS_GEOMETRY,
        )
    }

    #[test]
    fn absent_attributes_intern_as_the_empty_string() {
        let mut strings = StringTable::new();
        let built = row(&mut strings, 1, "IfcWall");
        // description and objectType were None and must still be valid indices.
        assert_eq!(built.description, built.object_type);
        assert_ne!(built.name, built.description);
    }

    #[test]
    fn section_length_matches_the_column_layout() {
        let mut strings = StringTable::new();
        let rows = vec![row(&mut strings, 1, "IfcWall"), row(&mut strings, 2, "IfcSlab")];
        let bytes = write_entities(&rows);

        let count = 2usize;
        // 4 (count) + per-row columns + triples (2 + 2 distinct * 10) + rawTypeName
        let expected = 4 + count * (4 + 2 + 4 + 4 + 4 + 4 + 1 + 4 + 4 + 4) + (2 + 2 * 10) + count * 4;
        assert_eq!(bytes.len(), expected);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 2);
    }

    #[test]
    fn type_ranges_cover_every_distinct_type_once() {
        let mut strings = StringTable::new();
        let rows = vec![
            row(&mut strings, 1, "IfcWall"),
            row(&mut strings, 2, "IfcWall"),
            row(&mut strings, 3, "IfcSlab"),
        ];
        let mut buf = Buf::new();
        write_type_ranges(&mut buf, &rows);
        let bytes = buf.into_vec();
        assert_eq!(u16::from_le_bytes(bytes[0..2].try_into().unwrap()), 2);
        assert_eq!(bytes.len(), 2 + 2 * 10);
    }
}
