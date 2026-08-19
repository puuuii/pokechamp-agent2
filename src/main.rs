// src/main.rs
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};
use minifb::{Window, WindowOptions};
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{
        CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
    },
};
use rayon::prelude::*;
use std::thread;
use std::time::{Duration, Instant};

// ==========================================
// 1. Core / Traits
// ==========================================

/// ピクセルデータ（0x00RRGGBB）を格納するフレームバッファ型
pub type FrameBuffer = Vec<u32>;

/// 映像キャプチャデバイスのインターフェース
pub trait VideoSource {
    fn capture_frame(&mut self, out_buf: &mut FrameBuffer) -> Result<()>;
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
}

impl NokhwaCapture {
    pub fn new(config: &VideoConfig) -> Result<Self> {
        let index = CameraIndex::Index(config.camera_index);
        let format = CameraFormat::new(
            Resolution::new(config.width, config.height),
            config.frame_format,
            config.fps,
        );
        let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Exact(format));

        let mut camera = Camera::new(index, requested)?;
        camera.open_stream()?;
        println!("Camera opened. Actual format: {:?}", camera.camera_format());

        Ok(Self { camera })
    }
}

impl VideoSource for NokhwaCapture {
    fn capture_frame(&mut self, out_buf: &mut FrameBuffer) -> Result<()> {
        let frame = self.camera.frame()?;
        let decoded = frame.decode_image::<RgbFormat>()?;
        let raw = decoded.as_raw();

        // 確保済みバッファに書き込み（再利用）
        out_buf.resize((decoded.width() * decoded.height()) as usize, 0);
        out_buf
            .par_chunks_exact_mut(1)
            .zip(raw.par_chunks_exact(3))
            .for_each(|(out, px)| {
                out[0] = ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | (px[2] as u32);
            });

        Ok(())
    }
}

/// キャプチャループを実行・非同期管理するサービス
pub struct CaptureService {
    config: VideoConfig,
}

impl CaptureService {
    pub fn new(config: VideoConfig) -> Self {
        Self { config }
    }

    pub fn spawn_loop(self) -> Result<(Receiver<FrameBuffer>, Sender<FrameBuffer>)> {
        let (tx_frame, rx_frame) = bounded::<FrameBuffer>(2);
        let (tx_pool, rx_pool) = bounded::<FrameBuffer>(3);

        let buf_size = (self.config.width * self.config.height) as usize;
        for _ in 0..3 {
            let _ = tx_pool.send(vec![0u32; buf_size]);
        }

        // スレッド起動 (Cameraの生成・所有権管理はスレッド内部で行う)
        thread::spawn(move || {
            // スレッド内部で NokhwaCapture を初期化 (Send境界を回避)
            let mut source = match NokhwaCapture::new(&self.config) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to initialize camera: {e}");
                    return;
                }
            };

            let mut frame_count = 0u32;
            let mut last_report = Instant::now();

            loop {
                let mut buf = rx_pool.try_recv().unwrap_or_else(|_| vec![0u32; buf_size]);

                if let Err(e) = source.capture_frame(&mut buf) {
                    eprintln!("Capture frame error: {e}");
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }

                if let Err(crossbeam_channel::TrySendError::Full(returned_buf)) =
                    tx_frame.try_send(buf)
                {
                    let _ = rx_pool.try_recv();
                    drop(returned_buf);
                }

                frame_count += 1;
                if last_report.elapsed().as_secs() >= 1 {
                    println!("Capture FPS: {frame_count}");
                    frame_count = 0;
                    last_report = Instant::now();
                }
            }
        });

        Ok((rx_frame, tx_pool))
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
            current_frame: vec![0u32; width * height],
        })
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open()
    }

    /// 受信チャネルから最新フレームを取り出し、画面を更新する
    pub fn render_latest(
        &mut self,
        rx_frame: &Receiver<FrameBuffer>,
        tx_pool: &Sender<FrameBuffer>,
    ) -> Result<()> {
        let mut updated = false;

        // 最新のフレームまでキューを消化
        while let Ok(frame) = rx_frame.try_recv() {
            let old_frame = std::mem::replace(&mut self.current_frame, frame);
            let _ = tx_pool.try_send(old_frame);
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
// 5. Application Entry Point
// ==========================================

fn main() -> Result<()> {
    let video_config = VideoConfig::default();
    let width = video_config.width as usize;
    let height = video_config.height as usize;

    // 1. キャプチャサービスのセットアップと起動
    let capture_service = CaptureService::new(video_config);
    let (rx_frame, tx_pool) = capture_service.spawn_loop()?;

    // 2. 音声処理の起動
    thread::spawn(|| {
        let audio_pipeline = audio::CpalAudioPassthrough::new("gc311");
        if let Err(e) = audio_pipeline.start() {
            eprintln!("Audio error: {e}");
        }
    });

    // 3. 表示ウィンドウの初期化
    let mut window = DisplayWindow::new("Switch Capture", width, height)?;

    // 4. メインループ（描画＆UIイベント）
    while window.is_open() {
        window.render_latest(&rx_frame, &tx_pool)?;
    }

    Ok(())
}
