use crate::media::MediaError;

/// A borrowed 8-bit 4:2:0 image with interleaved chroma, as decoded video is laid out in memory.
#[derive(Debug, Clone, Copy)]
pub struct Nv12Image<'a> {
    pub width: u32,
    pub height: u32,
    pub luma: &'a [u8],
    pub luma_stride: usize,
    pub chroma: &'a [u8],
    pub chroma_stride: usize,
}

impl<'a> Nv12Image<'a> {
    /// Checks that both planes are large enough for the declared size and strides.
    pub fn validate(&self) -> Result<(), MediaError> {
        let width = self.width as usize;
        let height = self.height as usize;
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(MediaError::InvalidFrame("NV12 needs a non-zero, even size"));
        }
        let fits = |plane: &[u8], stride: usize, rows: usize| {
            stride >= width && plane.len() >= stride * (rows - 1) + width
        };
        if !fits(self.luma, self.luma_stride, height)
            || !fits(self.chroma, self.chroma_stride, height / 2)
        {
            return Err(MediaError::InvalidFrame(
                "NV12 plane is smaller than its size",
            ));
        }
        Ok(())
    }

    pub fn luma_rows(&self) -> impl Iterator<Item = &'a [u8]> {
        rows(self.luma, self.luma_stride, self.width, self.height)
    }

    pub fn chroma_rows(&self) -> impl Iterator<Item = &'a [u8]> {
        rows(self.chroma, self.chroma_stride, self.width, self.height / 2)
    }

    /// The size of the image with both planes packed without padding.
    pub const fn packed_len(&self) -> usize {
        self.width as usize * self.height as usize * 3 / 2
    }
}

fn rows(plane: &[u8], stride: usize, width: u32, height: u32) -> impl Iterator<Item = &[u8]> {
    plane
        .chunks(stride.max(1))
        .take(height as usize)
        .filter_map(move |row| row.get(..width as usize))
}
