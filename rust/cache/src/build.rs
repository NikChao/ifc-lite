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

use std::fs::File;
use std::io::{Result, Seek, SeekFrom, Write};
use std::time::Instant;

use ifc_lite_processing::data_model::{
    extract_data_model_tables, DataModelEntities, DataModelTables,
};
use ifc_lite_processing::{process, MeshData, MeshFrame, ProcessingRequest, ProcessingSink};

use crate::coordinate_info::{Aabb, CoordinateInfo};
use crate::frame;
use crate::geometry::{append_scratch, GeometryStream};
use crate::header_flags;
use crate::mesh_record::MeshRecord;
use crate::strings::StringTable;
use crate::tables::{build_entities, build_property_tables, build_relationships};
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
    // Properties and quantities are NOT extracted: `@ifc-lite/cache` writes both
    // sections empty -- 16 bytes each, a count and three empty indexes -- so the
    // viewer never reads them from here. Extracting them anyway costs most of
    // the memory budget on a property-heavy model and produces bytes nothing
    // loads.
    let model = extract_data_model_tables(
        source,
        DataModelEntities::Rooted,
        DataModelTables { properties: false, quantities: false },
    );
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
    // The geometry pass resolves property sets and presentation layers by
    // default, for a viewer that wants them on the first frame. This writer
    // wants neither: the Properties section is built from the data model, and
    // the mesh record carries no layer. On a model with 474k property sets
    // that resolution costs 5.9 GiB of a 10 GiB budget and changes not one of
    // the 596,253 meshes.
    request.features = request.features.properties(false).presentation_layers(false);

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
    section.write_all(&head)?;
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
