#[derive(Debug, Clone, Copy)]
pub struct CropArea {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl CropArea {
    pub fn default_720p() -> Self {
        Self {
            x: 520,
            y: 90,
            width: 240,
            height: 60,
        }
    }

    pub fn clamp(&mut self, max_w: usize, max_h: usize) {
        self.width = self.width.clamp(20, max_w);
        self.height = self.height.clamp(20, max_h);
        self.x = self.x.clamp(0, max_w.saturating_sub(self.width));
        self.y = self.y.clamp(0, max_h.saturating_sub(self.height));
    }
}
