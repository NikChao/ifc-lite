// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The scalar sections, built in memory from the extracted data model.

use std::collections::HashMap;

use ifc_lite_processing::data_model::DataModel;

use crate::entities::{flags, EntityRow};
use crate::properties::{PropertyTable, PropertyValueType};
use crate::quantities::{quantity_type, QuantityTable};
use crate::relationships::{relationship_type, RelationshipGraph};
use crate::strings::StringTable;

pub fn build_entities(strings: &mut StringTable, model: &DataModel) -> Vec<EntityRow> {
    // Which entities carry property or quantity sets, for the flags byte.
    let mut has_properties: HashMap<u32, u8> = HashMap::new();
    for relationship in &model.relationships {
        let bit = match relationship.rel_type.to_ascii_lowercase().as_str() {
            "ifcreldefinesbyproperties" => flags::HAS_PROPERTIES,
            _ => continue,
        };
        *has_properties.entry(relationship.related_id).or_insert(0) |= bit;
        *has_properties.entry(relationship.relating_id).or_insert(0) |= bit;
    }

    model
        .entities
        .iter()
        .map(|entity| {
            let mut bits = has_properties
                .get(&entity.entity_id)
                .copied()
                .unwrap_or(0);
            if entity.has_geometry {
                bits |= flags::HAS_GEOMETRY;
            }
            if entity.type_name.to_ascii_lowercase().ends_with("type") {
                bits |= flags::IS_TYPE;
            }
            EntityRow::from_metadata(
                strings,
                entity.entity_id,
                &entity.type_name,
                entity.global_id.as_deref(),
                entity.name.as_deref(),
                entity.description.as_deref(),
                entity.object_type.as_deref(),
                bits,
            )
        })
        .collect()
}

/// Property and quantity sets reach their elements through
/// `IfcRelDefinesByProperties`, which the data model reports as a plain edge.
/// Which end is the set is not fixed by the extractor, so the side that appears
/// in the set tables is taken as the set and the other as the element -- which
/// is right whichever way round the edge was recorded.
pub fn build_property_tables(
    strings: &mut StringTable,
    model: &DataModel,
) -> (PropertyTable, QuantityTable) {
    let psets: HashMap<u32, _> = model
        .property_sets
        .iter()
        .map(|set| (set.pset_id, set))
        .collect();
    let qsets: HashMap<u32, _> = model
        .quantity_sets
        .iter()
        .map(|set| (set.qset_id, set))
        .collect();

    let mut properties = PropertyTable::new();
    let mut quantities = QuantityTable::new();

    for relationship in &model.relationships {
        if !relationship
            .rel_type
            .eq_ignore_ascii_case("IfcRelDefinesByProperties")
        {
            continue;
        }
        let (set_id, entity_id) = (relationship.relating_id, relationship.related_id);
        let (set_id, entity_id) = if psets.contains_key(&set_id) || qsets.contains_key(&set_id) {
            (set_id, entity_id)
        } else {
            (entity_id, set_id)
        };

        if let Some(set) = psets.get(&set_id) {
            for property in &set.properties {
                properties.push(
                    strings,
                    entity_id,
                    &set.pset_name,
                    None,
                    &property.property_name,
                    value_type_of(&property.property_type),
                    &property.property_value,
                );
            }
        }
        if let Some(set) = qsets.get(&set_id) {
            for quantity in &set.quantities {
                quantities.push_value(
                    strings,
                    entity_id,
                    &set.qset_name,
                    None,
                    &quantity.quantity_name,
                    quantity_type(&quantity.quantity_type),
                    quantity.quantity_value,
                    set.method_of_measurement.as_deref(),
                );
            }
        }
    }

    (properties, quantities)
}

/// The IFC value type token the extractor reports, mapped to the column the
/// reader will look in. An unrecognised token reads as text, which shows the
/// value as authored rather than coercing it.
fn value_type_of(property_type: &str) -> PropertyValueType {
    match property_type.to_ascii_lowercase().as_str() {
        "ifcreal" | "ifclengthmeasure" | "ifcareameasure" | "ifcvolumemeasure"
        | "ifcpositivelengthmeasure" | "ifcratiomeasure" | "ifcthermaltransmittancemeasure" => {
            PropertyValueType::Real
        }
        "ifcinteger" | "ifccountmeasure" => PropertyValueType::Integer,
        "ifcboolean" => PropertyValueType::Boolean,
        "ifclogical" => PropertyValueType::Logical,
        "ifclabel" => PropertyValueType::Label,
        "ifcidentifier" => PropertyValueType::Identifier,
        "ifctext" => PropertyValueType::Text,
        _ => PropertyValueType::String,
    }
}

pub fn build_relationships(model: &DataModel) -> RelationshipGraph {
    let mut graph = RelationshipGraph::new();
    for relationship in &model.relationships {
        // An unmapped class is left out rather than guessed: a wrong ordinal
        // reads back as a different kind of edge entirely.
        if let Some(rel_type) = relationship_type(&relationship.rel_type) {
            graph.push(
                relationship.rel_id,
                rel_type,
                relationship.relating_id,
                relationship.related_id,
            );
        }
    }
    graph
}
