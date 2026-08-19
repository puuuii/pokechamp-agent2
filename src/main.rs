// src/main.rs
use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use minifb::{Window, WindowOptions};
use nokhwa::{
    pixel_format::RgbFormat,
    utils::{CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution},
    Camera,
};
use std::thread;
use std::time::Instant;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FPS: u32 = 60;

fn spawn_capture_thread(tx: Sender<Vec<u32>>) {
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

        // 注意: ここで報告されるframe_rateはドライバ/nokhwa側の表示バグで
        // 実態と違うことがある。実際のfpsは下のcapture fpsログで判断する。
        println!("actual format (reported): {:?}", camera.camera_format());

        let width = WIDTH as usize;
        let height = HEIGHT as usize;
        let mut buf = vec![0u32; width * height];

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
            let raw = decoded.as_raw();
            for (i, px) in raw.chunks_exact(3).enumerate() {
                buf[i] = ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | px[2] as u32;
            }
            let _ = tx.try_send(buf.clone());

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
    let (tx, rx): (Sender<Vec<u32>>, Receiver<Vec<u32>>) = bounded(2);
    spawn_capture_thread(tx);

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
    window.set_target_fps(60);

    let mut last_frame = vec![0u32; (WIDTH * HEIGHT) as usize];

    while window.is_open() {
        while let Ok(f) = rx.try_recv() {
            last_frame = f;
        }
        window.update_with_buffer(&last_frame, WIDTH as usize, HEIGHT as usize)?;
    }

    Ok(())
}

mod audio {
    use anyhow::{Context, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use ringbuf::{traits::{Consumer, Producer, Split}, HeapRb};

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

        // 出力側のバッファ(出力レート・出力chドメイン)を確保
        let ring = HeapRb::<f32>::new(out_rate as usize * out_channels * 2); // 約2秒分
        let (mut producer, mut consumer) = ring.split();

        let ratio = out_rate as f64 / in_rate as f64;
        let mut last_mono_sample: f32 = 0.0;

        let input_stream = input_device.build_input_stream(
            &input_config.into(),
            move |data: &[f32], _| {
                // 入力をモノラルにダウンミックス
                let in_frames: Vec<f32> = data
                    .chunks_exact(in_channels)
                    .map(|c| c.iter().sum::<f32>() / in_channels as f32)
                    .collect();
                let n_in = in_frames.len();
                if n_in == 0 {
                    return;
                }

                // サンプルレート変換（線形補間）+ 出力チャンネル数へ複製
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
