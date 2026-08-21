const MIN_RELATIVE_SIZE: f32 = 0.02; // 全体に対する最小サイズ比率

/// 相対座標(0.0〜1.0)で表現するクロップ範囲。
/// (0.0, 0.0) が画面左上、(1.0, 1.0) が右下に対応する。
#[derive(Debug, Clone, Copy)]
pub struct CropArea {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl CropArea {
    /// 旧 default_720p() (x:520, y:90, w:240, h:60 @ 1280x720) を相対値に変換したもの
    pub fn default_relative() -> Self {
        Self {
            x: 520.0 / 1280.0,
            y: 90.0 / 720.0,
            width: 240.0 / 1280.0,
            height: 60.0 / 720.0,
        }
    }

    /// 0.0〜1.0 の範囲に収める(widthはゼロ以下にならないようにする)
    pub fn clamp(&mut self) {
        self.width = self.width.clamp(MIN_RELATIVE_SIZE, 1.0);
        self.height = self.height.clamp(MIN_RELATIVE_SIZE, 1.0);
        self.x = self.x.clamp(0.0, 1.0 - self.width);
        self.y = self.y.clamp(0.0, 1.0 - self.height);
    }

    /// 実フレームサイズ(ピクセル)に変換する
    pub fn to_pixels(self, frame_width: usize, frame_height: usize) -> PixelCropArea {
        PixelCropArea {
            x: (self.x * frame_width as f32).round() as usize,
            y: (self.y * frame_height as f32).round() as usize,
            width: ((self.width * frame_width as f32).round() as usize).max(1),
            height: ((self.height * frame_height as f32).round() as usize).max(1),
        }
    }
}

/// ピクセル座標に変換した後のクロップ範囲(描画・OCR前処理で使う)
#[derive(Debug, Clone, Copy)]
pub struct PixelCropArea {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}
