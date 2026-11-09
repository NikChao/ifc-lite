// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Data model extraction service - extracts properties, relationships, and spatial hierarchy.

mod classifications;
mod documents;
mod jobs;
mod materials;
mod material_units;
mod material_usages;
mod metadata;
pub mod property;
pub mod quantity;
pub mod render;
pub mod rows;
pub mod spatial_walk;
pub mod type_assignments;
mod inherit;
mod properties;
mod property_value;
mod quantities;
mod relationships;
mod rooted;
mod slots;
mod spatial;
mod types;

pub use rooted::DataModelEntities;
pub use types::*;

use classifications::extract_classifications;
use documents::extract_documents;
use ifc_lite_core::{
    build_entity_index, DecodedEntity, EntityDecoder, EntityIndex, EntityScanner,
};
use jobs::JobBuckets;
use materials::extract_materials;
use material_units::MaterialUnitContext;
use metadata::extract_entity_metadata;
use properties::extract_properties;
use quantities::extract_quantities;
use relationships::extract_relationships;
use spatial::build_spatial_hierarchy;
use std::sync::Arc;

/// Extract complete data model from IFC content.
pub fn extract_data_model<T>(content: &T) -> DataModel
where
    T: AsRef<[u8]> + ?Sized,
{
    extract_data_model_with(content, DataModelEntities::All)
}

/// Extract the data model, building entity metadata only for the rows
/// `entities` asks for.
///
/// [`DataModelEntities::Rooted`] narrows the table exactly as
/// [`DataModelEntities::apply`] does, but it does so BEFORE the metadata is
/// built rather than after. Over `various/O-S1-BWK-BIM architectural` that is
/// 211,716 rows built instead of 4,411,807 -- six owned `String`s each -- and
/// 1665 MiB of peak RSS instead of 6180. A caller with a memory ceiling (a
/// browser tab, a function with a fixed budget) cannot afford the peak even
/// though it can afford the result.
///
/// The output matches `extract_data_model` followed by `apply` row for row,
/// which `rooted_tests.rs` asserts directly. That holds only while every other
/// table is built first; see [`rooted::metadata_jobs`] for why.
pub fn extract_data_model_with<T>(content: &T, entities_variant: DataModelEntities) -> DataModel
where
    T: AsRef<[u8]> + ?Sized,
{
    extract_data_model_tables(content, entities_variant, DataModelTables::default())
}

/// Which optional tables to build.
///
/// Property and quantity extraction reads every `IfcPropertySet` and
/// `IfcElementQuantity` in the file and owns a `String` per value. A consumer
/// that does not serve properties from this data model -- a cache writer whose
/// format carries them elsewhere -- pays that for nothing: dropping both takes
/// `various/O-S1-BWK-BIM architectural` from 1665 MiB of peak RSS to 999, and
/// `ara3d/ISSUE_053_20181220Holter_Tower_10.ifc` from 870 to 707.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataModelTables {
    pub properties: bool,
    pub quantities: bool,
}

impl Default for DataModelTables {
    fn default() -> Self {
        Self { properties: true, quantities: true }
    }
}

/// Extract the data model, building only the tables `tables` asks for.
pub fn extract_data_model_tables<T>(
    content: &T,
    entities_variant: DataModelEntities,
    tables: DataModelTables,
) -> DataModel
where
    T: AsRef<[u8]> + ?Sized,
{
    let content = content.as_ref();
    let extract_start = std::time::Instant::now();
    tracing::info!(
        content_size = content.len(),
        "Starting data model extraction"
    );

    // Build entity index (shared across all extractors)
    let entity_index = Arc::new(build_entity_index(content));

    let all_entities = scan_entity_jobs(content);
    let total_entities = all_entities.len();

    // Parallel extraction using rayon::join. Every extractor borrows the
    // content: `rayon::join` scopes the borrow, so the copy of the whole
    // upload this used to make (up to `MAX_FILE_SIZE_MB`, on top of the parse
    // running beside it) bought nothing (#7676, SERVER-11).
    let buckets = JobBuckets::new(&all_entities);
    let ((property_sets, quantity_sets), relationships) = rayon::join(
        || {
            rayon::join(
                || {
                    if tables.properties {
                        extract_properties(&buckets.property_sets, content, &entity_index)
                    } else {
                        Vec::new()
                    }
                },
                || {
                    if tables.quantities {
                        extract_quantities(&buckets.quantity_sets, content, &entity_index)
                    } else {
                        Vec::new()
                    }
                },
            )
        },
        || extract_relationships(&all_entities, content, &entity_index),
    );

    // Every other table runs first, because together they name the non-rooted
    // entities a rooted table still has to carry, so no relation dangles.
    //
    // The association pass is one of those tables and is easy to miss: a
    // `materials` row points at an `IfcMaterial` by `definition_id` and
    // `material_id`, and those rows carry no GlobalId and are referenced by
    // nothing else. Filtering before this ran dropped them, so the entities
    // table no longer described ids the materials table points at -- the exact
    // dangle `retain_rooted_entities` keeps them to prevent. It needs only the
    // relationships and the buckets, so it belongs here.
    let Associations {
        length_unit_scale,
        classifications,
        materials,
        documents,
        refused,
    } = extract_associations(content, &entity_index, &buckets, &relationships);

    // Only now is it known which jobs need metadata, which is what lets the
    // Rooted variant skip building the rest.
    let metadata_jobs = rooted::metadata_jobs(
        entities_variant,
        &all_entities,
        &property_sets,
        &quantity_sets,
        &relationships,
        rooted::AssociationRefs {
            classifications: &classifications,
            materials: &materials,
            documents: &documents,
        },
    );
    let entities = extract_entity_metadata(&metadata_jobs, content, &entity_index);

    // Build spatial hierarchy (depends on relationships and entities)
    let spatial_hierarchy = build_spatial_hierarchy(
        &relationships,
        &entities,
        content,
        &entity_index,
        length_unit_scale,
    );

    let extract_time = extract_start.elapsed();
    tracing::info!(
        entities = entities.len(),
        property_sets = property_sets.len(),
        quantity_sets = quantity_sets.len(),
        relationships = relationships.len(),
        classifications = classifications.len(),
        materials = materials.len(),
        documents = documents.len(),
        spatial_nodes = spatial_hierarchy.nodes.len(),
        extract_time_ms = extract_time.as_millis(),
        "Data model extraction complete"
    );

    let mut model = DataModel {
        entities,
        total_entities,
        property_sets,
        quantity_sets,
        relationships,
        classifications,
        materials,
        documents,
        spatial_hierarchy,
        refused,
    };

    // `metadata_jobs` filters on the type name alone, which is all it has
    // before the rows exist, so it deliberately over-keeps: any type the IFC4
    // registry does not list is built in case it carries a GlobalId. The exact
    // rule needs the extracted row, so it runs here -- on the small table
    // rather than on all 30.5M. Without this the two paths disagree: a
    // federated model keeps 788k rows where `apply` keeps 225k.
    entities_variant.apply(&mut model);
    model
}

/// Every STEP instance in the file, in scan order.
fn scan_entity_jobs(content: &[u8]) -> Vec<EntityJob<'_>> {
    let mut scanner = EntityScanner::new(content);
    let mut all_entities: Vec<EntityJob> = Vec::new();

    let mut last_id = 0u32;
    let mut last_type = "";
    let mut max_id = 0u32;
    let mut last_end = 0usize;
    let content_len = content.len();

    while let Some((id, type_name, start, end)) = scanner.next_entity() {
        last_id = id;
        last_type = type_name;
        last_end = end;
        if id > max_id {
            max_id = id;
        }
        all_entities.push(EntityJob {
            id,
            type_name,
            start,
            end,
        });
    }

    let remaining_bytes = content_len.saturating_sub(last_end);
    tracing::debug!(
        total_entities = all_entities.len(),
        last_id = last_id,
        max_id = max_id,
        last_type = %last_type,
        last_end = last_end,
        content_len = content_len,
        remaining_bytes = remaining_bytes,
        "Scanned all entities"
    );

    log_scan_census(&all_entities);
    all_entities
}

/// The `IfcRelAssociates*` tables, with the length-unit scale they resolve against.
struct Associations {
    length_unit_scale: f64,
    classifications: Vec<ClassificationAssociation>,
    materials: Vec<MaterialAssociation>,
    documents: Vec<DocumentAssociation>,
    refused: Option<String>,
}

/// Classifications, materials and documents, which follow the same
/// `IfcRelAssociates*` pattern as the relationship pass but resolve the
/// referenced object into a flat, element-keyed shape (issue #900 parity).
fn extract_associations(
    content: &[u8],
    entity_index: &Arc<EntityIndex>,
    buckets: &JobBuckets,
    relationships: &[Relationship],
) -> Associations {
    // Data path: main's value, never a refusal (#7678); the decoder is lossless.
    let mut unit_decoder = EntityDecoder::with_arc_index(content, entity_index.clone());
    let project_ids = ifc_lite_core::find_project_ids(content);
    let length_unit_scale = unit_decoder.length_unit_scale();
    tracing::debug!(
        length_unit_scale = length_unit_scale,
        "Extracted length unit scale"
    );
    // Materials report layer thickness in metres, so they need the scale.
    let material_units = MaterialUnitContext::new(
        &project_ids, relationships, &mut unit_decoder, length_unit_scale,
    );

    let ((classifications, materials), documents) = rayon::join(
        || {
            rayon::join(
                || extract_classifications(&buckets.rel_classifications, content, entity_index),
                || {
                    extract_materials(
                        &buckets.rel_materials,
                        content,
                        entity_index,
                        &material_units,
                    )
                },
            )
        },
        || extract_documents(&buckets.rel_documents, content, entity_index),
    );

    // A table over its budget is not served truncated: the refusal travels
    // with the model to the one serializer every route uses (#7675).
    let (materials, refused) = match materials {
        Ok(rows) => (rows, None),
        Err(reason) => (Vec::new(), Some(reason)),
    };

    Associations {
        length_unit_scale,
        classifications,
        materials,
        documents,
        refused,
    }
}

/// Debug-level census of the scanned jobs, to diagnose a model that extracts
/// nothing. Costs nothing unless debug tracing is enabled.
fn log_scan_census(all_entities: &[EntityJob]) {
    if tracing::enabled!(tracing::Level::DEBUG) {
        let sample_types: Vec<&str> = all_entities
            .iter()
            .take(20)
            .map(|j| j.type_name)
            .collect();
        tracing::debug!(?sample_types, "Sample entity types from scan");

        // Check if any type contains "PROPERTY" or "REL" (case-insensitive)
        let has_property_like = all_entities
            .iter()
            .any(|j| j.type_name.to_uppercase().contains("PROPERTY"));
        let has_rel_like = all_entities
            .iter()
            .any(|j| j.type_name.to_uppercase().starts_with("IFCREL"));
        tracing::debug!(
            has_property_like = has_property_like,
            has_rel_like = has_rel_like,
            "Entity type pattern check"
        );

        // Debug: count property sets and relationships in scanned entities
        let pset_count = all_entities
            .iter()
            .filter(|j| j.type_name.to_uppercase() == "IFCPROPERTYSET")
            .count();
        let rel_count = all_entities
            .iter()
            .filter(|j| {
                let t = j.type_name.to_uppercase();
                t == "IFCRELDEFINESBYPROPERTIES"
                    || t == "IFCRELAGGREGATES"
                    || t == "IFCRELCONTAINEDINSPATIALSTRUCTURE"
            })
            .count();
        tracing::debug!(
            pset_count = pset_count,
            rel_count = rel_count,
            "Entity type counts before extraction"
        );
    }
}

/// Collect the `RelatedObjects` (attribute 4) entity ids of an `IfcRelAssociates*`.
pub(super) fn related_object_ids(rel: &DecodedEntity) -> Vec<u32> {
    rel.get_list(4)
        .map(|list| list.iter().filter_map(|v| v.as_entity_ref()).collect())
        .unwrap_or_default()
}


#[cfg(test)]
#[path = "tests.rs"]
mod tests;
