// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The lookup indexes the Properties and Quantities sections carry.
//!
//! Layout: entryCount u32, then per entry key u32, count u32, indices u32[count].
//!
//! Each one maps a key (an entity id, or a string index for a set or property
//! name) to the rows that mention it, so the viewer can answer "every property
//! of this element" without scanning the columns. The rows are built as the
//! columns are, which means one pass rather than a second walk at write time.

use std::collections::HashMap;

use crate::buf::Buf;

/// Insertion-ordered so the section is byte-stable across runs: a `HashMap`
/// alone would emit entries in an arbitrary order, which makes two caches of
/// the same model differ and defeats byte-comparison against the TS writer.
#[derive(Default)]
pub struct IndexMap {
    order: Vec<u32>,
    rows: HashMap<u32, Vec<u32>>,
}

impl IndexMap {
    pub fn new() -> Self {
        IndexMap::default()
    }

    pub fn push(&mut self, key: u32, row: u32) {
        match self.rows.get_mut(&key) {
            Some(existing) => existing.push(row),
            None => {
                self.order.push(key);
                self.rows.insert(key, vec![row]);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn write(&self, buf: &mut Buf) {
        buf.u32(self.order.len() as u32);
        for key in &self.order {
            let rows = &self.rows[key];
            buf.u32(*key);
            buf.u32(rows.len() as u32);
            buf.u32_slice(rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_rows_under_their_key() {
        let mut index = IndexMap::new();
        index.push(7, 0);
        index.push(9, 1);
        index.push(7, 2);

        let mut buf = Buf::new();
        index.write(&mut buf);
        let bytes = buf.into_vec();

        let read = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(read(0), 2, "two distinct keys");
        assert_eq!(read(4), 7);
        assert_eq!(read(8), 2, "key 7 holds rows 0 and 2");
        assert_eq!(read(12), 0);
        assert_eq!(read(16), 2);
        assert_eq!(read(20), 9);
        assert_eq!(read(24), 1);
    }

    #[test]
    fn empty_index_writes_only_its_count() {
        let mut buf = Buf::new();
        IndexMap::new().write(&mut buf);
        assert_eq!(buf.into_vec(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn key_order_follows_first_insertion() {
        let mut index = IndexMap::new();
        index.push(50, 0);
        index.push(10, 1);
        index.push(50, 2);
        assert_eq!(index.order, vec![50, 10]);
    }
}
