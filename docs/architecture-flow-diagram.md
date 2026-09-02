# アーキテクチャ フロー図

> 構成: main(表示) / キャプチャ / 推論 / 音声 の4スレッド。
> 映像は 1280x720@60 YUYV を nokhwa で取得し RRGGBB u32 にデコード、
> 音声は 50ms 相当のリングバッファでパススルー。

## 1. データフロー（映像・音声・共有状態）

```mermaid
flowchart TB
    subgraph HW[ハードウェア]
        CAM[キャプチャカード カメラ<br>1280x720 @60 YUYV]
        AIN[キャプチャカード 音声入力]
        AOUT[既定音声出力]
    end

    subgraph VID[映像パス]
        CAP[キャプチャスレッド<br>CaptureService::spawn_loop]
        DECODE[YUYV to RRGGBB デコード<br>rayon 並列]
        FB[FrameBuffer<br>Arc of Vec u32]
        TxD[tx_display<br>bounded 1 最新フレーム優先]
        TxM[tx_ml<br>bounded 1 30フレーム毎]
        DISP[DisplayWindow render<br>mainスレッド]
        INF[推論スレッド<br>run_analysis_loop]
        PRE[preprocess_white_text_extraction<br>バイリニア3x拡大 + 白文字抽出]
        BMP[SoftwareBitmap RGBA8]
        OCR[Windows OcrEngine ja-JP]
        PHASE[PhaseDetector.tick<br>フェーズ遷移判定]
    end

    subgraph AUD[音声パス CpalAudioPassthrough]
        RIN[input stream cb]
        RS[LinearResampler<br>レート/チャネル変換]
        RB[ringbuf f32<br>容量 = 50ms相当]
        ROUT[output stream cb]
    end

    STATUS[phase_status<br>Arc RwLock String]
    CROP[crop_area<br>Arc RwLock CropArea]
    MPA[manual_phase_advance<br>Arc AtomicBool]
    SHUT[shutdown<br>Arc AtomicBool]

    CAM --> CAP --> DECODE --> FB
    FB -->|毎フレーム| TxD --> DISP
    FB -->|30フレーム毎| TxM --> INF
    INF -->|3秒スロットル| PRE --> BMP --> OCR --> PHASE
    PHASE -->|書き込み| STATUS
    STATUS -->|読み込み| DISP
    DISP -->|矢印キーで更新| CROP
    CROP -->|パーティ名OCR| INF
    DISP -->|▶クリック| MPA
    MPA -->|swap false| INF
    AIN --> RIN --> RS --> RB --> ROUT --> AOUT
    DISP -->|ウィンドウクローズ| SHUT
    SHUT -->|ループ終了| CAP
    SHUT -->|ループ終了| INF
    SHUT -->|100msポーリング| AUD
```

## 2. スレッドシーケンス（起動 → 定常 → シャットダウン）

```mermaid
sequenceDiagram
    autonumber
    participant M as Main(表示スレッド)
    participant C as キャプチャスレッド
    participant I as 推論スレッド
    participant A as 音声スレッド
    participant W as Windows OCR ja-JP
    participant HW as キャプチャカード GC311

    Note over M: init_tracing
    M->>M: load_or_default x4 (phase_rules/inference/display/audio)
    M->>M: HardwareProfile const (GC311)
    M->>M: NokhwaCapture::new (デバイス名一致, カメラオープン)
    M->>C: CaptureService::spawn_loop(shutdown)
    M->>A: thread: CpalAudioPassthrough::for_hardware(...).start()
    M->>I: InferenceWorker::spawn(rx_ml, config, rules, ...)
    M->>M: DisplayWindow::open_uncapped (minifb)

    A->>A: 入力デバイス検索 (gc311)
    A->>A: リングバッファ容量 = 50ms相当サンプル数
    A->>A: input stream play / output stream play

    loop キャプチャスレッド (1フレーム毎 60fps)
        C->>HW: capture_frame()
        HW-->>C: YUYV フレーム
        Note over C: rayon並列 RRGGBB u32 デコード (FrameBuffer)
        C->>M: tx_display try_send (bounded 1, 古い破棄)
        Note over C: 30フレーム毎
        C->>I: tx_ml try_send (bounded 1)
    end

    loop 推論スレッド (MLフレーム毎)
        I->>I: manual_phase_advance.swap(false)?
        Note over I: trueなら advance_manually + set_phase_text
        opt 3秒スロットル通過
            I->>I: パーティ名OCR (現状ログのみ)
            I->>I: ocr_target_match: crop.to_pixels(1280,720)
            I->>I: preprocess (3x拡大 + 白文字抽出)
            I->>W: OcrEngine.RecognizeAsync(SoftwareBitmap)
            W-->>I: テキスト (空白除去済み)
            I->>I: PhaseDetector.tick: フェーズ遷移判定
            I->>I: set_phase_text
        end
    end

    loop Mainスレッド render (無制限fps)
        M->>M: crop_input.handle (矢印キーで crop_area 書き込み)
        M->>M: phase_button.handle_click (▶で manual_phase_advance = true)
        M->>M: update_phase_panel (phase_status 読み込み)
        M->>M: drain rx_display to current_frame
        M->>M: blit_video_frame + window.update_with_buffer
    end

    loop 音声スレッド (ストリームコールバック)
        A->>A: input cb: LinearResamplerで ringbuf push
        A->>A: output cb: ringbuf pop (空なら無音)
        A->>A: shutdownポーリング (100ms)
    end

    M->>M: ウィンドウクローズ
    M->>M: shutdown.store(true)
    M->>C: join (ループ終了)
    M->>A: join (ストリーム停止)
    M->>I: join (チャネルクローズ)
```

## 3. フェーズ状態遷移

> 自動判定は `PhaseDetector::detect_from_idle / detect_selecting / detect_battling`。
> 「待機」と「対戦終了」は監視上同一状態（`detect_from_idle`）だが表示・手動進行サイクルのため別状態。
> 手動進行は ▶ボタンで 選出 → バトル → 対戦終了 → 選出… を循環（待機はサイクル外）。

```mermaid
stateDiagram-v2
    [*] --> Waiting : 起動
    Waiting --> Ended : 自動: ended文字列 5種以上
    Waiting --> Selecting : 自動: リボン文字列 3種以上
    Ended --> Selecting : 自動: リボン一致
    Selecting --> Battling : 自動: リボン消滅
    Battling --> Ended : 自動: ended文字列 5種以上
    Waiting --> Selecting : 手動 ▶
    Selecting --> Battling : 手動 ▶
    Battling --> Ended : 手動 ▶
    Ended --> Selecting : 手動 ▶
```
