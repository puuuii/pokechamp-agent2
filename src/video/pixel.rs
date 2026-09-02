/// ピクセルパック形式はRRGGBB(32bit、上位バイト=赤)。
/// 形式前提が変わるときはこのモジュールだけ直せばよく、
/// `colorspace`・`jp_text`・`inference::preprocess` から参照される。

/// 32bitパックピクセルを (R, G, B) に分解する。
#[inline(always)]
pub fn unpack_rgb(packed: u32) -> (u8, u8, u8) {
    (
        ((packed >> 16) & 0xFF) as u8,
        ((packed >> 8) & 0xFF) as u8,
        (packed & 0xFF) as u8,
    )
}

/// (R, G, B) を32bitパックピクセルに組む。
#[inline(always)]
pub fn pack_rgb(red: u8, green: u8, blue: u8) -> u32 {
    (red as u32) << 16 | (green as u32) << 8 | blue as u32
}
