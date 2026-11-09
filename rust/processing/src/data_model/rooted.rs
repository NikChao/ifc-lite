// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Which rows the data model's entities table carries (issue #6034).
//!
//! Its own module because it is a WIRE CONTRACT as much as a filter: the parse
//! routes read it off the query, the data-model cache key is built from it, the
//! fetch route selects by it, and the client sends it. One definition keeps
//! those four in step, the same shape as `parquet_layout.rs`.

use super::slots::RootSlots;
use super::types::{
    ClassificationAssociation, DataModel, DocumentAssociation, EntityJob, MaterialAssociation,
    PropertySet, QuantitySet, Relationship,
};
use rustc_hash::{FxHashMap, FxHashSet};

/// Which STEP instances the data model's entities table carries.
///
/// OPT-IN, defaulting to [`Self::All`], because the default table is a public
/// payload: a client that looks up a non-rooted instance by id (an
/// `IfcCartesianPoint`, an `IfcPropertySingleValue`) keeps working, and a
/// default request writes and reads exactly the bytes it did before #6034.
///
/// [`Self::Rooted`] is for the clients that only ever look up objects. On a
/// large architectural model about five in six entity rows are geometry and
/// property plumbing with no GlobalId (`IfcPolyLoop`, `IfcFace`,
/// `IfcCartesianPoint`, `IfcPropertySingleValue`, ...), none of which another
/// table of the data model points at: property values are already flattened
/// into the properties table, quantities into the quantities table.
///
/// The two variants are cached under separate keys (see
/// `routes::parse::cache_keys::data_model_cache_key`), so a warm cache can
/// never answer one with the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
pub enum DataModelEntities {
    /// Every STEP instance in the file. The only behaviour before #6034.
    #[default]
    #[serde(rename = "all")]
    All,
    /// Only rooted entities (`IfcRoot` subtypes, the ones carrying a
    /// GlobalId), plus every non-rooted instance another table of the same
    /// data model references by id, so no relation dangles. See
    /// [`retain_rooted_entities`].
    #[serde(rename = "rooted")]
    Rooted,
}

impl DataModelEntities {
    /// The data-model cache-key infix naming this variant. Empty for
    /// [`Self::All`], so a default request keeps hitting the entries already
    /// on disk.
    pub fn cache_infix(self) -> &'static str {
        match self {
            Self::All => "",
            Self::Rooted => "-rooted",
        }
    }

    /// Narrow `data_model`'s entities table to this variant. A no-op for
    /// [`Self::All`]; every other table is left untouched either way.
    pub fn apply(self, data_model: &mut DataModel) {
        if self == Self::Rooted {
            retain_rooted_entities(data_model);
        }
    }
}

/// Keep the entity rows a rooted-only client can need, dropping the rest.
///
/// A row is kept when EITHER
/// - it is rooted: it carries a GlobalId, or its type is one the schema
///   registry says declares one (an `IfcRoot` subtype whose GlobalId the file
///   left empty is still an object); or
/// - another table of this data model references its id: a material, a
///   material layer set or usage (`materials.definition_id` /
///   `material_id`), the `RelatingMaterial` / `RelatingClassification` /
///   `RelatingDocument` of an `IfcRelAssociates*` row, and so on.
///
/// The second rule is what keeps every relation resolvable. Without it the
/// relationships table would name `IfcMaterial` and `IfcClassificationReference`
/// ids the entities table no longer describes, and a client resolving a
/// relation's endpoint to its type or name through the entities table would
/// find nothing. They are few, so keeping them costs nothing measurable; the
/// rows this drops are the ones nothing points at.
///
/// `entity_id`s are untouched, and the kept rows stay in file order.
pub(crate) fn retain_rooted_entities(data_model: &mut DataModel) {
    let referenced = referenced_ids(data_model);
    // Types are few (hundreds) and rows are many (millions), so the registry
    // lookup runs once per type.
    let mut rooted_type: FxHashMap<String, bool> = FxHashMap::default();
    let keep: Vec<bool> = data_model
        .entities
        .iter()
        .map(|entity| {
            if entity.global_id.is_some() || referenced.contains(&entity.entity_id) {
                return true;
            }
            if let Some(&rooted) = rooted_type.get(entity.type_name.as_str()) {
                return rooted;
            }
            let rooted = declares_global_id(&entity.type_name);
            rooted_type.insert(entity.type_name.clone(), rooted);
            rooted
        })
        .collect();
    let mut keep = keep.into_iter();
    // `retain` visits every element exactly once, in order.
    data_model
        .entities
        .retain(|_| keep.next().unwrap_or(true));
}

/// Whether the schema registry lists `type_name` as declaring a GlobalId,
/// i.e. as an `IfcRoot` subtype. A type the registry does not know answers
/// `false`: for those the extracted `global_id` (read at the `IfcRoot`
/// position by the metadata fallback) is the only evidence there is.
fn declares_global_id(type_name: &str) -> bool {
    RootSlots::declares_global_id(type_name)
}

/// Every entity id another table of `data_model` points at. `0` is the
/// tables' "none" (a root spatial node's parent, a synthetic relationship's
/// `rel_id`), never an instance, so it is skipped.
fn referenced_ids(data_model: &DataModel) -> FxHashSet<u32> {
    let mut ids = FxHashSet::default();
    let mut add = |id: u32| {
        if id != 0 {
            ids.insert(id);
        }
    };
    for rel in &data_model.relationships {
        add(rel.rel_id);
        add(rel.relating_id);
        add(rel.related_id);
    }
    for pset in &data_model.property_sets {
        add(pset.pset_id);
    }
    for qset in &data_model.quantity_sets {
        add(qset.qset_id);
    }
    for row in &data_model.classifications {
        add(row.element_id);
    }
    for row in &data_model.materials {
        add(row.element_id);
        add(row.association_id);
        add(row.definition_id);
        if let Some(material_id) = row.material_id {
            add(material_id);
        }
    }
    for row in &data_model.documents {
        add(row.element_id);
    }
    let spatial = &data_model.spatial_hierarchy;
    add(spatial.project_id);
    for node in &spatial.nodes {
        add(node.entity_id);
        add(node.parent_id);
        node.children_ids.iter().for_each(|&id| add(id));
        node.element_ids.iter().for_each(|&id| add(id));
    }
    for lookup in [
        &spatial.element_to_storey,
        &spatial.element_to_building,
        &spatial.element_to_site,
        &spatial.element_to_space,
    ] {
        for &(element, container) in lookup {
            add(element);
            add(container);
        }
    }
    ids
}

#[cfg(test)]
#[path = "rooted_tests.rs"]
mod tests;

/// The association tables, borrowed together so the pre-filter takes one
/// argument for them rather than three. They are grouped because they are one
/// fact: the output of the association pass, every row of which can name a
/// non-rooted id.
#[derive(Clone, Copy)]
pub(crate) struct AssociationRefs<'a> {
    pub classifications: &'a [ClassificationAssociation],
    pub materials: &'a [MaterialAssociation],
    pub documents: &'a [DocumentAssociation],
}

/// The jobs whose metadata a `DataModelEntities::Rooted` extraction still has
/// to build, so the rest are never materialised.
///
/// [`retain_rooted_entities`] decides with the extracted row, which knows
/// whether a GlobalId was actually present. Here only the type name is known,
/// so the test is deliberately WIDER in one place: a type the IFC4 registry
/// does not list is kept, because for those the extracted GlobalId is the only
/// evidence there is and dropping them here would lose rows the post-filter
/// would have kept. Running `retain_rooted_entities` afterwards applies the
/// exact rule to the much smaller table, so the two paths agree.
///
/// Vendor extensions are the realistic unknown type and they are rare; a file
/// made entirely of them simply costs what the unfiltered path costs today.
///
/// The caller must have built EVERY other table first, associations included.
/// The reference set below has to be the same one [`referenced_ids`] builds, or
/// this drops a row the post-filter keeps and the two paths stop agreeing. That
/// is not hypothetical: the associations were once extracted after this ran,
/// and the catalogued models lost every non-rooted `IfcMaterial` the materials
/// table points at -- 88 rows on `ara3d/ISSUE_068_ARK_NUS_skolebygg.ifc`.
pub(crate) fn metadata_jobs<'a>(
    entities: DataModelEntities,
    all_entities: &'a [EntityJob<'a>],
    property_sets: &[PropertySet],
    quantity_sets: &[QuantitySet],
    relationships: &[Relationship],
    associations: AssociationRefs<'_>,
) -> Vec<EntityJob<'a>> {
    if entities == DataModelEntities::All {
        return all_entities.to_vec();
    }

    let referenced = referenced_by_tables(property_sets, quantity_sets, relationships, associations);
    // Types are few (hundreds) and rows are many (millions), so the registry
    // lookup runs once per type rather than once per row.
    let mut keep_type: FxHashMap<&str, bool> = FxHashMap::default();
    all_entities
        .iter()
        .filter(|job| {
            if referenced.contains(&job.id) {
                return true;
            }
            if let Some(&keep) = keep_type.get(job.type_name) {
                return keep;
            }
            let keep = match RootSlots::for_type(job.type_name) {
                Some(slots) => slots.global_id.is_some(),
                None => true,
            };
            keep_type.insert(job.type_name, keep);
            keep
        })
        .copied()
        .collect()
}

/// Every entity id the small tables point at, the ids
/// [`referenced_ids`] collects before the entities table exists.
fn referenced_by_tables(
    property_sets: &[PropertySet],
    quantity_sets: &[QuantitySet],
    relationships: &[Relationship],
    associations: AssociationRefs<'_>,
) -> FxHashSet<u32> {
    let mut ids = FxHashSet::default();
    let mut add = |id: u32| {
        if id != 0 {
            ids.insert(id);
        }
    };
    for set in property_sets {
        add(set.pset_id);
    }
    for set in quantity_sets {
        add(set.qset_id);
    }
    for relationship in relationships {
        add(relationship.rel_id);
        add(relationship.relating_id);
        add(relationship.related_id);
    }
    for row in associations.classifications {
        add(row.element_id);
    }
    // `definition_id` and `material_id` are the non-rooted ones: an
    // `IfcMaterial` or an `IfcMaterialLayerSet` carries no GlobalId, and no
    // other table names it.
    for row in associations.materials {
        add(row.element_id);
        add(row.association_id);
        add(row.definition_id);
        if let Some(material_id) = row.material_id {
            add(material_id);
        }
    }
    for row in associations.documents {
        add(row.element_id);
    }
    ids
}
