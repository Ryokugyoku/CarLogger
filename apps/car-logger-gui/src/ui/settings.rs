use crate::localization::{LANGUAGE_SETTING_KEY, Language, apply_language};
use crate::ui::TranslationManager;
use car_logger_storage::SqliteCanFrameRepository;
use gtk::prelude::*;
use gtk::{Box as GtkBox, ComboBoxText, Label, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub struct SettingsView {
    root: GtkBox,
}

impl SettingsView {
    pub fn setup(
        builder: &gtk::Builder,
        translation_manager: Rc<RefCell<TranslationManager>>,
        repository: Option<Rc<SqliteCanFrameRepository>>,
    ) -> Self {
        let root: GtkBox = builder
            .object("settings_view")
            .expect("Could not find settings_view");
        let combo_language: ComboBoxText = builder
            .object("combo_language")
            .expect("Could not find combo_language");

        // 設定画面のラベルを登録
        if let Some(lbl) = builder.object::<Label>("lbl_settings_title") {
            translation_manager.borrow_mut().add(lbl, "Settings");
        }
        if let Some(lbl) = builder.object::<Label>("lbl_language") {
            translation_manager.borrow_mut().add(lbl, "Language");
        }
        if let Some(lbl) = builder.object::<Label>("lbl_restart_info") {
            translation_manager
                .borrow_mut()
                .add(lbl, "Some changes might require restart.");
        }

        let current_lang = repository
            .as_ref()
            .and_then(|repo| repo.get_setting(LANGUAGE_SETTING_KEY).ok().flatten());
        let current_language = Language::from_saved_or_environment(current_lang.as_deref());
        combo_language.set_active_id(Some(current_language.combo_id()));

        combo_language.connect_changed(glib::clone!(
            #[strong]
            translation_manager,
            #[strong]
            repository,
            move |combo| {
                if let Some(id) = combo.active_id() {
                    tracing::info!("Language changed to: {}", id);
                    let language = Language::from_combo_id(id.as_str());
                    apply_language(language);

                    if let Some(repo) = &repository
                        && let Err(e) = repo.set_setting(LANGUAGE_SETTING_KEY, language.locale())
                    {
                        tracing::error!("Failed to save language setting: {}", e);
                    }

                    translation_manager.borrow().update_all();
                }
            }
        ));

        Self { root }
    }

    pub fn widget(&self) -> &GtkBox {
        &self.root
    }
}
