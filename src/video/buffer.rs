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
}
