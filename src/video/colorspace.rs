use rayon::prelude::*;

use super::pixel::pack_rgb;

const LUMA_STUDIO_MIN_OFFSET: i32 = 16;
const CHROMA_CENTER_OFFSET: i32 = 128;

#[inline(always)]
pub fn yuv_to_packed_rgb(luma: i32, chroma_u: i32, chroma_v: i32) -> u32 {
    let normalized_luma = luma - LUMA_STUDIO_MIN_OFFSET;
    let normalized_u = chroma_u - CHROMA_CENTER_OFFSET;
    let normalized_v = chroma_v - CHROMA_CENTER_OFFSET;

    let red = ((298 * normalized_luma + 409 * normalized_v + 128) >> 8).clamp(0, 255) as u8;
    let green = ((298 * normalized_luma - 100 * normalized_u - 208 * normalized_v + 128) >> 8)
        .clamp(0, 255) as u8;
    let blue = ((298 * normalized_luma + 516 * normalized_u + 128) >> 8).clamp(0, 255) as u8;

    pack_rgb(red, green, blue)
}

pub fn decode_yuyv_to_packed_rgb_parallel(
    yuyv_raw_bytes: &[u8],
    output_pixels: &mut [u32],
    image_width: usize,
) {
    let bytes_per_yuyv_row = image_width * 2;

    output_pixels
        .par_chunks_exact_mut(image_width)
        .zip(yuyv_raw_bytes.par_chunks_exact(bytes_per_yuyv_row))
        .for_each(|(output_row, input_row_bytes)| {
            let output_pixel_pairs = output_row.chunks_exact_mut(2);
            let input_yuyv_quads = input_row_bytes.chunks_exact(4);

            for (pixel_pair, yuyv_quad) in output_pixel_pairs.zip(input_yuyv_quads) {
                let luma_0 = yuyv_quad[0] as i32;
                let chroma_u = yuyv_quad[1] as i32;
                let luma_1 = yuyv_quad[2] as i32;
                let chroma_v = yuyv_quad[3] as i32;

                pixel_pair[0] = yuv_to_packed_rgb(luma_0, chroma_u, chroma_v);
                pixel_pair[1] = yuv_to_packed_rgb(luma_1, chroma_u, chroma_v);
            }
        });
}
