// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Little-endian primitives, matching `BufferWriter` in `@ifc-lite/cache`.
//! Every multi-byte field in the format is little-endian; a big-endian write
//! produces a file the viewer's reader misparses field by field rather than
//! rejecting, so the endianness lives in one place.

pub struct Buf {
    bytes: Vec<u8>,
}

impl Default for Buf {
    fn default() -> Self {
        Self::new()
    }
}

impl Buf {
    pub fn new() -> Self {
        Buf { bytes: Vec::new() }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Buf {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }

    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn f32(&mut self, value: f32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn f64(&mut self, value: f64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    pub fn u32_slice(&mut self, values: &[u32]) {
        for value in values {
            self.u32(*value);
        }
    }

    pub fn zeros(&mut self, count: usize) {
        self.bytes.resize(self.bytes.len() + count, 0);
    }
}
