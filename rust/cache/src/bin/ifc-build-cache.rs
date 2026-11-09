// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Builds a `.ifclite` cache from one IFC file, and measures what that costs.
//!
//! `<source.ifc> <cache-output>` writes the cache. With no output it measures
//! the parse and the geometry pass instead, which is how the memory questions
//! behind this crate were answered and how the next one will be.
//!
//! By default the meshes leave through the sink and are dropped, which measures
//! the pipeline's own floor. `--retain` holds every mesh instead, which is what
//! `BinaryCacheWriter` and `simplifyModel` read: that run measures whether the
//! writer's input fits under the Lambda ceiling at all.
//!
//! With `--cache <out>` it instead builds the `.ifclite` model cache at that
//! path, streaming each mesh into its chunk as the pipeline emits it, and
//! prints the same stats shape `buildIfcModelCache` reads from the TS builder.
//!
//! Prints one JSON object on stdout; progress goes to stderr.

use std::fs::File;
use std::time::Instant;

use ifc_lite_processing::{process, MeshData, ProcessingRequest, ProcessingSink};
use memmap2::Mmap;

/// This process's resident peak. macOS reports `ru_maxrss` in bytes, Linux in KiB.
fn peak_rss_bytes() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return 0;
    }
    let maxrss = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        maxrss
    } else {
        maxrss * 1024
    }
}

fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// What the retained meshes occupy, as the cache writer would find them: the
/// three vertex arrays, the per-mesh strings, and the structs holding them.
/// Peak RSS covers this and the allocator's slack; this is the part a writer
/// cannot avoid paying for.
fn retained_bytes(meshes: &[MeshData]) -> u64 {
    let string_len = |s: &Option<String>| s.as_ref().map_or(0, String::len) as u64;
    meshes
        .iter()
        .map(|mesh| {
            (mesh.positions.len() * 4 + mesh.normals.len() * 4 + mesh.indices.len() * 4) as u64
                + mesh.ifc_type.len() as u64
                + string_len(&mesh.global_id)
                + string_len(&mesh.name)
                + string_len(&mesh.presentation_layer)
                + string_len(&mesh.material_name)
                + std::mem::size_of::<MeshData>() as u64
        })
        .sum()
}

/// Counts what the pass emits, holding no mesh past the batch it arrived in.
struct Probe {
    meshes: usize,
    triangles: usize,
    vertices: usize,
    batches: usize,
    last_report: Instant,
}

impl ProcessingSink for Probe {
    fn on_batch(&mut self, meshes: &[MeshData], processed: usize, total: usize) {
        self.batches += 1;
        self.meshes += meshes.len();
        for mesh in meshes {
            self.triangles += mesh.indices.len() / 3;
            self.vertices += mesh.positions.len() / 3;
        }
        if self.last_report.elapsed().as_secs() >= 10 {
            eprintln!(
                "[ifc-build-cache] {processed}/{total} jobs, {} meshes, rss {} MiB",
                self.meshes,
                mib(peak_rss_bytes()),
            );
            self.last_report = Instant::now();
        }
    }
}

fn main() {
    // Positional, matching the TS builder's `<source.ifc> [<cache-output>]`, so
    // the activity passes the same argv to either one.
    let mut positional = Vec::new();
    let mut retain = false;
    let mut data_model_only = false;
    for arg in std::env::args().skip(1) {
        if arg == "--retain" {
            retain = true;
        } else if arg == "--data-model" {
            data_model_only = true;
        } else {
            positional.push(arg);
        }
    }
    let mut positional = positional.into_iter();
    let path = positional.next();
    let cache_out = positional.next();
    let path = match path {
        Some(path) => path,
        None => {
            eprintln!("usage: ifc-build-cache [--retain] <model.ifc> [<cache-output>]");
            std::process::exit(2);
        }
    };

    let file = File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    // Mapped rather than read: the point of the native path is that the source is
    // file-backed and reclaimable instead of a heap copy inside every worker.
    let source = unsafe { Mmap::map(&file) }.unwrap_or_else(|e| panic!("mmap {path}: {e}"));
    eprintln!("[ifc-build-cache] mapped {} MiB from {path}", mib(source.len() as u64));

    if data_model_only {
        let started = Instant::now();
        // The variant the cache build uses, so the number describes that path
        // rather than the unfiltered one.
        let model = ifc_lite_processing::data_model::extract_data_model_with(
            &source[..],
            ifc_lite_processing::data_model::DataModelEntities::Rooted,
        );
        println!(
            concat!(
                r#"{{"stage":"data_model","entities":{},"property_sets":{},"#,
                r#""quantity_sets":{},"relationships":{},"seconds":{:.1},"peak_rss_mib":{}}}"#
            ),
            model.entities.len(),
            model.property_sets.len(),
            model.quantity_sets.len(),
            model.relationships.len(),
            started.elapsed().as_secs_f64(),
            mib(peak_rss_bytes()),
        );
        return;
    }

    if let Some(out) = cache_out {
        build_model_cache(&source, &out);
        return;
    }

    let mut request = ProcessingRequest::default();
    request.retain_meshes = retain;
    // The geometry pass resolves property sets and presentation layers by
    // default. The cache takes its properties from the data model instead, so
    // IFC_GEOMETRY_FEATURES=off asks what that resolution costs: a model with
    // 474k property sets spends the whole memory budget before the writer runs.
    if std::env::var("IFC_GEOMETRY_FEATURES").as_deref() == Ok("off") {
        request.features = request.features.properties(false).presentation_layers(false);
    }
    // Off by default in Rust but on in the build the viewer's cache comes from,
    // and it changes the mesh count, so the measurement matches without it.
    request.layer_slicing = true;

    let mut probe = Probe {
        meshes: 0,
        triangles: 0,
        vertices: 0,
        batches: 0,
        last_report: Instant::now(),
    };

    let started = Instant::now();
    let result = process(&source, &request, &mut probe);
    let elapsed = started.elapsed();

    println!(
        concat!(
            r#"{{"file_mib":{},"wall_seconds":{:.1},"peak_rss_mib":{},"#,
            r#""meshes":{},"batches":{},"triangles":{},"vertices":{},"#,
            r#""retained_meshes":{},"retained_mib":{}}}"#
        ),
        mib(source.len() as u64),
        elapsed.as_secs_f64(),
        mib(peak_rss_bytes()),
        probe.meshes,
        probe.batches,
        probe.triangles,
        probe.vertices,
        result.meshes.len(),
        mib(retained_bytes(&result.meshes)),
    );
}

/// Builds the model cache and prints the stats `buildIfcModelCache` parses.
/// Field names match the TS builder's `ModelCacheStats` so the activity reads
/// either builder without knowing which one ran.
fn build_model_cache(source: &[u8], out: &str) {
    let cache = File::create(out).unwrap_or_else(|e| panic!("create {out}: {e}"));
    // Beside the cache, so it lands on the same filesystem -- Lambda gives
    // /tmp 10 GiB, which holds both.
    let scratch_path = format!("{out}.chunks");
    let scratch = File::options()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&scratch_path)
        .unwrap_or_else(|e| panic!("create {scratch_path}: {e}"));

    let stats = ifc_lite_cache::build::build_cache(source, cache, scratch)
        .unwrap_or_else(|e| panic!("build cache: {e}"));
    let _ = std::fs::remove_file(&scratch_path);

    println!(
        concat!(
            r#"{{"entityCount":{},"meshCount":{},"cacheBytes":{},"parseMs":{},"#,
            r#""geometryMs":{},"uncutHosts":0,"totalVertices":{},"totalTriangles":{},"#,
            r#""peakRssMib":{}}}"#
        ),
        stats.entity_count,
        stats.mesh_count,
        stats.cache_bytes,
        stats.parse_ms,
        stats.geometry_ms,
        stats.total_vertices,
        stats.total_triangles,
        mib(peak_rss_bytes()),
    );
}
