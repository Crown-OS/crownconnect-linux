use crate::media::video::Nv12Image;

/// Packs both planes of `image` into `out` without row padding.
pub fn pack_nv12(image: &Nv12Image<'_>, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(image.packed_len());
    image
        .luma_rows()
        .chain(image.chroma_rows())
        .for_each(|row| out.extend_from_slice(row));
}

/// Converts `image` to packed 4:2:2 YUYV, repeating each chroma row for two luma rows.
pub fn nv12_to_yuyv(image: &Nv12Image<'_>, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(image.width as usize * image.height as usize * 2);
    let chroma_rows = image.chroma_rows().flat_map(|row| [row, row]);
    for (luma_row, chroma_row) in image.luma_rows().zip(chroma_rows) {
        let (luma_pairs, _) = luma_row.as_chunks::<2>();
        let (chroma_pairs, _) = chroma_row.as_chunks::<2>();
        for ([y0, y1], [u, v]) in luma_pairs.iter().zip(chroma_pairs) {
            out.extend_from_slice(&[*y0, *u, *y1, *v]);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "a test that slices out of range should fail loudly"
)]
mod tests {
    use super::*;

    const LUMA: [u8; 12] = [1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0];
    const CHROMA: [u8; 6] = [10, 20, 30, 40, 0, 0];

    fn padded_image() -> Nv12Image<'static> {
        Nv12Image {
            width: 4,
            height: 2,
            luma: &LUMA,
            luma_stride: 6,
            chroma: &CHROMA,
            chroma_stride: 6,
        }
    }

    #[test]
    fn packs_away_row_padding() {
        let mut packed = Vec::new();
        pack_nv12(&padded_image(), &mut packed);
        assert_eq!(packed, [1, 2, 3, 4, 5, 6, 7, 8, 10, 20, 30, 40]);
    }

    #[test]
    fn converts_to_yuyv_sharing_chroma_between_rows() {
        let mut yuyv = Vec::new();
        nv12_to_yuyv(&padded_image(), &mut yuyv);
        assert_eq!(
            yuyv,
            [1, 10, 2, 20, 3, 30, 4, 40, 5, 10, 6, 20, 7, 30, 8, 40]
        );
    }

    #[test]
    fn rejects_short_planes() {
        let image = Nv12Image {
            luma: &LUMA[..8],
            ..padded_image()
        };
        assert!(image.validate().is_err());
        assert!(padded_image().validate().is_ok());
    }
}
