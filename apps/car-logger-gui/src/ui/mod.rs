use crate::localization::translate;
use gtk::prelude::{ButtonExt, CheckButtonExt, WidgetExt};
use gtk::{Button, CheckButton, DrawingArea, Label};

/// アプリケーション内のラベルの翻訳を管理する構造体。
pub struct TranslationManager {
    widgets: Vec<(TranslatableWidget, String)>,
    redraw_areas: Vec<DrawingArea>,
}

enum TranslatableWidget {
    Label(Label),
    Button(Button),
    CheckButton(CheckButton),
}

impl TranslationManager {
    /// `TranslationManager` の新しいインスタンスを作成します。
    pub fn new() -> Self {
        Self {
            widgets: Vec::new(),
            redraw_areas: Vec::new(),
        }
    }

    /// 指定されたラベルとメッセージIDを管理対象に追加します。
    pub fn add(&mut self, label: Label, msgid: &str) {
        self.widgets
            .push((TranslatableWidget::Label(label), msgid.to_string()));
    }

    pub fn add_button(&mut self, button: Button, msgid: &str) {
        self.widgets
            .push((TranslatableWidget::Button(button), msgid.to_string()));
    }

    pub fn add_check_button(&mut self, button: CheckButton, msgid: &str) {
        self.widgets
            .push((TranslatableWidget::CheckButton(button), msgid.to_string()));
    }

    pub fn add_redraw_area(&mut self, drawing_area: DrawingArea) {
        self.redraw_areas.push(drawing_area);
    }

    /// 管理されているすべてのラベルのテキストを現在のロケール設定に基づいて更新します。
    pub fn update_all(&self) {
        for (widget, msgid) in &self.widgets {
            let translated = translate(msgid);
            match widget {
                TranslatableWidget::Label(label) => label.set_text(&translated),
                TranslatableWidget::Button(button) => button.set_label(&translated),
                TranslatableWidget::CheckButton(button) => button.set_label(Some(&translated)),
            }
        }

        for drawing_area in &self.redraw_areas {
            drawing_area.queue_draw();
        }
    }
}

pub mod can_id_manager;
pub mod health;
pub mod log_charts;
pub mod settings;
pub mod sidebar;
