mod audio;
mod config;
mod hardware;
mod inference;
mod video;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use tracing::error;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use audio::{AudioConfig, CpalAudioPassthrough};
use hardware::{AudioPipeline, HardwareProfile};
use inference::{
    InferenceConfig, InferenceWorker, PartyIconMatcher, PartyMatchStatus, PartySlots, PhaseRules,
    PhaseStatus, PokemonNameLookup,
};
use video::{CaptureService, CropArea, DisplayApp, DisplayPanelConfig, NokhwaCapture};

const PHASE_RULES_CONFIG_PATH: &str = "config/phase_rules.toml";
const INFERENCE_CONFIG_PATH: &str = "config/inference.toml";
const DISPLAY_CONFIG_PATH: &str = "config/display.toml";
const AUDIO_CONFIG_PATH: &str = "config/audio.toml";
const PARTY_SLOTS_CONFIG_PATH: &str = "config/party_slots.toml";
const POKEMON_ICON_DIR: &str = "img";
const ML_SUBSAMPLING_INTERVAL_FRAMES: u32 = 30;
const EMBEDDING_MODEL_PATH: &str = "models/embedding_model.onnx";
const POKEMON_DATA_PATH: &str = "raw_data/pkchPokemonData.json";

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
    let party_slots =
        config::load_or_default::<PartySlots>(PARTY_SLOTS_CONFIG_PATH, "選出パーティ位置");
    let panel_config = config::load_or_default::<DisplayPanelConfig>(DISPLAY_CONFIG_PATH, "表示");
    let audio_config = config::load_or_default::<AudioConfig>(AUDIO_CONFIG_PATH, "音声");

    let party_matcher = match PartyIconMatcher::load_from_dir(
        Path::new(EMBEDDING_MODEL_PATH),
        Path::new(POKEMON_ICON_DIR),
    ) {
        Ok(matcher) => Arc::new(matcher),
        Err(e) => {
            error!(
                "ポケモンアイコンテンプレートの読み込みに失敗しました: {e}。マッチング機能は無効になります。"
            );
            Arc::new(PartyIconMatcher::empty())
        }
    };

    let pokemon_names = match PokemonNameLookup::load_from_file(Path::new(POKEMON_DATA_PATH)) {
        Ok(lookup) => Arc::new(lookup),
        Err(e) => {
            error!(
                "ポケモン名データの読み込みに失敗しました: {e}。ファイル名表示にフォールバックします。"
            );
            Arc::new(PokemonNameLookup::empty())
        }
    };

    let profile = HardwareProfile::AVERMEDIA_LIVE_GAMER_MINI_GC311;

    let video_source = NokhwaCapture::new(&profile.video, profile.video_device_keyword)?;
    let capture_service =
        CaptureService::new(Box::new(video_source), ML_SUBSAMPLING_INTERVAL_FRAMES);

    let shutdown = Arc::new(AtomicBool::new(false));

    let (rx_display, rx_ml, capture_handle) = capture_service.spawn_loop(Arc::clone(&shutdown));

    let crop_area = Arc::new(RwLock::new(CropArea::default_relative()));
    let phase_status: PhaseStatus = Arc::new(RwLock::new(String::new()));
    let manual_phase_advance = Arc::new(AtomicBool::new(false));
    let party_match_status: PartyMatchStatus = Arc::new(RwLock::new(vec![String::new(); 12]));

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
        Arc::clone(&party_matcher),
        party_slots.clone(),
        Arc::clone(&party_match_status),
        Arc::clone(&pokemon_names),
    );

    println!("\n=================== クロップ調整操作 ===================");
    println!("  [矢印キー]           : 赤枠の移動 (X, Y)");
    println!("  [Shift + 矢印キー]   : 赤枠のサイズ変更 (Width, Height)");
    println!("========================================================\n");

    let display_resolution = profile.video.resolution();
    let total_width =
        panel_config.left_panel_width + display_resolution.0 + panel_config.right_panel_width;
    let total_height = display_resolution.1 + panel_config.bottom_panel_height;

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([total_width as f32, total_height as f32])
            .with_resizable(false),
        vsync: false,
        ..Default::default()
    };

    let shutdown_on_close = Arc::clone(&shutdown);
    let run_result = eframe::run_native(
        "Switch Capture",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(DisplayApp::new(
                cc,
                display_resolution,
                panel_config,
                rx_display,
                crop_area,
                phase_status,
                manual_phase_advance,
                party_match_status,
                party_slots,
            )))
        }),
    );

    shutdown_on_close.store(true, Ordering::Relaxed);
    let _ = capture_handle.join();
    let _ = audio_handle.join();
    let _ = inference_handle.join();

    run_result.map_err(|e| anyhow::anyhow!("eframe実行エラー: {e}"))
}
