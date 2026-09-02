mod audio;
mod config;
mod hardware;
mod inference;
mod video;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use tracing::error;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use audio::{AudioConfig, CpalAudioPassthrough};
use hardware::{AudioPipeline, HardwareProfile};
use inference::{InferenceConfig, InferenceWorker, PhaseRules, PhaseStatus};
use video::{CaptureService, CropArea, DisplayPanelConfig, DisplayWindow, NokhwaCapture};

const PHASE_RULES_CONFIG_PATH: &str = "config/phase_rules.toml";
const INFERENCE_CONFIG_PATH: &str = "config/inference.toml";
const DISPLAY_CONFIG_PATH: &str = "config/display.toml";
const AUDIO_CONFIG_PATH: &str = "config/audio.toml";

/// ML用のサブサンプリング間隔(フレーム数)。
const ML_SUBSAMPLING_INTERVAL_FRAMES: u32 = 30;

/// ロギング初期化(RUST_LOG 環境変数、未設定時は info)。
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

fn main() -> anyhow::Result<()> {
    init_tracing();

    let phase_rules =
        config::load_or_default::<PhaseRules>(PHASE_RULES_CONFIG_PATH, "フェーズルール");
    let inference_config =
        config::load_or_default::<InferenceConfig>(INFERENCE_CONFIG_PATH, "推論");
    let panel_config = config::load_or_default::<DisplayPanelConfig>(DISPLAY_CONFIG_PATH, "表示");
    let audio_config = config::load_or_default::<AudioConfig>(AUDIO_CONFIG_PATH, "音声");

    // キャプチャ機種の識別情報。機種追加は hardware.rs のプロファイルconst追加で対応する。
    let profile = HardwareProfile::AVERMEDIA_LIVE_GAMER_MINI_GC311;

    // 具体的な映像ソース(nokhwa)の生成は呼び出し側で行い、
    // `CaptureService` には `Box<dyn VideoSource>` として注入する。
    let video_source = NokhwaCapture::new(&profile.video, profile.video_device_keyword)?;
    let capture_service =
        CaptureService::new(Box::new(video_source), ML_SUBSAMPLING_INTERVAL_FRAMES);

    // 全体シャットダウンフラグ。表示ウィンドウクローズで立てる。
    let shutdown = Arc::new(AtomicBool::new(false));

    let (rx_display, rx_ml, capture_handle) = capture_service.spawn_loop(Arc::clone(&shutdown));

    let crop_area = Arc::new(RwLock::new(CropArea::default_relative()));
    let phase_status: PhaseStatus = Arc::new(RwLock::new(String::new()));
    // 表示側の▶ボタンで立てる、手動フェーズ進行リクエスト。
    let manual_phase_advance = Arc::new(AtomicBool::new(false));

    let shutdown_audio = Arc::clone(&shutdown);
    let audio_handle = thread::spawn(move || {
        let audio_pipeline =
            CpalAudioPassthrough::for_hardware(&profile, audio_config, shutdown_audio);
        if let Err(e) = audio_pipeline.start() {
            error!("Audio pipeline error: {e}");
        }
    });

    let inference_handle = InferenceWorker::spawn(
        rx_ml,
        inference_config,
        phase_rules,
        Arc::clone(&crop_area),
        Arc::clone(&phase_status),
        Arc::clone(&manual_phase_advance),
        Arc::clone(&shutdown),
    );

    let display_resolution = profile.video.resolution();
    let mut window = DisplayWindow::open_uncapped(
        "Switch Capture",
        display_resolution,
        panel_config,
        &manual_phase_advance,
    )?;

    println!("\n=================== クロップ調整操作 ===================");
    println!("  [矢印キー]           : 赤枠の移動 (X, Y)");
    println!("  [Shift + 矢印キー]   : 赤枠のサイズ変更 (Width, Height)");
    println!("========================================================\n");

    while window.is_open() {
        if let Err(e) = window.render_latest(&rx_display, &crop_area, &phase_status) {
            error!("Display render error: {e}");
            continue;
        }
    }

    // ウィンドウクローズ: シャットダウンを要求し、全スレッドをjoinして終了する。
    shutdown.store(true, Ordering::Relaxed);
    let _ = capture_handle.join();
    let _ = audio_handle.join();
    let _ = inference_handle.join();

    Ok(())
}
