mod config;
mod localization;
mod ui;

use crate::localization::{LANGUAGE_SETTING_KEY, Language, apply_language};
use crate::ui::TranslationManager;
use crate::ui::can_id_manager::CanIdManagerView;
use crate::ui::settings::SettingsView;
use crate::ui::sidebar::Sidebar;
use car_logger_application::CanFrameSource;
use car_logger_storage::SqliteCanFrameRepository;
#[cfg(target_os = "linux")]
use car_logger_transport::SocketCanSource;
use car_logger_transport::{SerialCanSource, list_connected_interfaces};
use gettextrs::{bindtextdomain, textdomain};
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, ComboBoxText, CssProvider, Label, Stack,
    gio, glib,
};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

const APPLICATION_ID: &str = "com.carlogger.CarLogger";

/// アプリケーションのエントリーポイント。
///
/// # 要件
/// - ログ出力（tracing）の初期化を行うこと。
/// - 保存された設定または環境変数からロケールを設定し、gettext の初期化を行うこと。
/// - GTKアプリケーションのリソースを登録すること。
/// - GTKアプリケーションインスタンスを作成し、起動（startup）およびアクティブ化（activate）シグナルをハンドリングすること。
/// - アプリケーションのメインループを実行すること。
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let database_path = config::database_path();
    let repository = open_repository(&database_path);
    let initial_lang = repository
        .as_ref()
        .and_then(|repo| repo.get_setting(LANGUAGE_SETTING_KEY).ok().flatten());
    let initial_language = Language::from_saved_or_environment(initial_lang.as_deref());

    apply_language(initial_language);
    let _ = bindtextdomain("car-logger", "/usr/share/locale"); // 実際には適切なパスを設定
    let _ = textdomain("car-logger");

    // リソースの登録
    gio::resources_register_include!("car-logger.gresource")
        .expect("Failed to register resources.");

    let application = Application::builder()
        .application_id(APPLICATION_ID)
        .build();

    application.connect_startup(|_| {
        load_css();
    });

    application.connect_activate(move |app| {
        let repo = open_repository(&database_path).map(Rc::new);
        build_ui(app, repo);
    });
    application.run();
}

fn open_repository(database_path: &Path) -> Option<SqliteCanFrameRepository> {
    match SqliteCanFrameRepository::open(database_path) {
        Ok(repository) => Some(repository),
        Err(error) => {
            tracing::error!(
                path = %database_path.display(),
                "Failed to open settings database: {error}"
            );
            None
        }
    }
}

/// アプリケーション全体に適用するCSSスタイルシートをリソースから読み込みます。
///
/// # 要件
/// - GResource 内の指定されたパスから CSS データを読み込むこと。
/// - デフォルトのディスプレイに対して CSS プロバイダーを登録し、アプリケーション全体でスタイルが適用されるようにすること。
fn load_css() {
    let provider = CssProvider::new();
    provider.load_from_resource("/com/carlogger/CarLogger/css/style.css");

    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("Could not connect to a display."),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// アプリケーションのメインユーザーインターフェースを構築します。
///
/// # 引数
/// * `application` - UIを紐付けるGTKアプリケーションインスタンス。
/// * `repository` - 設定の保存・取得に使用するデータベースリポジトリ（任意）。
///
/// # 要件
/// - GResource から UI 定義（window.ui, sidebar.ui, settings_view.ui）を読み込むこと。
/// - 各ページのタイトルや設定画面のラベルを `TranslationManager` に登録すること。
/// - 言語設定の変更を検知し、リポジトリへの保存と UI の動的翻訳更新を行うこと。
/// - サイドバーのホバーによる展開・縮小アニメーションおよびボタンによる画面遷移を実現すること。
/// - アプリケーションウィンドウを表示すること。
fn build_ui(application: &Application, repository: Option<Rc<SqliteCanFrameRepository>>) {
    let builder = gtk::Builder::from_resource("/com/carlogger/CarLogger/ui/window.ui");
    let window: ApplicationWindow = builder
        .object("CarLoggerWindow")
        .expect("Could not find CarLoggerWindow");
    window.set_application(Some(application));

    let translation_manager = Rc::new(RefCell::new(TranslationManager::new()));

    let main_stack: Stack = builder
        .object("main_stack")
        .expect("Could not find main_stack");
    let sidebar_container: GtkBox = builder
        .object("sidebar_container")
        .expect("Could not find sidebar_container");
    let settings_container: GtkBox = builder
        .object("settings_container")
        .expect("Could not find settings_container");
    let can_id_manager_container: GtkBox = builder
        .object("can_id_manager_container")
        .expect("Could not find can_id_manager_container");
    setup_transport_header(&builder);

    // 各ページのタイトルラベルを登録（window.ui内）
    if let Some(lbl) = builder.object::<Label>("lbl_dash_title") {
        translation_manager.borrow_mut().add(lbl, "Dashboard");
    }
    if let Some(lbl) = builder.object::<Label>("lbl_logs_title") {
        translation_manager.borrow_mut().add(lbl, "Log Analysis");
    }
    if let Some(lbl) = builder.object::<Label>("lbl_maint_title") {
        translation_manager.borrow_mut().add(lbl, "Maintenance");
    }

    // サイドバーの読み込み
    let sidebar_builder = gtk::Builder::from_resource("/com/carlogger/CarLogger/ui/sidebar.ui");
    let sidebar = Sidebar::setup(&sidebar_builder, main_stack, translation_manager.clone());
    sidebar_container.append(sidebar.widget());

    // CAN ID管理画面の読み込み
    let can_id_manager_builder =
        gtk::Builder::from_resource("/com/carlogger/CarLogger/ui/can_id_manager_view.ui");
    let can_id_manager_view = CanIdManagerView::setup(
        &can_id_manager_builder,
        translation_manager.clone(),
        repository.clone(),
    );
    can_id_manager_container.append(can_id_manager_view.widget());

    // 設定画面の読み込み
    let settings_builder =
        gtk::Builder::from_resource("/com/carlogger/CarLogger/ui/settings_view.ui");
    let settings_view =
        SettingsView::setup(&settings_builder, translation_manager.clone(), repository);
    settings_container.append(settings_view.widget());

    translation_manager.borrow().update_all();

    window.present();
}

fn setup_transport_header(builder: &gtk::Builder) {
    let interface_combo: ComboBoxText = builder
        .object("cmb_transport_interface")
        .expect("Could not find cmb_transport_interface");
    let mode_combo: ComboBoxText = builder
        .object("cmb_transport_mode")
        .expect("Could not find cmb_transport_mode");
    let connect_button: Button = builder
        .object("btn_transport_connect")
        .expect("Could not find btn_transport_connect");
    let status_label: Label = builder
        .object("lbl_transport_status")
        .expect("Could not find lbl_transport_status");

    interface_combo.remove_all();
    let interfaces = list_connected_interfaces();
    for interface in &interfaces {
        let label = format!("{} - {}", interface.manufacturer, interface.name);
        interface_combo.append(Some(&interface.path), &label);
    }

    if interfaces.is_empty() {
        interface_combo.append(Some("none"), "No interfaces detected");
        interface_combo.set_active(Some(0));
        interface_combo.set_sensitive(false);
        connect_button.set_sensitive(false);
        status_label.set_text("No device");
    } else {
        interface_combo.set_active(Some(0));
        connect_button.set_sensitive(true);
    }

    let active_source: Rc<RefCell<Option<Box<dyn CanFrameSource>>>> = Rc::new(RefCell::new(None));

    connect_button.connect_clicked(glib::clone!(
        #[strong]
        interface_combo,
        #[strong]
        mode_combo,
        #[strong]
        status_label,
        #[strong]
        active_source,
        move |_| {
            let Some(interface_path) = interface_combo.active_id().map(|id| id.to_string()) else {
                status_label.set_text("No interface selected");
                return;
            };
            let interface_label = interface_combo
                .active_text()
                .map(|text| text.to_string())
                .unwrap_or_else(|| "Unknown interface".to_string());
            let mode = mode_combo
                .active_text()
                .map(|text| text.to_string())
                .unwrap_or_else(|| "Stream".to_string());

            match open_transport_source(&interface_path, &mode) {
                Ok(source) => {
                    active_source.replace(Some(source));
                    status_label.set_text(&format!("Connected: {interface_label} / {mode}"));
                }
                Err(error) => {
                    active_source.replace(None);
                    status_label.set_text(&format!("Connection failed: {error}"));
                }
            }
        }
    ));
}

fn open_transport_source(
    interface_path: &str,
    mode: &str,
) -> anyhow::Result<Box<dyn CanFrameSource>> {
    #[cfg(target_os = "linux")]
    {
        if interface_path.starts_with("can") || interface_path.starts_with("vcan") {
            return Ok(Box::new(SocketCanSource::open(interface_path)?));
        }
    }

    let baud_rate = if mode == "OBD-2" { 38_400 } else { 500_000 };
    Ok(Box::new(SerialCanSource::open(interface_path, baud_rate)?))
}
