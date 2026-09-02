/// Groups a pixel buffer together with its dimensions.
/// Used so that buffers can be passed to drawing functions without spreading out the arguments.
pub struct PixelBuffer<'a> {
    pub pixels: &'a mut [u32],
    pub width: usize,
    pub height: usize,
}

impl<'a> PixelBuffer<'a> {
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Self {
        Self {
            pixels,
            width,
            height,
        }
    }

    /// Returns a mutable reference to the pixel data.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        self.pixels
    }

    /// Fills the rectangle starting at (x, y) with the specified color (clamped at buffer edges).
    pub fn clear_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: u32) {
        for row in 0..height {
            let py = y + row;
            if py >= self.height {
                break;
            }
            for col in 0..width {
                let px = x + col;
                if px >= self.width {
                    break;
                }
                self.pixels[py * self.width + px] = color;
            }
        }
    }
}
