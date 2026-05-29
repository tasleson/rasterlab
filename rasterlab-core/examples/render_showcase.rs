//! Loads `test_images/showcase.rlab` and renders each virtual copy through the
//! pipeline. Useful as a smoke test after running `gen_showcase_rlab` (and as a
//! release-eve check that every op in the showcase still applies cleanly).
//!
//! ```bash
//! cargo run --release -p rasterlab-core --example render_showcase
//! ```

use std::path::Path;

use rasterlab_core::{formats::FormatRegistry, pipeline::EditPipeline, project::RlabFile};

fn main() {
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(16 * 1024 * 1024)
        .build_global();

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rlab_path = manifest
        .parent()
        .expect("workspace root")
        .join("test_images/showcase.rlab");

    let rlab = RlabFile::read(&rlab_path)
        .unwrap_or_else(|e| panic!("read {}: {}", rlab_path.display(), e));
    let registry = FormatRegistry::with_builtins();
    let src = registry
        .decode_bytes(
            &rlab.original_bytes,
            rlab.meta.source_path.as_deref().map(Path::new),
        )
        .expect("decode embedded source");

    println!(
        "source: {}×{} ({} bytes encoded)",
        src.width,
        src.height,
        rlab.original_bytes.len()
    );
    println!("active copy: {}", rlab.active_copy_index);

    let mut all_ok = true;
    for (i, copy) in rlab.copies.iter().enumerate() {
        let mut p = EditPipeline::new(src.deep_clone());
        if let Err(e) = p.load_state(copy.pipeline_state.clone()) {
            println!("copy {i} ({}): load_state error: {e}", copy.name);
            all_ok = false;
            continue;
        }
        let ops = p.ops().len();
        let cursor = p.cursor();
        let enabled = p.ops().iter().filter(|e| e.enabled).count();
        match p.render() {
            Ok(out) => println!(
                "copy {i:>2}  {:<24}  ops={ops:>2} (enabled={enabled:>2})  cursor={cursor:>2}  rendered {}×{}",
                copy.name, out.width, out.height,
            ),
            Err(e) => {
                println!("copy {i} ({}): render error: {e}", copy.name);
                all_ok = false;
            }
        }
    }

    if !all_ok {
        std::process::exit(1);
    }
}
