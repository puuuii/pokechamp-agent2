use serde::de::DeserializeOwned;
use std::fs;
use tracing::error;

use anyhow::Context;

/// TOML 設定ファイルを読み込む。
pub fn load_toml<T: DeserializeOwned>(path: &str, label: &str) -> anyhow::Result<T> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("設定ファイル({label})を読み込めません: {path}"))?;
    toml::from_str(&contents)
        .with_context(|| format!("設定ファイル({label})の解析に失敗しました: {path}"))
}

/// TOML 設定ファイルを読み込む。
/// 失敗時はログして組み込みデフォルト(`Default`)にフォールバックする。
pub fn load_or_default<T: DeserializeOwned + Default>(path: &str, label: &str) -> T {
    match load_toml::<T>(path, label) {
        Ok(value) => value,
        Err(e) => {
            error!("{label}の設定を読み込めませんでした: {e}。組み込みデフォルトを使います。");
            T::default()
        }
    }
}