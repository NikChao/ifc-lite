// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The property table, mirroring `sections/properties.js`.
//!
//! Layout: count u32, then the columns --
//!   entityId u32, psetName u32, psetGlobalId u32, propName u32, propType u8,
//!   valueString u32, valueReal f64, valueInt i32, valueBool u8, unitId i32
//! -- each count long, then three indexes in order: entityIndex, psetIndex,
//! propIndex.
//!
//! One row is one property of one property set on one element, so a model with
//! 30M entities produces a lot of rows; the columns are written straight out of
//! the row vector rather than assembled per row.

use crate::buf::Buf;
use crate::index_map::IndexMap;
use crate::strings::StringTable;

/// Matches `PropertyValueType` in @ifc-lite/data. The reader switches on this
/// byte to decide which of the four value columns to read, so a wrong code
/// returns the wrong column's value rather than an error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PropertyValueType {
    String = 0,
    Real = 1,
    Integer = 2,
    Boolean = 3,
    Logical = 4,
    Label = 5,
    Identifier = 6,
    Text = 7,
    Enum = 8,
    Reference = 9,
    List = 10,
}

pub struct PropertyRow {
    pub entity_id: u32,
    pub pset_name: u32,
    pub pset_global_id: u32,
    pub prop_name: u32,
    pub prop_type: PropertyValueType,
    pub value_string: u32,
    pub value_real: f64,
    pub value_int: i32,
    pub value_bool: u8,
    pub unit_id: i32,
}

#[derive(Default)]
pub struct PropertyTable {
    rows: Vec<PropertyRow>,
    entity_index: IndexMap,
    pset_index: IndexMap,
    prop_index: IndexMap,
}

impl PropertyTable {
    pub fn new() -> Self {
        PropertyTable::default()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Adds one property, classifying its canonical string form into the typed
    /// column the reader expects.
    ///
    /// `extract_data_model` hands every value over as text plus a type token, so
    /// the typed columns are recovered here: a value that does not parse as the
    /// type claims falls back to the string column, which reads correctly rather
    /// than yielding a zero the viewer would show as a real measurement.
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        strings: &mut StringTable,
        entity_id: u32,
        pset_name: &str,
        pset_global_id: Option<&str>,
        prop_name: &str,
        prop_type: PropertyValueType,
        value: &str,
    ) {
        let row_index = self.rows.len() as u32;

        let mut value_string = u32::MAX;
        let mut value_real = 0.0f64;
        let mut value_int = 0i32;
        let mut value_bool = 0u8;
        let mut resolved = prop_type;

        match prop_type {
            PropertyValueType::Real => match value.parse::<f64>() {
                Ok(parsed) => value_real = parsed,
                Err(_) => {
                    resolved = PropertyValueType::String;
                    value_string = strings.intern(value);
                }
            },
            PropertyValueType::Integer => match value.parse::<i32>() {
                Ok(parsed) => value_int = parsed,
                Err(_) => {
                    resolved = PropertyValueType::String;
                    value_string = strings.intern(value);
                }
            },
            PropertyValueType::Boolean | PropertyValueType::Logical => {
                match value.to_ascii_lowercase().as_str() {
                    "true" | "t" => value_bool = 1,
                    "false" | "f" => value_bool = 0,
                    // IfcLogical's third state. The reader treats any non-zero
                    // as true, so UNKNOWN is kept as text instead.
                    _ => {
                        resolved = PropertyValueType::String;
                        value_string = strings.intern(value);
                    }
                }
            }
            _ => value_string = strings.intern(value),
        }

        let pset_name_id = strings.intern(pset_name);
        let prop_name_id = strings.intern(prop_name);

        self.entity_index.push(entity_id, row_index);
        self.pset_index.push(pset_name_id, row_index);
        self.prop_index.push(prop_name_id, row_index);

        self.rows.push(PropertyRow {
            entity_id,
            pset_name: pset_name_id,
            pset_global_id: strings.intern(pset_global_id.unwrap_or("")),
            prop_name: prop_name_id,
            prop_type: resolved,
            value_string,
            value_real,
            value_int,
            value_bool,
            // No unit resolution yet: -1 is the table's own absent marker, and a
            // wrong unit id would silently rescale a quantity.
            unit_id: -1,
        });
    }

    pub fn write(&self) -> Vec<u8> {
        let count = self.rows.len();
        let mut buf = Buf::with_capacity(4 + count * 34);

        buf.u32(count as u32);
        for row in &self.rows {
            buf.u32(row.entity_id);
        }
        for row in &self.rows {
            buf.u32(row.pset_name);
        }
        for row in &self.rows {
            buf.u32(row.pset_global_id);
        }
        for row in &self.rows {
            buf.u32(row.prop_name);
        }
        for row in &self.rows {
            buf.u8(row.prop_type as u8);
        }
        for row in &self.rows {
            buf.u32(row.value_string);
        }
        for row in &self.rows {
            buf.f64(row.value_real);
        }
        for row in &self.rows {
            buf.i32(row.value_int);
        }
        for row in &self.rows {
            buf.u8(row.value_bool);
        }
        for row in &self.rows {
            buf.i32(row.unit_id);
        }

        self.entity_index.write(&mut buf);
        self.pset_index.write(&mut buf);
        self.prop_index.write(&mut buf);

        buf.into_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_lands_in_the_real_column() {
        let mut strings = StringTable::new();
        let mut table = PropertyTable::new();
        table.push(
            &mut strings,
            42,
            "Pset_WallCommon",
            None,
            "ThermalTransmittance",
            PropertyValueType::Real,
            "0.35",
        );
        assert_eq!(table.rows[0].value_real, 0.35);
        assert_eq!(table.rows[0].prop_type, PropertyValueType::Real);
    }

    #[test]
    fn an_unparsable_real_falls_back_to_text() {
        let mut strings = StringTable::new();
        let mut table = PropertyTable::new();
        table.push(
            &mut strings,
            42,
            "Pset_WallCommon",
            None,
            "ThermalTransmittance",
            PropertyValueType::Real,
            "not-a-number",
        );
        // Reported as the string it is, rather than as 0.0 -- which the viewer
        // would render as a genuine measurement of zero.
        assert_eq!(table.rows[0].prop_type, PropertyValueType::String);
        assert_ne!(table.rows[0].value_string, u32::MAX);
        assert_eq!(table.rows[0].value_real, 0.0);
    }

    #[test]
    fn booleans_accept_both_spellings_and_keep_unknown_as_text() {
        let mut strings = StringTable::new();
        let mut table = PropertyTable::new();
        for value in ["TRUE", "false", "UNKNOWN"] {
            table.push(
                &mut strings,
                1,
                "Pset",
                None,
                "IsExternal",
                PropertyValueType::Logical,
                value,
            );
        }
        assert_eq!(table.rows[0].value_bool, 1);
        assert_eq!(table.rows[1].value_bool, 0);
        assert_eq!(table.rows[2].prop_type, PropertyValueType::String);
    }

    #[test]
    fn section_length_matches_the_column_layout() {
        let mut strings = StringTable::new();
        let mut table = PropertyTable::new();
        table.push(&mut strings, 1, "Pset", None, "A", PropertyValueType::Label, "x");
        table.push(&mut strings, 1, "Pset", None, "B", PropertyValueType::Label, "y");
        let bytes = table.write();

        let columns = 2 * (4 + 4 + 4 + 4 + 1 + 4 + 8 + 4 + 1 + 4);
        // entityIndex: 1 key holding 2 rows. psetIndex: likewise. propIndex: 2 keys, 1 row each.
        let indexes = (4 + 4 + 4 + 8) + (4 + 4 + 4 + 8) + (4 + (4 + 4 + 4) * 2);
        assert_eq!(bytes.len(), 4 + columns + indexes);
    }
}
