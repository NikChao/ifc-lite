// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The quantity table, mirroring `sections/quantities.js`.
//!
//! Layout: count u32, then the columns --
//!   entityId u32, qsetName u32, qsetGlobalId u32, quantityName u32,
//!   quantityType u8, value f64, unitId i32, formula u32
//! -- each count long, then entityIndex, qsetIndex and quantityIndex.
//!
//! The same shape as the property table with one value column instead of four,
//! because every quantity is a number.

use crate::buf::Buf;
use crate::index_map::IndexMap;
use crate::strings::StringTable;

/// Matches `QuantityType` in @ifc-lite/data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum QuantityType {
    Length = 0,
    Area = 1,
    Volume = 2,
    Count = 3,
    Weight = 4,
    Time = 5,
    Number = 6,
}

/// Resolves the `IfcQuantity*` class name the data model reports. An unmapped
/// class becomes `Number`, which is the neutral reading: the value is still
/// shown, just without a dimension claim.
pub fn quantity_type(class_name: &str) -> QuantityType {
    match class_name.to_ascii_lowercase().as_str() {
        "ifcquantitylength" => QuantityType::Length,
        "ifcquantityarea" => QuantityType::Area,
        "ifcquantityvolume" => QuantityType::Volume,
        "ifcquantitycount" => QuantityType::Count,
        "ifcquantityweight" => QuantityType::Weight,
        "ifcquantitytime" => QuantityType::Time,
        _ => QuantityType::Number,
    }
}

struct QuantityRow {
    entity_id: u32,
    qset_name: u32,
    qset_global_id: u32,
    quantity_name: u32,
    quantity_type: QuantityType,
    value: f64,
    unit_id: i32,
    formula: u32,
}

#[derive(Default)]
pub struct QuantityTable {
    rows: Vec<QuantityRow>,
    entity_index: IndexMap,
    qset_index: IndexMap,
    quantity_index: IndexMap,
}

impl QuantityTable {
    pub fn new() -> Self {
        QuantityTable::default()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Adds one quantity. A value that does not parse is skipped rather than
    /// stored as zero: there is no string column to fall back to here, and a
    /// zero area reads as a measured zero.
    pub fn push(
        &mut self,
        strings: &mut StringTable,
        entity_id: u32,
        qset_name: &str,
        qset_global_id: Option<&str>,
        quantity_name: &str,
        quantity_type: QuantityType,
        value: &str,
        formula: Option<&str>,
    ) -> bool {
        let Ok(parsed) = value.parse::<f64>() else {
            return false;
        };
        self.push_value(
            strings,
            entity_id,
            qset_name,
            qset_global_id,
            quantity_name,
            quantity_type,
            parsed,
            formula,
        );
        true
    }

    /// The same, for a value the extractor already reports as a number -- which
    /// avoids a decimal round trip that can lose the last bit of an f64.
    #[allow(clippy::too_many_arguments)]
    pub fn push_value(
        &mut self,
        strings: &mut StringTable,
        entity_id: u32,
        qset_name: &str,
        qset_global_id: Option<&str>,
        quantity_name: &str,
        quantity_type: QuantityType,
        parsed: f64,
        formula: Option<&str>,
    ) {
        let row_index = self.rows.len() as u32;
        let qset_name_id = strings.intern(qset_name);
        let quantity_name_id = strings.intern(quantity_name);

        self.entity_index.push(entity_id, row_index);
        self.qset_index.push(qset_name_id, row_index);
        self.quantity_index.push(quantity_name_id, row_index);

        self.rows.push(QuantityRow {
            entity_id,
            qset_name: qset_name_id,
            qset_global_id: strings.intern(qset_global_id.unwrap_or("")),
            quantity_name: quantity_name_id,
            quantity_type,
            value: parsed,
            // No unit resolution yet; -1 is the table's absent marker.
            unit_id: -1,
            formula: strings.intern(formula.unwrap_or("")),
        });
    }

    pub fn write(&self) -> Vec<u8> {
        let count = self.rows.len();
        let mut buf = Buf::with_capacity(4 + count * 29);

        buf.u32(count as u32);
        for row in &self.rows {
            buf.u32(row.entity_id);
        }
        for row in &self.rows {
            buf.u32(row.qset_name);
        }
        for row in &self.rows {
            buf.u32(row.qset_global_id);
        }
        for row in &self.rows {
            buf.u32(row.quantity_name);
        }
        for row in &self.rows {
            buf.u8(row.quantity_type as u8);
        }
        for row in &self.rows {
            buf.f64(row.value);
        }
        for row in &self.rows {
            buf.i32(row.unit_id);
        }
        for row in &self.rows {
            buf.u32(row.formula);
        }

        self.entity_index.write(&mut buf);
        self.qset_index.write(&mut buf);
        self.quantity_index.write(&mut buf);

        buf.into_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_quantity_classes_read_as_a_plain_number() {
        assert_eq!(quantity_type("IfcQuantityArea"), QuantityType::Area);
        assert_eq!(quantity_type("IFCQUANTITYVOLUME"), QuantityType::Volume);
        assert_eq!(quantity_type("IfcQuantitySomethingNew"), QuantityType::Number);
    }

    #[test]
    fn an_unparsable_value_is_skipped_rather_than_stored_as_zero() {
        let mut strings = StringTable::new();
        let mut table = QuantityTable::new();
        let kept = table.push(
            &mut strings,
            1,
            "Qto_WallBaseQuantities",
            None,
            "NetSideArea",
            QuantityType::Area,
            "not-a-number",
            None,
        );
        assert!(!kept);
        assert!(table.is_empty());
    }

    #[test]
    fn section_length_matches_the_column_layout() {
        let mut strings = StringTable::new();
        let mut table = QuantityTable::new();
        table.push(&mut strings, 1, "Qto", None, "A", QuantityType::Area, "1.5", None);
        table.push(&mut strings, 1, "Qto", None, "B", QuantityType::Length, "2", None);
        let bytes = table.write();

        let columns = 2 * (4 + 4 + 4 + 4 + 1 + 8 + 4 + 4);
        // entityIndex and qsetIndex: one key, two rows. quantityIndex: two keys, one row each.
        let indexes = (4 + 4 + 4 + 8) * 2 + (4 + (4 + 4 + 4) * 2);
        assert_eq!(bytes.len(), 4 + columns + indexes);
    }
}
