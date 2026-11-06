// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Builds a complete `.ifclite` cache from one IFC file.
//!
//! Two passes over the source, which is memory-mapped rather than read, so the
//! 1.7 GiB of a federated model stays file-backed and reclaimable:
//!
//!   1. `extract_data_model` for the entity, property, quantity and
//!      relationship sections. These are built in memory -- they are tables of
//!      scalars, not meshes -- and written before the geometry.
//!   2. `process` for the geometry, with `retain_meshes` off. Each batch is
//!      serialised into the open chunk for its spatial cell and dropped, so the
//!      mesh set never accumulates. That is the whole point: holding it costs
//!      6.2 GiB on a federated model, against a 10 GiB Lambda.
//!
//! The geometry section's chunks go to a scratch file while the directory is
//! collected, then the head and the chunks are appended to the cache. Lambda
//! has 10 GiB of ephemeral storage, which comfortably holds the 2.3 GiB a
//! federated model's cache occupies.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Result, Seek, SeekFrom, Write};
use std::time::Instant;

use ifc_lite_processing::data_model::{extract_data_model_with, DataModel, DataModelEntities};
use ifc_lite_processing::{process, MeshData, MeshFrame, ProcessingRequest, ProcessingSink};

use crate::coordinate_info::{Aabb, CoordinateInfo};
use crate::frame;
use crate::entities::{flags, EntityRow};
use crate::geometry::{append_scratch, GeometryStream};
use crate::header_flags;
use crate::mesh_record::MeshRecord;
use crate::properties::{PropertyTable, PropertyValueType};
use crate::quantities::{quantity_type, QuantityTable};
use crate::relationships::{relationship_type, RelationshipGraph};
use crate::strings::StringTable;
use crate::{CacheWriter, Header, SectionType};

/// What the build reports, matching the fields `buildIfcModelCache` reads out
/// of the TS builder's stdout.
pub struct BuildStats {
    pub entity_count: u64,
    pub mesh_count: u64,
    pub cache_bytes: u64,
    pub parse_ms: u64,
    pub geometry_ms: u64,
    pub total_vertices: u64,
    pub total_triangles: u64,
}

/// Strings, Entities, Properties, Quantities, Relationships, Geometry.
const SECTION_COUNT: usize = 6;

pub fn build_cache(source: &[u8], out: File, scratch: File) -> Result<BuildStats> {
    let parse_started = Instant::now();
    // Rooted entities only -- IfcRoot subtypes, plus the non-rooted instances
    // another table references so no relation dangles. Every STEP line would be
    // 30.5M rows on a federated model where the TS writer emits 225k: the other
    // 135 of every 136 are geometry primitives, which the table is not for.
    //
    // The variant is passed in rather than applied afterwards because building
    // the dropped rows is what exceeds the Lambda's memory, not keeping them.
    let model = extract_data_model_with(source, DataModelEntities::Rooted);
    let total_entities = model.total_entities as u64;
    let parse_ms = parse_started.elapsed().as_millis() as u64;

    let mut strings = StringTable::new();
    let entity_rows = build_entities(&mut strings, &model);
    let (properties, quantities) = build_property_tables(&mut strings, &model);
    let graph = build_relationships(&model);

    let mut writer = CacheWriter::create(out, SECTION_COUNT)?;

    // Every other section indexes into the string table, so it is complete by
    // the time any of them is written -- but it is written first, matching the
    // TS writer's section order.
    writer.section(SectionType::Strings, &strings.write())?;
    writer.section(SectionType::Entities, &super::entities::write_entities(&entity_rows))?;
    writer.section(SectionType::Properties, &properties.write())?;
    writer.section(SectionType::Quantities, &quantities.write())?;
    writer.section(SectionType::Relationships, &graph.write())?;

    let geometry_started = Instant::now();
    let mut sink = CacheSink {
        stream: GeometryStream::new(scratch),
        failure: None,
    };

    let mut request = ProcessingRequest::default();
    // The meshes are serialised as they arrive and dropped. Retaining them is
    // what the native path exists to avoid.
    request.retain_meshes = false;
    // On in the build the viewer's cache comes from, and it changes the mesh
    // count, so the cache matches only with it.
    request.layer_slicing = true;

    let result = process(source, &request, &mut sink);
    let geometry_ms = geometry_started.elapsed().as_millis() as u64;

    if let Some(failure) = sink.failure {
        return Err(failure);
    }

    let stream = sink.stream;
    let mesh_count = stream.mesh_count;
    let total_vertices = stream.total_vertices;
    let total_triangles = stream.total_triangles;
    let coordinate_info = coordinate_info_of(&result, &stream);

    let (head, mut scratch) = stream.finish(&coordinate_info)?;
    let mut section = writer.section_stream(SectionType::Geometry);
    section.write(&head)?;
    scratch.flush()?;
    let mut scratch_reader = scratch;
    append_scratch(&mut scratch_reader, &mut section)?;
    section.finish();

    let cache_bytes = writer.finish(Header {
        flags: header_flags::HAS_GEOMETRY | header_flags::SOURCE_HASH_UNSET,
        // Unset rather than a real xxhash64: hashing 1.7 GiB to prove the
        // source matches costs a full pass the activity already knows the
        // answer to -- it fetched the object by key.
        source_hash: 0,
        schema: schema_code(&result.metadata.schema_version),
        // Every STEP instance parsed, not the table's row count: the TS
        // writer reports the same 30.5M beside its 225k-row table.
        entity_count: total_entities as u32,
        total_vertices: total_vertices as u32,
        total_triangles: total_triangles as u32,
    })?;

    Ok(BuildStats {
        entity_count: total_entities,
        mesh_count,
        cache_bytes,
        parse_ms,
        geometry_ms,
        total_vertices,
        total_triangles,
    })
}

/// Serialises each mesh into its chunk as the pipeline emits it.
struct CacheSink<W: Write> {
    stream: GeometryStream<W>,
    /// The sink cannot return an error, so the first one is held and raised
    /// once `process` returns. A later batch failing the same way adds nothing.
    failure: Option<std::io::Error>,
}

impl<W: Write> ProcessingSink for CacheSink<W> {
    fn on_batch(&mut self, meshes: &[MeshData], _processed: usize, _total: usize) {
        if self.failure.is_some() {
            return;
        }
        for mesh in meshes {
            let record = MeshRecord {
                express_id: mesh.express_id,
                ifc_type: &mesh.ifc_type,
                positions: &mesh.positions,
                normals: &mesh.normals,
                indices: &mesh.indices,
                color: mesh.color,
                geometry_class: mesh.geometry_class,
                geometry_item_id: mesh.geometry_item_id,
                material_id: mesh.material_id,
                origin: mesh.origin,
                local_bounds: mesh.local_bounds,
                local_to_world: mesh.local_to_world,
            };
            if let Err(error) = self.stream.push(&record) {
                self.failure = Some(error);
                return;
            }
        }
    }
}

fn build_entities(strings: &mut StringTable, model: &DataModel) -> Vec<EntityRow> {
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
fn build_property_tables(
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

fn build_relationships(model: &DataModel) -> RelationshipGraph {
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

fn coordinate_info_of<W: Write>(
    result: &ifc_lite_processing::ProcessingResult,
    stream: &GeometryStream<W>,
) -> CoordinateInfo {
    // The translation the pipeline SUBTRACTED from every vertex, in IFC Z-up --
    // which is the frame's own anchor, not `metadata.coordinate_info
    // .origin_shift`. The latter carries the georeferencing easting/northing
    // with a zero vertical, so using it drops the site elevation and the whole
    // model sits at the wrong height while looking otherwise correct.
    //
    // It belongs in `wasmRtcOffset`, the one coordinate-info field kept in IFC
    // Z-up; `originShift` stays zero, which is what the wasm builder writes.
    // The viewer recombines them: originShift + ifcToViewerAxes(wasmRtcOffset).
    let rtc_ifc = match &result.frame {
        MeshFrame::SiteLocal { placement } => [placement[12], placement[13], placement[14]],
        MeshFrame::ModelRtc { anchor } => [anchor.0, anchor.1, anchor.2],
        // Nothing was subtracted: the vertices are already IFC world
        // coordinates, so there is no offset to record.
        MeshFrame::RawIfc => [0.0, 0.0, 0.0],
    };
    let origin_shift = [0.0, 0.0, 0.0];

    // Already in the viewer frame: the stream converts each mesh as it arrives.
    let (min, max) = stream.world_bounds();
    let shifted_bounds = Aabb { min, max };

    // World bounds are the render-frame ones plus the offset that was removed.
    let rtc_viewer = frame::point_f64(rtc_ifc);
    let original_bounds = Aabb {
        min: [
            min[0] + rtc_viewer[0],
            min[1] + rtc_viewer[1],
            min[2] + rtc_viewer[2],
        ],
        max: [
            max[0] + rtc_viewer[0],
            max[1] + rtc_viewer[1],
            max[2] + rtc_viewer[2],
        ],
    };

    CoordinateInfo {
        origin_shift,
        original_bounds,
        shifted_bounds,
        has_large_coordinates: rtc_ifc.iter().any(|axis| axis.abs() > 1.0e5),
        wasm_rtc_offset: Some(rtc_ifc),
        building_rotation: None,
        length_unit_scale: result.metadata.length_unit_scale,
    }
}

/// The schema byte the header carries. Unknown schemas write 0, which the
/// reader treats as unstated rather than as IFC2X3.
fn schema_code(schema_version: &str) -> u8 {
    let upper = schema_version.to_ascii_uppercase();
    if upper.contains("IFC2X3") {
        1
    } else if upper.contains("IFC4X3") {
        3
    } else if upper.contains("IFC4") {
        2
    } else {
        0
    }
}

/// Truncates the cache file to the bytes actually written, for a caller reusing
/// a file that already existed.
pub fn truncate_to(file: &mut File, length: u64) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.set_len(length)
}
