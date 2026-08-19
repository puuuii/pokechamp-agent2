// src/main.rs
use anyhow::Result;
use crossbeam_channel::{Receiver, bounded};
use minifb::{Window, WindowOptions};
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{
        CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
    },
};
use rayon::prelude::*;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

// ==========================================
// 1. Core / Traits
// ==========================================

/// ピクセルデータ（0x00RRGGBB）を格納するフレームバッファ型。
/// Arcで包むことでdisplay系統とML推論系統の両方へ実体コピーなしでfan-outできる。
pub type FrameBuffer = Arc<Vec<u32>>;

/// 映像キャプチャデバイスのインターフェース
pub trait VideoSource {
    fn capture_frame(&mut self) -> Result<FrameBuffer>;
}

/// 音声パススルーデバイスのインターフェース
pub trait AudioPipeline: Send + 'static {
    fn start(self) -> Result<()>;
}

// ==========================================
// 2. Video Capture Component
// ==========================================

pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub camera_index: u32,
    pub frame_format: FrameFormat,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            camera_index: 0,
            frame_format: FrameFormat::YUYV,
        }
    }
}

pub struct NokhwaCapture {
    camera: Camera,
    width: usize,
    height: usize,
}

impl NokhwaCapture {
    pub fn new(config: &VideoConfig) -> Result<Self> {
        let format = CameraFormat::new(
            Resolution::new(config.width, config.height),
            config.frame_format,
            config.fps,
        );

        // キャプチャカードは対応解像度/fpsの組み合わせがピンポイントなことが多く、
        // Exact固定だと弾かれるケースがあるためClosestへフォールバックする。
        let requested_exact = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Exact(format));

        let mut camera = match Camera::new(CameraIndex::Index(config.camera_index), requested_exact)
        {
            Ok(cam) => cam,
            Err(e) => {
                eprintln!(
                    "Exact format request failed ({e}). Falling back to closest available format."
                );
                let requested_closest =
                    RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(format));
                Camera::new(CameraIndex::Index(config.camera_index), requested_closest)?
            }
        };

        camera.open_stream()?;
        let actual = camera.camera_format();
        println!("Camera opened. Actual format: {actual:?}");

        // 以降の capture_frame は YUYV 前提で直接デコードするため、
        // フォールバック等で実際のフォーマットがYUYV以外になっていないか起動時に確認する。
        // （MJPEG等に化けた場合、ここで気付かずに描画が壊れるのを防ぐ）
        anyhow::ensure!(
            actual.format() == FrameFormat::YUYV,
            "Direct YUYV decode requires FrameFormat::YUYV, but got {:?}. \
             Either force the device into YUYV or restore decode_image::<RgbFormat>().",
            actual.format()
        );
        anyhow::ensure!(
            actual.resolution().width() % 2 == 0,
            "YUYV requires an even width (2 bytes = 1 pixel pair), got {}",
            actual.resolution().width()
        );

        Ok(Self {
            camera,
            width: actual.resolution().width() as usize,
            height: actual.resolution().height() as usize,
        })
    }
}

/// BT.601相当のYUV→RGB変換（各成分0-255にクランプ）
#[inline(always)]
fn yuv_to_packed_u32(y: i32, u: i32, v: i32) -> u32 {
    let c = y - 16;
    let r = (298 * c + 409 * v + 128) >> 8;
    let g = (298 * c - 100 * u - 208 * v + 128) >> 8;
    let b = (298 * c + 516 * u + 128) >> 8;
    let r = r.clamp(0, 255) as u32;
    let g = g.clamp(0, 255) as u32;
    let b = b.clamp(0, 255) as u32;
    (r << 16) | (g << 8) | b
}

impl VideoSource for NokhwaCapture {
    fn capture_frame(&mut self) -> Result<FrameBuffer> {
        let frame = self.camera.frame()?;
        // decode_image::<RgbFormat>() を経由せず、YUYVの生バイト列を直接u32へ変換する。
        // (YUYV→RGB→u32 の2パスを、YUYV→u32 の1パスにまとめてメモリトラフィックを半減させる)
        let yuyv = frame.buffer();

        let width = self.width;
        let height = self.height;
        let pixel_count = width * height;

        // ゼロ初期化をスキップ（お手軽版）: 以下のループで全要素を必ず1回ずつ書き込むため、
        // vec![0u32; n] のゼロクリアコストを払う必要がない。
        let mut out_buf: Vec<u32> = Vec::with_capacity(pixel_count);
        // SAFETY: 直後の par_chunks_exact_mut(width) が幅方向・高さ方向を漏れなく走査し、
        // 各要素をちょうど1回書き込んでから関数を返す。書き込み前に読み出す経路は存在しない。
        unsafe {
            out_buf.set_len(pixel_count);
        }

        out_buf
            .par_chunks_exact_mut(width)
            .zip(yuyv.par_chunks_exact(width * 2))
            .for_each(|(out_row, in_row)| {
                for (out_pair, in_quad) in out_row.chunks_exact_mut(2).zip(in_row.chunks_exact(4)) {
                    let y0 = in_quad[0] as i32;
                    let u = in_quad[1] as i32 - 128;
                    let y1 = in_quad[2] as i32;
                    let v = in_quad[3] as i32 - 128;
                    out_pair[0] = yuv_to_packed_u32(y0, u, v);
                    out_pair[1] = yuv_to_packed_u32(y1, u, v);
                }
            });

        let _ = height; // heightはchunks_exactの境界チェック用途で暗黙的に使用済み
        Ok(Arc::new(out_buf))
    }
}

/// キャプチャループを実行・非同期管理するサービス。
/// 1本のキャプチャから display 用と ML 推論用の2系統にフレームを配る。
pub struct CaptureService {
    config: VideoConfig,
    /// 何フレームに1回MLチャネルへ回すか（例: 30なら60fps環境で約2fps相当）
    ml_sample_interval: u32,
}

impl CaptureService {
    pub fn new(config: VideoConfig, ml_sample_interval: u32) -> Self {
        Self {
            config,
            ml_sample_interval: ml_sample_interval.max(1),
        }
    }

    /// 戻り値: (表示用Receiver, ML推論用Receiver)
    /// FrameBufferはArc<Vec<u32>>なのでcloneしても中身のコピーは発生しない。
    pub fn spawn_loop(self) -> Result<(Receiver<FrameBuffer>, Receiver<FrameBuffer>)> {
        let (tx_display, rx_display) = bounded::<FrameBuffer>(2);
        let (tx_ml, rx_ml) = bounded::<FrameBuffer>(1);

        // キャプチャスレッド内でバックプレッシャー時に古いフレームを捨てるために
        // Receiver側の複製ハンドルを持っておく（crossbeamのReceiverはMPMCなのでOK）
        let rx_display_for_capture = rx_display.clone();
        let rx_ml_for_capture = rx_ml.clone();

        let ml_sample_interval = self.ml_sample_interval;

        thread::spawn(move || {
            let mut source = match NokhwaCapture::new(&self.config) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to initialize camera: {e}");
                    return;
                }
            };

            let mut frame_count = 0u32;
            let mut ml_tick = 0u32;
            let mut last_report = Instant::now();

            loop {
                let buf = match source.capture_frame() {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("Capture frame error: {e}");
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                };

                // --- 表示系統: 詰まっていたら最古のフレームを捨てて最新を優先 ---
                if let Err(crossbeam_channel::TrySendError::Full(latest)) =
                    tx_display.try_send(Arc::clone(&buf))
                {
                    let _ = rx_display_for_capture.try_recv();
                    let _ = tx_display.try_send(latest);
                }

                // --- ML系統: 間引いてサンプリングし、推論が重くても表示側に影響させない ---
                ml_tick += 1;
                if ml_tick >= ml_sample_interval {
                    ml_tick = 0;
                    if let Err(crossbeam_channel::TrySendError::Full(latest)) = tx_ml.try_send(buf)
                    {
                        let _ = rx_ml_for_capture.try_recv();
                        let _ = tx_ml.try_send(latest);
                    }
                }

                frame_count += 1;
                if last_report.elapsed().as_secs() >= 1 {
                    println!("Capture FPS: {frame_count}");
                    frame_count = 0;
                    last_report = Instant::now();
                }
            }
        });

        Ok((rx_display, rx_ml))
    }
}

// ==========================================
// 3. Display Window Component
// ==========================================

pub struct DisplayWindow {
    window: Window,
    width: usize,
    height: usize,
    current_frame: FrameBuffer,
}

impl DisplayWindow {
    pub fn new(title: &str, width: usize, height: usize) -> Result<Self> {
        let mut window = Window::new(title, width, height, WindowOptions::default())?;
        window.set_target_fps(0); // 手動フレーム制御

        Ok(Self {
            window,
            width,
            height,
            current_frame: Arc::new(vec![0u32; width * height]),
        })
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open()
    }

    /// 受信チャネルから最新フレームを取り出し、画面を更新する
    pub fn render_latest(&mut self, rx_frame: &Receiver<FrameBuffer>) -> Result<()> {
        let mut updated = false;

        // 最新のフレームまでキューを消化（Arcのcloneなので実体コピーはしない）
        while let Ok(frame) = rx_frame.try_recv() {
            self.current_frame = frame;
            updated = true;
        }

        if updated {
            self.window
                .update_with_buffer(&self.current_frame, self.width, self.height)?;
        } else {
            self.window.update();
            thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }
}

// ==========================================
// 4. Audio Component
// ==========================================

pub mod audio {
    use super::AudioPipeline;
    use anyhow::{Context, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use ringbuf::{
        HeapRb,
        traits::{Consumer, Producer, Split},
    };
    use std::thread;

    pub struct CpalAudioPassthrough {
        device_keyword: String,
    }

    impl CpalAudioPassthrough {
        pub fn new(device_keyword: &str) -> Self {
            Self {
                device_keyword: device_keyword.to_lowercase(),
            }
        }
    }

    impl AudioPipeline for CpalAudioPassthrough {
        fn start(self) -> Result<()> {
            let host = cpal::default_host();

            let input_device = host
                .input_devices()?
                .find(|d| {
                    d.name()
                        .map(|n| n.to_lowercase().contains(&self.device_keyword))
                        .unwrap_or(false)
                })
                .context("キャプチャ音声デバイスが見つかりません")?;

            let output_device = host.default_output_device().context("出力デバイスなし")?;

            let input_config = input_device.default_input_config()?;
            let output_config = output_device.default_output_config()?;

            let in_channels = input_config.channels() as usize;
            let in_rate = input_config.sample_rate().0;
            let out_channels = output_config.channels() as usize;
            let out_rate = output_config.sample_rate().0;

            println!("Audio Input: {in_channels} ch, {in_rate} Hz");
            println!("Audio Output: {out_channels} ch, {out_rate} Hz");

            let ring = HeapRb::<f32>::new(out_rate as usize * out_channels / 20);
            let (mut producer, mut consumer) = ring.split();

            let ratio = out_rate as f64 / in_rate as f64;
            let mut last_mono_sample: f32 = 0.0;

            let input_stream = input_device.build_input_stream(
                &input_config.into(),
                move |data: &[f32], _| {
                    let in_frames: Vec<f32> = data
                        .chunks_exact(in_channels)
                        .map(|c| c.iter().sum::<f32>() / in_channels as f32)
                        .collect();
                    let n_in = in_frames.len();
                    if n_in == 0 {
                        return;
                    }

                    let n_out = ((n_in as f64) * ratio).round() as usize;
                    for i in 0..n_out {
                        let src_pos = i as f64 / ratio;
                        let idx = src_pos.floor() as isize;
                        let frac = (src_pos - src_pos.floor()) as f32;

                        let s0 = if idx < 0 {
                            last_mono_sample
                        } else {
                            in_frames[idx as usize]
                        };
                        let s1 = if (idx + 1) as usize >= n_in {
                            *in_frames.last().unwrap()
                        } else {
                            in_frames[(idx + 1) as usize]
                        };
                        let sample = s0 + (s1 - s0) * frac;

                        for _ in 0..out_channels {
                            let _ = producer.try_push(sample);
                        }
                    }
                    last_mono_sample = *in_frames.last().unwrap();
                },
                move |err| eprintln!("Input stream error: {err}"),
                None,
            )?;

            let output_stream = output_device.build_output_stream(
                &output_config.into(),
                move |data: &mut [f32], _| {
                    for sample in data.iter_mut() {
                        *sample = consumer.try_pop().unwrap_or(0.0);
                    }
                },
                move |err| eprintln!("Output stream error: {err}"),
                None,
            )?;

            input_stream.play()?;
            output_stream.play()?;

            // ストリームの生存期間を維持するためにブロック
            loop {
                thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
    }
}

// ==========================================
// 5. ML Inference Component（プレースホルダー）
// ==========================================
//
// 現状は未実装。将来ここに `ort` クレート（champions-agentで使ったのと同系統）で
// ONNXモデルをロードし、rx_mlから受け取ったフレームに対して推論を行う想定。
// フレームはCaptureService側で間引かれて送られてくるため、
// ここでの処理がどれだけ重くても表示ループのFPSには影響しない。
pub mod inference {
    use super::FrameBuffer;
    use crossbeam_channel::Receiver;
    use std::thread;

    pub struct InferenceConfig {
        pub target_width: u32,
        pub target_height: u32,
    }

    pub struct InferenceWorker;

    impl InferenceWorker {
        /// 別スレッドで推論ループを起動する。
        /// 今はフレームを受け取って捨てるだけのスタブ。
        pub fn spawn(rx_ml: Receiver<FrameBuffer>, _config: InferenceConfig) {
            thread::spawn(move || {
                for frame in rx_ml.iter() {
                    // TODO: frame (0xRRGGBB の u32配列) を target_width x target_height に
                    //       リサイズしてモデル入力テンソルへ変換する
                    // TODO: ort::Session::run(...) で推論を実行する
                    // TODO: 推論結果をオーバーレイ描画用の共有状態やチャネルへ反映する
                    let _ = frame.len(); // 現状は未使用（プレースホルダー）
                }
            });
        }
    }
}

// ==========================================
// 6. Application Entry Point
// ==========================================

fn main() -> Result<()> {
    let video_config = VideoConfig::default();
    let width = video_config.width as usize;
    let height = video_config.height as usize;

    // 60fps想定でNフレームに1回だけMLチャネルへ回す（30 = 約2fps相当）
    const ML_SAMPLE_INTERVAL: u32 = 30;

    // 1. キャプチャサービスのセットアップと起動（display用・ML用の2系統を受け取る）
    let capture_service = CaptureService::new(video_config, ML_SAMPLE_INTERVAL);
    let (rx_display, rx_ml) = capture_service.spawn_loop()?;

    // 2. 音声処理の起動
    thread::spawn(|| {
        let audio_pipeline = audio::CpalAudioPassthrough::new("gc311");
        if let Err(e) = audio_pipeline.start() {
            eprintln!("Audio error: {e}");
        }
    });

    // 3. 推論ワーカーの起動（現状はスタブ。将来ここにONNX推論を実装していく）
    inference::InferenceWorker::spawn(
        rx_ml,
        inference::InferenceConfig {
            target_width: 224,
            target_height: 224,
        },
    );

    // 4. 表示ウィンドウの初期化
    let mut window = DisplayWindow::new("Switch Capture", width, height)?;

    // 5. メインループ（描画＆UIイベント）
    while window.is_open() {
        window.render_latest(&rx_display)?;
    }

    Ok(())
}
