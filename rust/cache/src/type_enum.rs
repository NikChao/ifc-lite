// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! IFC class name to its `IfcTypeEnum` ordinal, generated from @ifc-lite/data.
//!
//! The Entities section stores this ordinal in its `typeEnum` column. A shifted
//! ordinal silently retypes every element of that class in a cache rather than
//! failing, so regenerate whenever @ifc-lite/data moves:
//!
//!   node --input-type=module -e "import { IfcTypeEnum } from
//!   './node_modules/@ifc-lite/data/dist/types.js'; ..." (see git history)
//!
//! Keys are lower-cased: a STEP file spells the class `IFCWALL` and the schema
//! registry spells it `IfcWall`, and both resolve to one ordinal.

/// Sorted by name, so lookup is a binary search and allocates nothing but the
/// lower-cased key.
static TYPE_ORDINALS: &[(&str, u16)] = &[
    ("ifcalignment", 89),
    ("ifcannotation", 55),
    ("ifcbeam", 16),
    ("ifcbeamtype", 305),
    ("ifcbridge", 61),
    ("ifcbridgepart", 62),
    ("ifcbuilding", 3),
    ("ifcbuildingelementpart", 33),
    ("ifcbuildingelementproxy", 27),
    ("ifcbuildingelementproxytype", 316),
    ("ifcbuildingstorey", 4),
    ("ifccablesegment", 51),
    ("ifcchimney", 31),
    ("ifccivilelement", 57),
    ("ifccolumn", 15),
    ("ifccolumntype", 304),
    ("ifccourse", 70),
    ("ifccovering", 20),
    ("ifccoveringtype", 310),
    ("ifccurtainwall", 21),
    ("ifccurtainwalltype", 315),
    ("ifcdiscreteaccessory", 38),
    ("ifcdistributionelement", 40),
    ("ifcdistributionsystem", 320),
    ("ifcdoor", 12),
    ("ifcdoortype", 301),
    ("ifcductsegment", 49),
    ("ifcearthworkscut", 79),
    ("ifcearthworkselement", 77),
    ("ifcearthworksfill", 78),
    ("ifcelementassembly", 34),
    ("ifcelementquantity", 210),
    ("ifcenergyconversiondevice", 48),
    ("ifcfacility", 59),
    ("ifcfacilitypart", 60),
    ("ifcfacilitypartcommon", 322),
    ("ifcflowcontroller", 44),
    ("ifcflowfitting", 43),
    ("ifcflowmovingdevice", 45),
    ("ifcflowsegment", 42),
    ("ifcflowstoragedevice", 46),
    ("ifcflowterminal", 41),
    ("ifcflowtreatmentdevice", 47),
    ("ifcfooting", 26),
    ("ifcfootingtype", 309),
    ("ifcfurnishingelement", 52),
    ("ifcfurniture", 53),
    ("ifcgeographicelement", 58),
    ("ifcgeotechnicalassembly", 84),
    ("ifcgeotechnicalstratum", 83),
    ("ifckerb", 72),
    ("ifcmarinefacility", 67),
    ("ifcmarinepart", 321),
    ("ifcmechanicalfastener", 39),
    ("ifcmember", 24),
    ("ifcmembertype", 307),
    ("ifcmooringdevice", 73),
    ("ifcnavigationelement", 74),
    ("ifcopeningelement", 30),
    ("ifcpavement", 71),
    ("ifcpile", 23),
    ("ifcpiletype", 306),
    ("ifcpipesegment", 50),
    ("ifcplate", 25),
    ("ifcplatetype", 308),
    ("ifcpositioningelement", 88),
    ("ifcproject", 1),
    ("ifcpropertyboundedvalue", 203),
    ("ifcpropertyenumeratedvalue", 202),
    ("ifcpropertylistvalue", 204),
    ("ifcpropertyset", 200),
    ("ifcpropertysinglevalue", 201),
    ("ifcproxy", 54),
    ("ifcquantityarea", 212),
    ("ifcquantitycount", 214),
    ("ifcquantitylength", 211),
    ("ifcquantityvolume", 213),
    ("ifcquantityweight", 215),
    ("ifcrailing", 22),
    ("ifcrailingtype", 311),
    ("ifcrailway", 65),
    ("ifcrailwaypart", 66),
    ("ifcramp", 18),
    ("ifcrampflight", 29),
    ("ifcramptype", 313),
    ("ifcreferent", 80),
    ("ifcreinforcingbar", 35),
    ("ifcreinforcingmesh", 36),
    ("ifcrelaggregates", 101),
    ("ifcrelassociatesclassification", 105),
    ("ifcrelassociatesmaterial", 104),
    ("ifcrelconnectspathelements", 108),
    ("ifcrelcontainedinspatialstructure", 100),
    ("ifcreldefinesbyproperties", 102),
    ("ifcreldefinesbytype", 103),
    ("ifcrelfillselement", 107),
    ("ifcrelspaceboundary", 109),
    ("ifcrelvoidselement", 106),
    ("ifcroad", 63),
    ("ifcroadpart", 64),
    ("ifcroof", 19),
    ("ifcrooftype", 314),
    ("ifcshadingdevice", 32),
    ("ifcsign", 81),
    ("ifcsignal", 82),
    ("ifcsite", 2),
    ("ifcslab", 14),
    ("ifcslabtype", 303),
    ("ifcsolidstratum", 85),
    ("ifcspace", 5),
    ("ifcspatialzone", 317),
    ("ifcstair", 17),
    ("ifcstairflight", 28),
    ("ifcstairtype", 312),
    ("ifcsystem", 319),
    ("ifctendon", 37),
    ("ifctrackelement", 75),
    ("ifctransportelement", 56),
    ("ifcvehicle", 76),
    ("ifcvoidstratum", 86),
    ("ifcwall", 10),
    ("ifcwallstandardcase", 11),
    ("ifcwalltype", 300),
    ("ifcwaterstratum", 87),
    ("ifcwindow", 13),
    ("ifcwindowtype", 302),
    ("ifczone", 318),
];

/// The ordinal for an IFC class name, or 0 (`Unknown`) where the schema registry
/// has no enum member. Only about a quarter of concrete product classes have one,
/// which is why the section also carries the raw class name: 0 degrades to a
/// named type rather than losing the type.
pub fn type_ordinal(type_name: &str) -> u16 {
    let key = type_name.to_ascii_lowercase();
    match TYPE_ORDINALS.binary_search_by(|(name, _)| (*name).cmp(&key.as_str())) {
        Ok(at) => TYPE_ORDINALS[at].1,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_regardless_of_case() {
        assert_eq!(type_ordinal("IfcWall"), 10);
        assert_eq!(type_ordinal("IFCWALL"), 10);
    }

    #[test]
    fn spatial_classes_match_the_registry() {
        assert_eq!(type_ordinal("IfcProject"), 1);
        assert_eq!(type_ordinal("IfcBuildingStorey"), 4);
        assert_eq!(type_ordinal("IfcSpace"), 5);
    }

    #[test]
    fn unknown_class_is_zero() {
        assert_eq!(type_ordinal("IfcNotARealClass"), 0);
    }

    #[test]
    fn table_is_sorted_for_binary_search() {
        assert!(TYPE_ORDINALS.windows(2).all(|w| w[0].0 < w[1].0));
    }
}
