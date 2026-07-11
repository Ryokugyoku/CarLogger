use crate::localization::translate;
use gtk::Label;

/// アプリケーション内のラベルの翻訳を管理する構造体。
pub struct TranslationManager {
    labels: Vec<(Label, String)>,
}

impl TranslationManager {
    /// `TranslationManager` の新しいインスタンスを作成します。
    pub fn new() -> Self {
        Self { labels: Vec::new() }
    }

    /// 指定されたラベルとメッセージIDを管理対象に追加します。
    pub fn add(&mut self, label: Label, msgid: &str) {
        self.labels.push((label, msgid.to_string()));
    }

    /// 管理されているすべてのラベルのテキストを現在のロケール設定に基づいて更新します。
    pub fn update_all(&self) {
        for (label, msgid) in &self.labels {
            label.set_text(&translate(msgid));
        }
    }
}

pub mod can_id_manager;
pub mod settings;
pub mod sidebar;
