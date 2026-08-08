//! Source-frame loading shared by the multi-image ops.
//!
//! Focus Stack, HDR Merge, and Panorama all store absolute source paths and
//! reload them on every `apply()` so the op stays self-contained for
//! non-destructive replay.  They load those frames identically, so the loop
//! lives here.

use crate::{
    cancel,
    error::{RasterError, RasterResult},
    formats::FormatRegistry,
    image::Image,
};

/// Load every frame named by `paths`, in order.
///
/// A path may be an ordinary image file or a `.rlab` container — a managed
/// library photo — which is decoded from its embedded original.  `label` names
/// the calling op so a failure reads `Focus Stack: cannot load '…'`.
pub(crate) fn load_frames(paths: &[String], label: &str) -> RasterResult<Vec<Image>> {
    let reg = FormatRegistry::with_builtins();
    paths
        .iter()
        .map(|p| {
            if cancel::is_requested() {
                return Err(RasterError::Cancelled);
            }
            reg.decode_source_file(std::path::Path::new(p))
                .map_err(|e| RasterError::InvalidParams(format!("{label}: cannot load '{p}': {e}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        pipeline::PipelineState,
        project::{RlabFile, RlabMeta, SavedCopy},
        traits::format_handler::EncodeOptions,
    };

    /// A small image with a recognisable pixel so decoded frames can be
    /// distinguished from a default-constructed one.
    fn sample_image() -> Image {
        let mut img = Image::new(4, 3);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            px[0] = (i * 20) as u8;
            px[1] = 40;
            px[2] = 60;
            px[3] = 255;
        }
        img
    }

    fn write_png(dir: &std::path::Path, name: &str, image: &Image) -> String {
        let reg = FormatRegistry::with_builtins();
        let path = dir.join(name);
        let bytes = reg
            .encode_file(image, &path, &EncodeOptions::default())
            .unwrap();
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// Wrap `png_bytes` in a `.rlab` container the way the library importer
    /// does, so the loader has a real ORIG chunk to unwrap.
    fn write_rlab(dir: &std::path::Path, name: &str, source_name: &str, image: &Image) -> String {
        let reg = FormatRegistry::with_builtins();
        let source = dir.join(source_name);
        let original_bytes = reg
            .encode_file(image, &source, &EncodeOptions::default())
            .unwrap();
        let path = dir.join(name);
        let meta = RlabMeta::new(
            "test",
            Some(source.to_string_lossy().as_ref()),
            image.width,
            image.height,
        );
        let copies = vec![SavedCopy {
            name: "Copy 1".into(),
            pipeline_state: PipelineState {
                entries: Vec::new(),
                cursor: 0,
            },
        }];
        RlabFile::new(meta, original_bytes, copies, 0, None)
            .write_v5(&path)
            .unwrap();
        path.to_string_lossy().into_owned()
    }

    /// Library photos are `.rlab` containers, so a stack started from the
    /// library grid hands these ops paths no image decoder understands. They
    /// must load as the embedded original, mixed freely with plain files.
    #[test]
    fn loads_plain_images_and_rlab_containers() {
        let dir = tempfile::tempdir().unwrap();
        let image = sample_image();
        let paths = vec![
            write_png(dir.path(), "frame.png", &image),
            write_rlab(dir.path(), "photo.rlab", "photo.png", &image),
        ];

        let frames = load_frames(&paths, "Focus Stack").unwrap();

        assert_eq!(frames.len(), 2);
        for frame in &frames {
            assert_eq!((frame.width, frame.height), (image.width, image.height));
            assert_eq!(frame.data, image.data);
        }
    }

    #[test]
    fn missing_frame_reports_the_op_and_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.png").to_string_lossy().into_owned();

        let err = load_frames(std::slice::from_ref(&missing), "Focus Stack").unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("Focus Stack"), "{msg}");
        assert!(msg.contains(&missing), "{msg}");
    }
}
