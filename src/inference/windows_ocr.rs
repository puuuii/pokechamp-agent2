use crossbeam_channel::Receiver;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::{debug, info};

use crate::hardware::FrameBuffer;
use crate::video::CropArea;

use super::analyzer::FrameAnalyzer;
use super::{ManualPhaseAdvance, PartyIconMatcher, PartyMatchStatus, PartySlots, PokemonNameLookup};

/// フレーム分析ループ(analyzer 汎用)。
#[allow(clippy::too_many_arguments)]
pub fn run_analysis_loop<A: FrameAnalyzer>(
    rx_ml: Receiver<FrameBuffer>,
    analysis_interval: Duration,
    mut analyzer: A,
    crop_area: Arc<RwLock<CropArea>>,
    phase_status: Arc<RwLock<String>>,
    manual_phase_advance: ManualPhaseAdvance,
    shutdown: Arc<AtomicBool>,
    party_matcher: Arc<PartyIconMatcher>,
    party_slots: PartySlots,
    party_match_status: PartyMatchStatus,
    frame_resolution: (usize, usize),
    pokemon_names: Arc<PokemonNameLookup>,
) -> anyhow::Result<()> {
    let mut last_analysis_time = Instant::now() - analysis_interval;
    // 選出フェーズ1回の滞在につき、パーティマッチングを1度だけ行うためのフラグ。
    // フェーズが切り替わるたびにリセットする。
    let mut party_matched_this_phase = false;

    set_phase_text(&phase_status, &analyzer.phase_text());

    for frame in rx_ml.iter() {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        if manual_phase_advance.swap(false, Ordering::Relaxed) {
            let text = analyzer.advance_manually();
            set_phase_text(&phase_status, &text);
            // 手動進行でもフェーズが切り替わったとみなし、フラグをリセットする。
            party_matched_this_phase = false;
            if analyzer.is_selecting() && !party_matched_this_phase {
                run_party_match(
                    &party_matcher,
                    &party_slots,
                    &frame,
                    frame_resolution,
                    &party_match_status,
                    &pokemon_names,
                );
                party_matched_this_phase = true;
            }
            continue;
        }

        if last_analysis_time.elapsed() < analysis_interval {
            continue;
        }
        last_analysis_time = Instant::now();

        run_party_name_analysis(&analyzer, &frame, &crop_area)?;

        if let Some(change) = analyzer.tick(&frame)? {
            info!(?change.phase, "Phase transition: {}", change.display_text);
            set_phase_text(&phase_status, &change.display_text);
            // フェーズが切り替わったので、次に選出フェーズへ入ったときにまた1回だけ推定できるようにする。
            party_matched_this_phase = false;
        }

        if analyzer.is_selecting() && !party_matched_this_phase {
            run_party_match(
                &party_matcher,
                &party_slots,
                &frame,
                frame_resolution,
                &party_match_status,
                &pokemon_names,
            );
            party_matched_this_phase = true;
        }
    }

    Ok(())
}

/// 選出フェーズ中、12箇所のポケモンアイコンをテンプレートマッチングし、
/// 結果(味方1〜6、相手1〜6の順)を表示側の共有状態に書き込む。
fn run_party_match(
    party_matcher: &PartyIconMatcher,
    party_slots: &PartySlots,
    frame: &FrameBuffer,
    frame_resolution: (usize, usize),
    party_match_status: &PartyMatchStatus,
    pokemon_names: &PokemonNameLookup,
) {
    let (frame_width, frame_height) = frame_resolution;

    let names: Vec<String> = party_slots
        .all_crops()
        .iter()
        .map(|&crop| {
            party_matcher
                .match_crop(frame, frame_width, frame_height, crop)
                .map(|file_name| pokemon_names.resolve(&file_name))
                .unwrap_or_default()
        })
        .collect();

    let mut guard = party_match_status.write().unwrap();
    if *guard != names {
        *guard = names;
    }
}

/// パーティ名などユーザー調整枠のOCR。未完成機能のため現状はログ出力のみ。
fn run_party_name_analysis<A: FrameAnalyzer>(
    analyzer: &A,
    frame: &FrameBuffer,
    crop_area: &Arc<RwLock<CropArea>>,
) -> anyhow::Result<()> {
    let crop = crop_area.read().unwrap();
    if let Some(normalized_text) = analyzer.recognize_party_name(frame, &crop)? {
        debug!("OCR Normalized: {normalized_text}");
    }
    Ok(())
}

/// 表示側のフェーズテキストを更新する(変化がなければ何もしない)。
fn set_phase_text(phase_status: &Arc<RwLock<String>>, text: &str) {
    let mut guard = phase_status.write().unwrap();
    if guard.as_str() != text {
        *guard = text.to_string();
    }
}
