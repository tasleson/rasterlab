/// Threads per workgroup for the shaders declaring `@workgroup_size(16, 16)`,
/// which is all of them but the two non-local-means kernels.
pub(crate) const WORKGROUP_SIZE: [u32; 2] = [16, 16];

/// The non-local-means kernels declare `@workgroup_size(8, 8)` instead.
///
/// A dispatch sized from the wrong constant silently leaves part of the image
/// unprocessed rather than failing, so these have to track the
/// `@workgroup_size` written in `shaders.rs`.
pub(crate) const NLM_WORKGROUP_SIZE: [u32; 2] = [8, 8];

pub(crate) fn expected_rgba_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}
