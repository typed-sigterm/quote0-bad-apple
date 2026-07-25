//! 152x296 8-bit grayscale -> 1bpp frame packing with Floyd-Steinberg dithering.
//!
//! Packed format matches quote0-desktop-status and the UC8251D RAM layout:
//! row-major, 8 pixels per byte, MSB first, 1 = white.

pub const WIDTH: usize = 152;
pub const HEIGHT: usize = 296;
pub const GRAY_BYTES: usize = WIDTH * HEIGHT; // 44992
pub const FRAME_BYTES: usize = WIDTH * HEIGHT / 8; // 5616

/// Floyd-Steinberg dither a grayscale frame and pack it to 1bpp.
///
/// `gray` must be exactly `GRAY_BYTES` long (row-major, 0 = black, 255 = white).
/// `invert` flips the final black/white assignment (error diffusion still
/// happens on the original intensities).
pub fn dither_pack(gray: &[u8], invert: bool) -> [u8; FRAME_BYTES] {
    assert_eq!(
        gray.len(),
        GRAY_BYTES,
        "gray frame must be {GRAY_BYTES} bytes"
    );

    let mut buf = [0i16; GRAY_BYTES];
    for (b, &g) in buf.iter_mut().zip(gray) {
        *b = i16::from(g);
    }

    let mut out = [0u8; FRAME_BYTES];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let i = y * WIDTH + x;
            let old = buf[i].clamp(0, 255);
            let white = old >= 128;
            let new: i16 = if white { 255 } else { 0 };
            let err = old - new;

            if white != invert {
                out[i / 8] |= 1 << (7 - (i % 8));
            }

            // diffuse the quantization error to not-yet-visited neighbors
            if x + 1 < WIDTH {
                buf[i + 1] += err * 7 / 16;
            }
            if y + 1 < HEIGHT {
                if x > 0 {
                    buf[i + WIDTH - 1] += err * 3 / 16;
                }
                buf[i + WIDTH] += err * 5 / 16;
                if x + 1 < WIDTH {
                    buf[i + WIDTH + 1] += err / 16;
                }
            }
        }
    }
    out
}

/// Expand a packed frame back to 0/255 grayscale (tests only).
#[cfg(test)]
pub fn unpack_gray(frame: &[u8; FRAME_BYTES]) -> [u8; GRAY_BYTES] {
    let mut out = [0u8; GRAY_BYTES];
    for (i, px) in out.iter_mut().enumerate() {
        *px = if frame[i / 8] & (1 << (7 - (i % 8))) != 0 {
            255
        } else {
            0
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_white_packs_to_ff() {
        let gray = [255u8; GRAY_BYTES];
        let packed = dither_pack(&gray, false);
        assert!(packed.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn all_black_packs_to_00() {
        let gray = [0u8; GRAY_BYTES];
        let packed = dither_pack(&gray, false);
        assert!(packed.iter().all(|&b| b == 0x00));
    }

    #[test]
    fn invert_swaps_output() {
        let white = dither_pack(&[255u8; GRAY_BYTES], true);
        assert!(white.iter().all(|&b| b == 0x00));
        let black = dither_pack(&[0u8; GRAY_BYTES], true);
        assert!(black.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn bit_order_is_msb_first() {
        // First pixel black, the remaining seven of the first byte white.
        let mut gray = [255u8; GRAY_BYTES];
        gray[0] = 0;
        let packed = dither_pack(&gray, false);
        assert_eq!(packed[0], 0b0111_1111);
    }

    #[test]
    fn unpack_roundtrip_matches_reference_layout() {
        // Reference (quote0-desktop-status server): idx = y*W + x,
        // white -> out[idx/8] |= 1 << (7 - idx%8).
        let mut gray = [0u8; GRAY_BYTES];
        gray[WIDTH + 2] = 255; // single white pixel at (2,1)
        let packed = dither_pack(&gray, false);
        let idx = WIDTH + 2;
        assert_eq!(packed[idx / 8], 1 << (7 - (idx % 8)));
        let back = unpack_gray(&packed);
        assert_eq!(back[idx], 255);
        assert_eq!(back.iter().filter(|&&p| p == 255).count(), 1);
    }

    #[test]
    fn mid_gray_dithers_without_drift() {
        // Uniform 128 should produce a 50/50 checker-ish mix, not all black.
        let gray = [128u8; GRAY_BYTES];
        let packed = dither_pack(&gray, false);
        let ones: u64 = packed.iter().map(|b| b.count_ones() as u64).sum();
        let ratio = ones as f64 / (GRAY_BYTES as f64);
        assert!(
            ratio > 0.3 && ratio < 0.7,
            "ratio {ratio} out of expected range"
        );
    }
}
