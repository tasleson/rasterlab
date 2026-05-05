pub(crate) const WORKGROUP_SIZE_X: u32 = 16;
pub(crate) const WORKGROUP_SIZE_Y: u32 = 16;

pub(crate) fn expected_rgba_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}
