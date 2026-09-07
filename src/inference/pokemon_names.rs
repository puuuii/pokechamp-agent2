use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use tracing::warn;

/// `raw_data/pkchPokemonData.json` の1エントリ。
/// 名前以外の大量のフィールド(types, bs, moves, items...)は無視する。
#[derive(Debug, Deserialize)]
struct PokemonEntry {
    name: String,
}

/// アイコンファイル名(拡張子なし、例: "199_1")をキーとしてポケモン名を引く辞書。
/// `raw_data/pkchPokemonData.json` はキー(図鑑番号+フォーム番号)をトップレベルの
/// オブジェクトキーとして持ち、各値が `name` フィールドを持つ形式。
pub struct PokemonNameLookup {
    key_to_name: HashMap<String, String>,
}

impl PokemonNameLookup {
    pub fn load_from_file(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!("ポケモン名データを読み込めません({}): {e}", path.display())
        })?;

        let raw: HashMap<String, PokemonEntry> = serde_json::from_str(&contents).map_err(|e| {
            anyhow::anyhow!("ポケモン名データの解析に失敗しました({}): {e}", path.display())
        })?;

        let key_to_name: HashMap<String, String> = raw
            .into_iter()
            .map(|(key, entry)| (key, entry.name))
            .collect();

        anyhow::ensure!(
            !key_to_name.is_empty(),
            "ポケモン名データが空です: {}",
            path.display()
        );

        Ok(Self { key_to_name })
    }

    /// 辞書を持たない空のルックアップ(読み込み失敗時のフォールバック用)。
    /// `resolve` は常に元のファイル名をそのまま返す。
    pub fn empty() -> Self {
        Self {
            key_to_name: HashMap::new(),
        }
    }

    /// アイコンファイル名(例: "199_1.png")からポケモン名を引く。
    /// 未登録キーの場合は警告ログを出し、元のファイル名をそのまま返す。
    /// 空文字列(未マッチ)の場合はそのまま空文字列を返す。
    pub fn resolve(&self, file_name: &str) -> String {
        if file_name.is_empty() {
            return String::new();
        }
        let key = file_name.strip_suffix(".png").unwrap_or(file_name);
        match self.key_to_name.get(key) {
            Some(name) => name.clone(),
            None => {
                warn!("ポケモン名データに未登録のキーです: {key}");
                file_name.to_string()
            }
        }
    }
}