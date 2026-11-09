// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The relationship graph, mirroring `sections/relationships.js`.
//!
//! The section is the forward half followed by the inverse half, each laid out
//! as: nodeCount u32, then [entityId u32, offset u32, count u32] per node,
//! edgeCount u32, edgeTargets u32[edgeCount], edgeTypes u16[edgeCount],
//! edgeRelIds u32[edgeCount], then the v18 shadow tail --
//! shadowedEdgeIndex length u32 and array, shadowedGroupOffsets array
//! (length + 1 entries), shadowedRelIds length u32 and array.
//!
//! The node entries are offsets into the edge arrays, so edges must be grouped
//! by node and written in that order: a node's edges are `edgeTargets[offset ..
//! offset + count]`. The viewer rebuilds the spatial hierarchy from this
//! section on load, which is why the Entities section can leave
//! `containedInStorey` absent.
//!
//! The shadow tail records edges collapsed from more than one `IfcRel*`
//! instance. Nothing here collapses edges -- every relationship is emitted as
//! it was extracted -- so both halves write the empty form: zero groups, a
//! single zero offset, zero ids. That is the same shape the TS writer emits for
//! a graph with no shadowing, which is the overwhelming majority.

use std::collections::HashMap;

use crate::buf::Buf;

/// Matches `RelationshipType` in @ifc-lite/data. Only the members the data
/// model actually produces are listed; an unmapped relationship is dropped
/// rather than guessed, because a wrong code reads back as a different kind of
/// edge entirely.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u16)]
pub enum RelationshipType {
    ContainsElements = 1,
    Aggregates = 2,
    Nests = 3,
    DefinesByProperties = 10,
    DefinesByType = 11,
    AssociatesMaterial = 20,
    AssociatesClassification = 30,
    AssociatesDocument = 31,
    FillsElement = 41,
    VoidsElement = 42,
    AssignsToGroup = 60,
    ReferencedInSpatialStructure = 70,
    Declares = 80,
}

/// Resolves the `IfcRel*` class name `extract_data_model` reports.
pub fn relationship_type(class_name: &str) -> Option<RelationshipType> {
    Some(match class_name.to_ascii_lowercase().as_str() {
        "ifcrelcontainedinspatialstructure" => RelationshipType::ContainsElements,
        "ifcrelaggregates" => RelationshipType::Aggregates,
        "ifcrelnests" => RelationshipType::Nests,
        "ifcreldefinesbyproperties" => RelationshipType::DefinesByProperties,
        "ifcreldefinesbytype" => RelationshipType::DefinesByType,
        "ifcrelassociatesmaterial" => RelationshipType::AssociatesMaterial,
        "ifcrelassociatesclassification" => RelationshipType::AssociatesClassification,
        "ifcrelassociatesdocument" => RelationshipType::AssociatesDocument,
        "ifcrelfillselement" => RelationshipType::FillsElement,
        "ifcrelvoidselement" => RelationshipType::VoidsElement,
        "ifcrelassignstogroup" => RelationshipType::AssignsToGroup,
        "ifcrelreferencedinspatialstructure" => RelationshipType::ReferencedInSpatialStructure,
        "ifcreldeclares" => RelationshipType::Declares,
        _ => return None,
    })
}

struct Edge {
    target: u32,
    edge_type: u16,
    rel_id: u32,
}

/// One direction of the graph, grouped by source node.
#[derive(Default)]
struct Half {
    order: Vec<u32>,
    edges: HashMap<u32, Vec<Edge>>,
}

impl Half {
    fn push(&mut self, from: u32, edge: Edge) {
        match self.edges.get_mut(&from) {
            Some(existing) => existing.push(edge),
            None => {
                self.order.push(from);
                self.edges.insert(from, vec![edge]);
            }
        }
    }

    fn write(&self, buf: &mut Buf) {
        buf.u32(self.order.len() as u32);
        let mut offset = 0u32;
        for node in &self.order {
            let count = self.edges[node].len() as u32;
            buf.u32(*node);
            buf.u32(offset);
            buf.u32(count);
            offset += count;
        }

        let total = offset;
        buf.u32(total);
        for node in &self.order {
            for edge in &self.edges[node] {
                buf.u32(edge.target);
            }
        }
        for node in &self.order {
            for edge in &self.edges[node] {
                buf.u16(edge.edge_type);
            }
        }
        for node in &self.order {
            for edge in &self.edges[node] {
                buf.u32(edge.rel_id);
            }
        }

        // The empty shadow tail: no groups, the single anchoring offset, no ids.
        buf.u32(0);
        buf.u32(0);
        buf.u32(0);
    }
}

#[derive(Default)]
pub struct RelationshipGraph {
    forward: Half,
    inverse: Half,
}

impl RelationshipGraph {
    pub fn new() -> Self {
        RelationshipGraph::default()
    }

    /// Records one edge in both directions, as the graph builder does: forward
    /// from the relating entity to the related one, inverse the other way.
    pub fn push(&mut self, rel_id: u32, rel_type: RelationshipType, relating: u32, related: u32) {
        let edge_type = rel_type as u16;
        self.forward.push(
            relating,
            Edge {
                target: related,
                edge_type,
                rel_id,
            },
        );
        self.inverse.push(
            related,
            Edge {
                target: relating,
                edge_type,
                rel_id,
            },
        );
    }

    pub fn write(&self) -> Vec<u8> {
        let mut buf = Buf::new();
        self.forward.write(&mut buf);
        self.inverse.write(&mut buf);
        buf.into_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn unmapped_relationship_classes_are_reported_absent() {
        assert_eq!(relationship_type("IfcRelConnectsPorts"), None);
        assert_eq!(
            relationship_type("IFCRELAGGREGATES"),
            Some(RelationshipType::Aggregates)
        );
    }

    #[test]
    fn node_offsets_address_their_own_edges() {
        let mut graph = RelationshipGraph::new();
        // One storey containing two elements.
        graph.push(100, RelationshipType::ContainsElements, 1, 10);
        graph.push(100, RelationshipType::ContainsElements, 1, 11);
        graph.push(101, RelationshipType::Aggregates, 2, 12);

        let bytes = graph.write();
        assert_eq!(read_u32(&bytes, 0), 2, "two forward nodes");
        // node 1 at offset 0 with 2 edges, node 2 at offset 2 with 1.
        assert_eq!(read_u32(&bytes, 4), 1);
        assert_eq!(read_u32(&bytes, 8), 0);
        assert_eq!(read_u32(&bytes, 12), 2);
        assert_eq!(read_u32(&bytes, 16), 2);
        assert_eq!(read_u32(&bytes, 20), 2);
        assert_eq!(read_u32(&bytes, 24), 1);
        assert_eq!(read_u32(&bytes, 28), 3, "three edges in total");
    }

    #[test]
    fn every_edge_appears_in_both_halves() {
        let mut graph = RelationshipGraph::new();
        graph.push(100, RelationshipType::ContainsElements, 1, 10);

        // Forward: one node (1). Inverse: one node (10). Both carry one edge.
        let bytes = graph.write();
        let forward_nodes = read_u32(&bytes, 0);
        assert_eq!(forward_nodes, 1);
        // forward: 4 + 12 + 4 (edgeCount) + 4 + 2 + 4 + 12 (shadow tail) = 42
        let inverse_at = 4 + 12 + 4 + 4 + 2 + 4 + 12;
        assert_eq!(read_u32(&bytes, inverse_at), 1, "one inverse node");
        assert_eq!(read_u32(&bytes, inverse_at + 4), 10, "keyed by the related entity");
    }
}
