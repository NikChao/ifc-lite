// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! IFC Z-up to the renderer's Y-up frame, which is what the cache stores.
//!
//! `ifcToViewerAxes` in @ifc-lite/geometry is the definition:
//!
//! ```js
//! { x: point.x, y: point.z, z: 0 - point.y }
//! ```
//!
//! The native pipeline emits IFC axes (`MeshFrame`: "IFC axes, Z up, metres"),
//! so every vertex, normal, origin and bound is converted on the way out.
//! Writing Z-up verbatim produces a cache that parses perfectly and renders the
//! building on its side -- and makes `simplifyModel`, which reads `y` as
//! elevation and `[x, z]` as the plan, cluster levels along a horizontal axis
//! and find no rooms at all.
//!
//! `CoordinateInfo.originShift` is stored Y-up for the same reason;
//! `wasmRtcOffset` is the exception and stays IFC Z-up.

/// One point, IFC Z-up to viewer Y-up.
#[inline]
pub fn point_f32(x: f32, y: f32, z: f32) -> [f32; 3] {
    [x, z, -y]
}

/// The same for the f64 fields (origins, offsets, bounds in metres).
#[inline]
pub fn point_f64(value: [f64; 3]) -> [f64; 3] {
    [value[0], value[2], -value[1]]
}

/// An axis-aligned box. Negating an axis swaps its min and max, so the
/// converted corners are re-ordered rather than mapped in place.
pub fn aabb_f32(min: [f32; 3], max: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let a = point_f32(min[0], min[1], min[2]);
    let b = point_f32(max[0], max[1], max[2]);
    (
        [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
        [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
    )
}

pub fn aabb_f64(min: [f64; 3], max: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let a = point_f64(min);
    let b = point_f64(max);
    (
        [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
        [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
    )
}

/// A column-major 4x4 placement, rebased into the viewer frame.
///
/// A matrix is a basis change, not a point: mapping its columns the way a
/// vertex is mapped would rotate the geometry twice. The correct form is
/// `R * M * R^-1`, where `R` is the axis swap above. `R` is its own inverse
/// transpose here only for the rotation block, so the translation column is
/// converted as a point and the basis block is conjugated.
pub fn matrix_f64(m: [f64; 16]) -> [f64; 16] {
    // Column-major: m[col * 4 + row].
    let at = |col: usize, row: usize| m[col * 4 + row];

    // R maps (x, y, z) -> (x, z, -y): row 0 <- x, row 1 <- z, row 2 <- -y.
    let map_row = |col: usize| [at(col, 0), at(col, 2), -at(col, 1)];

    // Conjugation: the basis columns are themselves permuted, because a column
    // of M is the image of a basis vector that R has also moved.
    let bx = map_row(0);
    let by = map_row(1);
    let bz = map_row(2);
    let translation = map_row(3);

    // Columns in the new basis order: x stays, y takes the old z, z takes -old y.
    let (cx, cy, cz) = (bx, bz, [-by[0], -by[1], -by[2]]);

    [
        cx[0], cx[1], cx[2], at(0, 3),
        cy[0], cy[1], cy[2], at(1, 3),
        cz[0], cz[1], cz[2], at(2, 3),
        translation[0], translation[1], translation[2], at(3, 3),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z_up_becomes_y_up() {
        // A point 10m above the ground in IFC is 10m up the viewer's Y.
        assert_eq!(point_f32(1.0, 2.0, 10.0), [1.0, 10.0, -2.0]);
        assert_eq!(point_f64([1.0, 2.0, 10.0]), [1.0, 10.0, -2.0]);
    }

    #[test]
    fn a_tower_stands_up() {
        // Tall in IFC Z must be tall in viewer Y, which is what "upright" means.
        let (min, max) = aabb_f32([0.0, 0.0, 0.0], [10.0, 20.0, 100.0]);
        assert_eq!(max[1] - min[1], 100.0, "height along viewer Y");
        assert_eq!(max[0] - min[0], 10.0);
        assert_eq!(max[2] - min[2], 20.0);
    }

    #[test]
    fn negated_axis_keeps_min_below_max() {
        let (min, max) = aabb_f64([0.0, 5.0, 0.0], [1.0, 9.0, 1.0]);
        assert!(min[2] <= max[2], "min {min:?} max {max:?}");
        assert_eq!((min[2], max[2]), (-9.0, -5.0));
    }

    #[test]
    fn identity_stays_identity() {
        let mut identity = [0.0; 16];
        for i in 0..4 {
            identity[i * 4 + i] = 1.0;
        }
        assert_eq!(matrix_f64(identity), identity);
    }

    #[test]
    fn a_translation_moves_like_a_point() {
        let mut m = [0.0; 16];
        for i in 0..4 {
            m[i * 4 + i] = 1.0;
        }
        // Column 3 is the translation: 10m up IFC Z.
        m[12] = 1.0;
        m[13] = 2.0;
        m[14] = 10.0;
        let out = matrix_f64(m);
        assert_eq!([out[12], out[13], out[14]], [1.0, 10.0, -2.0]);
    }
}
