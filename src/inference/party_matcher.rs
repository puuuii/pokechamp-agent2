// party_matcher.rs
use image::imageops::FilterType;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use tracing::{info, warn};

use crate::hardware::FrameBuffer;
use crate::video::{CropArea, PixelCropArea, unpack_rgb};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

static DEBUG_CROP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 埋め込みモデルの入力画像サイズ(正方形)。使用するONNXモデルに合わせて変更する。
/// timm/mobilenetv3_small_100.lamb_in1k は 224x224 前提。
const MODEL_INPUT_SIZE: u32 = 224;

/// 入力正規化パラメータ(ImageNet統計値)。timm系のImageNet学習済みモデルはこの値でよい。
/// CLIP系モデルに切り替える場合は以下に変更すること:
///   mean = [0.48145466, 0.4578275, 0.40821073]
///   std  = [0.26862954, 0.26130258, 0.27577711]
const NORMALIZE_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const NORMALIZE_STD: [f32; 3] = [0.229, 0.224, 0.225];

/// テンプレート画像の透過部分を潰すための背景色。
const BACKGROUND_COLOR: [u8; 3] = [128, 128, 128];

/// テンプレート側で「透過でない」と判定するアルファ値のしきい値(トリミング用)。
const ALPHA_OPAQUE_THRESHOLD: u8 = 128;

/// コサイン類似度(0.0〜1.0、1.0が完全一致)がこれ未満なら「該当なし」として扱う。
/// 実際のログを見ながら調整すること。
const SIMILARITY_THRESHOLD: f32 = 0.5;

/// 選出フェーズにおける、味方・相手それぞれ6体分のアイコン位置(相対座標)。
/// 通常は TOML ファイル(config/party_slots.toml)から読み込む。
#[derive(Debug, Clone, Deserialize)]
pub struct PartySlots {
    pub ally: [CropArea; 6],
    pub opponent: [CropArea; 6],
}

impl PartySlots {
    /// 味方1〜6、相手1〜6の順で12箇所のクロップ領域を返す。
    pub fn all_crops(&self) -> [CropArea; 12] {
        [
            self.ally[0],
            self.ally[1],
            self.ally[2],
            self.ally[3],
            self.ally[4],
            self.ally[5],
            self.opponent[0],
            self.opponent[1],
            self.opponent[2],
            self.opponent[3],
            self.opponent[4],
            self.opponent[5],
        ]
    }
}

impl Default for PartySlots {
    fn default() -> Self {
        Self {
            ally: [
                CropArea {
                    x: 0.2613,
                    y: 0.1450,
                    width: 0.0775,
                    height: 0.1008,
                },
                CropArea {
                    x: 0.2613,
                    y: 0.2625,
                    width: 0.0775,
                    height: 0.1008,
                },
                CropArea {
                    x: 0.2613,
                    y: 0.3775,
                    width: 0.0775,
                    height: 0.1008,
                },
                CropArea {
                    x: 0.2613,
                    y: 0.4950,
                    width: 0.0775,
                    height: 0.1008,
                },
                CropArea {
                    x: 0.2613,
                    y: 0.6125,
                    width: 0.0775,
                    height: 0.1008,
                },
                CropArea {
                    x: 0.2613,
                    y: 0.7300,
                    width: 0.0775,
                    height: 0.1008,
                },
            ],
            opponent: [
                CropArea {
                    x: 0.8400,
                    y: 0.1450,
                    width: 0.0775,
                    height: 0.1008,
                },
                CropArea {
                    x: 0.8400,
                    y: 0.2625,
                    width: 0.0775,
                    height: 0.1008,
                },
                CropArea {
                    x: 0.8400,
                    y: 0.3775,
                    width: 0.0775,
                    height: 0.1008,
                },
                CropArea {
                    x: 0.8400,
                    y: 0.4950,
                    width: 0.0775,
                    height: 0.1008,
                },
                CropArea {
                    x: 0.8400,
                    y: 0.6125,
                    width: 0.0775,
                    height: 0.1008,
                },
                CropArea {
                    x: 0.8400,
                    y: 0.7300,
                    width: 0.0775,
                    height: 0.1008,
                },
            ],
        }
    }
}

/// 事前計算済みのポケモンアイコンテンプレート(ファイル名 + 埋め込みベクトル)。
#[derive(Debug, Clone)]
struct IconTemplate {
    file_name: String,
    /// L2正規化済みの埋め込みベクトル(コサイン類似度=内積で比較できるようにしてある)。
    embedding: Vec<f32>,
}

/// キャッシュファイル名(img_dir 直下に生成)。
const CACHE_FILE_NAME: &str = ".embedding_cache.json";

#[derive(Debug, Serialize, Deserialize)]
struct CachedEmbedding {
    file_name: String,
    mtime_nanos: u128,
    embedding: Vec<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct EmbeddingCache {
    model_path: String,
    entries: Vec<CachedEmbedding>,
}

impl EmbeddingCache {
    /// 読み込み失敗、またはモデルが変わっていた場合は空キャッシュ扱い。
    fn load(path: &Path, model_path: &str) -> Self {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&contents) {
            Ok(cache) if cache.model_path == model_path => cache,
            _ => Self::default(),
        }
    }

    fn save(&self, path: &Path) {
        if let Ok(json) = serde_json::to_string(self) {
            if let Err(e) = std::fs::write(path, json) {
                warn!(
                    "埋め込みキャッシュの書き込みに失敗しました({}): {e}",
                    path.display()
                );
            }
        }
    }
}

fn file_mtime_nanos(path: &Path) -> anyhow::Result<u128> {
    let modified = std::fs::metadata(path)?.modified()?;
    Ok(modified.duration_since(UNIX_EPOCH)?.as_nanos())
}

/// `img/` 配下の全PNGを事前学習済み画像埋め込みモデルでベクトル化しておき、
/// 実行時はクロップ画像も同じモデルでベクトル化してコサイン類似度が最も高い
/// テンプレートを採用するマッチャー。
///
/// 色距離やハッシュと違い、「見た目の意味的な近さ」を学習済みモデルの特徴表現で
/// 比較するため、背景色や多少の位置ズレ・配色の近さに対して頑健。
pub struct PartyIconMatcher {
    /// モデル読み込みに失敗した場合は `None`(マッチは常に該当なしになる)。
    session: Option<Mutex<Session>>,
    templates: Vec<IconTemplate>,
}

impl PartyIconMatcher {
    /// `model_path` のONNXモデルを読み込み、`img_dir` 配下の全PNGを埋め込みベクトル化する。
    pub fn load_from_dir(model_path: &Path, img_dir: &Path) -> anyhow::Result<Self> {
        let mut session = Session::builder()?
            .commit_from_file(model_path)
            .map_err(|e| {
                anyhow::anyhow!(
                    "埋め込みモデルの読み込みに失敗しました({}): {e}",
                    model_path.display()
                )
            })?;

        let model_path_str = model_path.to_string_lossy().to_string();
        let cache_path = img_dir.join(CACHE_FILE_NAME);
        let old_cache = EmbeddingCache::load(&cache_path, &model_path_str);
        let old_by_name: HashMap<&str, &CachedEmbedding> = old_cache
            .entries
            .iter()
            .map(|e| (e.file_name.as_str(), e))
            .collect();

        let mut templates = Vec::new();
        let mut new_entries = Vec::new();
        let mut cache_hit = 0usize;

        let entries = std::fs::read_dir(img_dir).map_err(|e| {
            anyhow::anyhow!(
                "画像ディレクトリを読み込めません({}): {e}",
                img_dir.display()
            )
        })?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let is_png = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("png"))
                .unwrap_or(false);
            if !is_png {
                continue;
            }

            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let mtime_nanos = match file_mtime_nanos(&path) {
                Ok(m) => m,
                Err(e) => {
                    warn!("mtime取得に失敗しました({}): {e}", path.display());
                    continue;
                }
            };

            // キャッシュにヒットすれば推論をスキップする。
            if let Some(cached) = old_by_name.get(file_name.as_str()) {
                if cached.mtime_nanos == mtime_nanos {
                    templates.push(IconTemplate {
                        file_name: file_name.clone(),
                        embedding: cached.embedding.clone(),
                    });
                    new_entries.push(CachedEmbedding {
                        file_name,
                        mtime_nanos,
                        embedding: cached.embedding.clone(),
                    });
                    cache_hit += 1;
                    continue;
                }
            }

            let img = match image::open(&path) {
                Ok(img) => img,
                Err(e) => {
                    warn!("画像を読み込めませんでした({}): {e}", path.display());
                    continue;
                }
            };

            let trimmed = trim_transparent_margin(&img);
            let rgb_image = composite_on_background(&trimmed, BACKGROUND_COLOR);

            match embed_rgb_image(&mut session, &rgb_image) {
                Ok(embedding) => {
                    templates.push(IconTemplate {
                        file_name: file_name.clone(),
                        embedding: embedding.clone(),
                    });
                    new_entries.push(CachedEmbedding {
                        file_name,
                        mtime_nanos,
                        embedding,
                    });
                }
                Err(e) => warn!("埋め込み計算に失敗しました({}): {e}", path.display()),
            }
        }

        info!(
            "ポケモンアイコンテンプレートを{}件読み込みました(キャッシュヒット{}件)",
            templates.len(),
            cache_hit
        );
        anyhow::ensure!(
            !templates.is_empty(),
            "img/ にPNGが見つかりませんでした: {}",
            img_dir.display()
        );

        EmbeddingCache {
            model_path: model_path_str,
            entries: new_entries,
        }
        .save(&cache_path);

        Ok(Self {
            session: Some(Mutex::new(session)),
            templates,
        })
    }

    /// モデルもテンプレートも持たない空のマッチャー(読み込み失敗時のフォールバック用)。
    /// マッチは常に該当なし(None)になる。
    pub fn empty() -> Self {
        Self {
            session: None,
            templates: Vec::new(),
        }
    }

    /// 1つのクロップ領域(相対座標)をフレームから切り出し、
    /// コサイン類似度が最も高いテンプレートのファイル名を返す(閾値未満なら None)。
    pub fn match_crop(
        &self,
        frame: &FrameBuffer,
        frame_width: usize,
        frame_height: usize,
        crop: CropArea,
    ) -> Option<String> {
        let session_mutex = self.session.as_ref()?;

        let pixel_crop = crop.to_pixels(frame_width, frame_height);
        let rgb_image = extract_crop_as_rgb_image(frame, frame_width, frame_height, pixel_crop)?;

        #[cfg(debug_assertions)]
        {
            let n = DEBUG_CROP_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
            let dir = Path::new("debug_crops");
            if std::fs::create_dir_all(dir).is_ok() {
                let path = dir.join(format!("crop_{n:05}.png"));
                if let Err(e) = rgb_image.save(&path) {
                    warn!(
                        "デバッグ用クロップ画像の保存に失敗しました({}): {e}",
                        path.display()
                    );
                }
            }
        }

        let embedding = {
            let mut session = session_mutex.lock().unwrap();
            match embed_rgb_image(&mut session, &rgb_image) {
                Ok(embedding) => embedding,
                Err(e) => {
                    warn!("クロップの埋め込み計算に失敗しました: {e}");
                    return None;
                }
            }
        };

        let best = self
            .templates
            .iter()
            .map(|template| (template, cosine_similarity(&embedding, &template.embedding)))
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        if let Some((template, similarity)) = &best {
            tracing::debug!(
                "party_match: best={} similarity={:.3} (threshold={})",
                template.file_name,
                similarity,
                SIMILARITY_THRESHOLD
            );
        }

        best.filter(|(_, similarity)| *similarity >= SIMILARITY_THRESHOLD)
            .map(|(template, _)| template.file_name.clone())
    }
}

/// RGB画像をモデル入力サイズにリサイズし、正規化してNCHW形式のテンソルにする。
fn build_input_tensor(rgb_image: &image::RgbImage) -> Array4<f32> {
    let resized = image::imageops::resize(
        rgb_image,
        MODEL_INPUT_SIZE,
        MODEL_INPUT_SIZE,
        FilterType::Triangle,
    );

    let size = MODEL_INPUT_SIZE as usize;
    let mut array = Array4::<f32>::zeros((1, 3, size, size));

    for (x, y, pixel) in resized.enumerate_pixels() {
        let [r, g, b] = pixel.0;
        array[[0, 0, y as usize, x as usize]] =
            (r as f32 / 255.0 - NORMALIZE_MEAN[0]) / NORMALIZE_STD[0];
        array[[0, 1, y as usize, x as usize]] =
            (g as f32 / 255.0 - NORMALIZE_MEAN[1]) / NORMALIZE_STD[1];
        array[[0, 2, y as usize, x as usize]] =
            (b as f32 / 255.0 - NORMALIZE_MEAN[2]) / NORMALIZE_STD[2];
    }

    array
}

/// RGB画像をモデルに通して、L2正規化済みの埋め込みベクトルを得る。
fn embed_rgb_image(session: &mut Session, rgb_image: &image::RgbImage) -> anyhow::Result<Vec<f32>> {
    let input_array = build_input_tensor(rgb_image);
    let input_value = Value::from_array(input_array)?;
    let outputs = session.run(ort::inputs![input_value])?;
    let (_, data) = outputs[0].try_extract_tensor::<f32>()?;

    let mut embedding: Vec<f32> = data.to_vec();
    normalize_l2(&mut embedding);
    Ok(embedding)
}

/// ベクトルをL2正規化する(コサイン類似度を単純な内積で計算できるようにするため)。
fn normalize_l2(vec: &mut [f32]) {
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for v in vec.iter_mut() {
            *v /= norm;
        }
    }
}

/// 両方ともL2正規化済みである前提でのコサイン類似度(=内積)。
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// アルファ値が閾値以上のピクセルのバウンディングボックスで画像をトリミングする。
/// 元PNGの透過マージン量が画像ごとに違っても、アイコン本体だけを対象にするため。
fn trim_transparent_margin(img: &image::DynamicImage) -> image::DynamicImage {
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    let mut found = false;

    for (x, y, pixel) in rgba.enumerate_pixels() {
        if pixel.0[3] >= ALPHA_OPAQUE_THRESHOLD {
            found = true;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }

    if !found {
        return img.clone();
    }

    let crop_width = (max_x - min_x + 1).max(1);
    let crop_height = (max_y - min_y + 1).max(1);

    img.crop_imm(min_x, min_y, crop_width, crop_height)
}

/// 透過PNGを指定した背景色に合成し、不透明なRGB画像にする。
fn composite_on_background(img: &image::DynamicImage, background: [u8; 3]) -> image::RgbImage {
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut out = image::RgbImage::new(width, height);

    for (x, y, pixel) in rgba.enumerate_pixels() {
        let [r, g, b, a] = pixel.0;
        let alpha = a as f32 / 255.0;
        let out_r = (r as f32 * alpha + background[0] as f32 * (1.0 - alpha)).round() as u8;
        let out_g = (g as f32 * alpha + background[1] as f32 * (1.0 - alpha)).round() as u8;
        let out_b = (b as f32 * alpha + background[2] as f32 * (1.0 - alpha)).round() as u8;
        out.put_pixel(x, y, image::Rgb([out_r, out_g, out_b]));
    }

    out
}

/// packed RGB `FrameBuffer` の指定クロップ範囲を、等倍のRGB画像として切り出す。
fn extract_crop_as_rgb_image(
    frame: &FrameBuffer,
    frame_width: usize,
    frame_height: usize,
    crop: PixelCropArea,
) -> Option<image::RgbImage> {
    if crop.width == 0 || crop.height == 0 {
        return None;
    }

    let mut img = image::RgbImage::new(crop.width as u32, crop.height as u32);

    for y in 0..crop.height {
        let src_y = (crop.y + y).min(frame_height.saturating_sub(1));
        for x in 0..crop.width {
            let src_x = (crop.x + x).min(frame_width.saturating_sub(1));
            let packed = frame[src_y * frame_width + src_x];
            let (r, g, b) = unpack_rgb(packed);
            img.put_pixel(x as u32, y as u32, image::Rgb([r, g, b]));
        }
    }

    Some(img)
}
