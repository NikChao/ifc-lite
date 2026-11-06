// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The interned string table, mirroring `sections/strings.js`.
//!
//! Layout: count u32, offsets u32[count+1] (cumulative, starting at 0), then the
//! UTF-8 bytes back to back.
//!
//! Every other section refers to a string by its index here, so the table is
//! built first and written once. The reader rejects a table whose first offset
//! is not 0 or whose offsets are not monotonic, and it preserves duplicate
//! slots positionally -- so entries are never deduplicated after the fact, only
//! on the way in.

use std::collections::HashMap;

use crate::buf::Buf;

#[derive(Default)]
pub struct StringTable {
    strings: Vec<String>,
    index: HashMap<String, u32>,
}

impl StringTable {
    /// Index 0 is always the empty string.
    ///
    /// `StringTable.fromArray` on the read side GUARANTEES that slot, so a
    /// table whose first entry is anything else comes back with an empty string
    /// prepended and EVERY index shifted by one: `getGlobalId` then answers
    /// with its neighbour. That reads as plausible data -- a GlobalId that is
    /// really a type name -- rather than as corruption. The wasm writer's table
    /// carries the same reservation.
    pub fn new() -> Self {
        let mut table = StringTable::default();
        table.intern("");
        table
    }

    /// Index of `value`, adding it if this is the first time it is seen.
    pub fn intern(&mut self, value: &str) -> u32 {
        if let Some(existing) = self.index.get(value) {
            return *existing;
        }
        let id = self.strings.len() as u32;
        self.strings.push(value.to_owned());
        self.index.insert(value.to_owned(), id);
        id
    }

    /// Index of `value`, or the sentinel the sections use for an absent string.
    pub fn intern_opt(&mut self, value: Option<&str>) -> u32 {
        match value {
            Some(value) => self.intern(value),
            None => u32::MAX,
        }
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    pub fn write(&self) -> Vec<u8> {
        let encoded: Vec<&[u8]> = self.strings.iter().map(|s| s.as_bytes()).collect();
        let total: usize = encoded.iter().map(|b| b.len()).sum();

        let mut buf = Buf::with_capacity(4 + (encoded.len() + 1) * 4 + total);
        buf.u32(encoded.len() as u32);

        let mut running = 0u32;
        for bytes in &encoded {
            buf.u32(running);
            running += bytes.len() as u32;
        }
        buf.u32(running);

        for bytes in &encoded {
            buf.bytes(bytes);
        }

        buf.into_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_string_holds_index_zero() {
        let mut table = StringTable::new();
        assert_eq!(table.len(), 1);
        assert_eq!(table.intern(""), 0);
        assert_eq!(table.intern("IfcWall"), 1);
    }

    #[test]
    fn interning_is_positional_and_deduplicated() {
        let mut table = StringTable::new();
        assert_eq!(table.intern("IfcWall"), 1);
        assert_eq!(table.intern("IfcSlab"), 2);
        assert_eq!(table.intern("IfcWall"), 1);
        assert_eq!(table.len(), 3);
    }

    #[test]
    fn an_empty_table_still_carries_its_reserved_slot() {
        let bytes = StringTable::new().write();
        // count 1, offsets [0, 0], no data.
        assert_eq!(bytes, vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn offsets_are_cumulative_and_anchored_at_zero() {
        let mut table = StringTable::new();
        table.intern("ab");
        table.intern("cde");
        let bytes = table.write();

        let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(count, 3, "the reserved empty string plus two");
        let offsets: Vec<u32> = (0..4)
            .map(|i| {
                let at = 4 + i * 4;
                u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
            })
            .collect();
        assert_eq!(offsets, vec![0, 0, 2, 5]);
        assert_eq!(&bytes[20..], b"abcde");
    }
}
