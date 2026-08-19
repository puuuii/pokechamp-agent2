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
use std::thread;
use std::time::Instant;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FPS: u32 = 60;
const BUF_SIZE: usize = (WIDTH * HEIGHT) as usize;

fn spawn_capture_thread(tx_frame: Sender<Vec<u32>>, rx_pool: Receiver<Vec<u32>>) {
    let index = CameraIndex::Index(0);
    let format = CameraFormat::new(Resolution::new(WIDTH, HEIGHT), FrameFormat::MJPEG, FPS);
    let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Exact(format));

    thread::spawn(move || {
        let mut camera = match Camera::new(index, requested) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("カメラオープン失敗: {e}");
                return;
            }
        };

        if let Err(e) = camera.open_stream() {
            eprintln!("ストリーム開始失敗: {e}");
            return;
        }

        println!("actual format (reported): {:?}", camera.camera_format());

        let mut frame_count = 0u32;
        let mut last_report = Instant::now();

        loop {
            let frame = match camera.frame() {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("frame error: {e}");
                    continue;
                }
            };
            let decoded = match frame.decode_image::<RgbFormat>() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("decode error: {e}");
                    continue;
                }
            };

            // バッファプールから未使用バッファを取得（無ければ新規確保）
            let mut buf = rx_pool.try_recv().unwrap_or_else(|_| vec![0u32; BUF_SIZE]);

            let raw = decoded.as_raw();
            // SIMDやイテレータの最適化がかかりやすい形式に変更
            for (px, out) in raw.chunks_exact(3).zip(buf.iter_mut()) {
                *out = ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | (px[2] as u32);
            }

            // 最新フレームを送信。受信側が詰まっている場合は古いフレームを落としてバッファを回収
            if let Err(crossbeam_channel::TrySendError::Full(returned_buf)) = tx_frame.try_send(buf)
            {
                // 送信失敗時はバッファをプールに戻す
                let _ = rx_pool.try_recv();
                drop(returned_buf);
            }

            frame_count += 1;
            if last_report.elapsed().as_secs() >= 1 {
                println!("capture fps: {}", frame_count);
                frame_count = 0;
                last_report = Instant::now();
            }
        }
    });
}

fn main() -> Result<()> {
    // バッファプール用のチャンネルと、フレーム送信用チャンネルを作成
    let (tx_frame, rx_frame): (Sender<Vec<u32>>, Receiver<Vec<u32>>) = bounded(2);
    let (tx_pool, rx_pool): (Sender<Vec<u32>>, Receiver<Vec<u32>>) = bounded(3);

    // 初期バッファをプールに供給
    for _ in 0..3 {
        let _ = tx_pool.send(vec![0u32; BUF_SIZE]);
    }

    spawn_capture_thread(tx_frame, rx_pool);

    thread::spawn(|| {
        if let Err(e) = audio::run_passthrough() {
            eprintln!("audio error: {e}");
        }
    });

    let mut window = Window::new(
        "Switch2 Capture",
        WIDTH as usize,
        HEIGHT as usize,
        WindowOptions::default(),
    )?;

    // 手動フレーム制御にするためターゲットFPSの制限を解除
    window.set_target_fps(0);

    let mut current_frame = vec![0u32; BUF_SIZE];

    while window.is_open() {
        // 新しいフレームがあれば最新のものに更新し、使い終わったバッファをプールに返す
        let mut updated = false;
        while let Ok(frame) = rx_frame.try_recv() {
            let old_frame = std::mem::replace(&mut current_frame, frame);
            let _ = tx_pool.try_send(old_frame);
            updated = true;
        }

        if updated {
            window.update_with_buffer(&current_frame, WIDTH as usize, HEIGHT as usize)?;
        } else {
            // イベント処理のみ実施して描画更新をスキップ
            window.update();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    Ok(())
}

mod audio {
    // （オーディオモジュールは変更なし）
    use anyhow::{Context, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use ringbuf::{
        HeapRb,
        traits::{Consumer, Producer, Split},
    };

    pub fn run_passthrough() -> Result<()> {
        let host = cpal::default_host();

        let input_device = host
            .input_devices()?
            .find(|d| {
                d.name()
                    .map(|n| {
                        let n = n.to_lowercase();
                        n.contains("gc311") || n.contains("streamline")
                    })
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

        println!("input: {} ch, {} Hz", in_channels, in_rate);
        println!("output: {} ch, {} Hz", out_channels, out_rate);

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
            move |err| eprintln!("input stream error: {err}"),
            None,
        )?;

        let output_stream = output_device.build_output_stream(
            &output_config.into(),
            move |data: &mut [f32], _| {
                for sample in data.iter_mut() {
                    *sample = consumer.try_pop().unwrap_or(0.0);
                }
            },
            move |err| eprintln!("output stream error: {err}"),
            None,
        )?;

        input_stream.play()?;
        output_stream.play()?;

        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
}
