mod config;
mod dashboard;
mod localization;
mod realtime_logging;
mod signal_decoder;
mod ui;

use crate::dashboard::setup_dashboard_refresh;
use crate::localization::{LANGUAGE_SETTING_KEY, Language, apply_language};
use crate::realtime_logging::{
    RealtimeLoggingEvent, RealtimeLoggingSession, spawn_realtime_logging,
};
use crate::signal_decoder::definition_map;
use crate::ui::TranslationManager;
use crate::ui::can_id_manager::CanIdManagerView;
use crate::ui::log_charts::LogChartsView;
use crate::ui::settings::SettingsView;
use crate::ui::sidebar::Sidebar;
use car_logger_application::CanFrameSource;
use car_logger_domain::{RealtimeState, SignalKind};
use car_logger_storage::StorageRepository;
#[cfg(target_os = "linux")]
use car_logger_transport::SocketCanSource;
use car_logger_transport::{ConnectionMode, SerialCanSource, list_connected_interfaces};
use crossbeam_channel::unbounded;
use gettextrs::{bindtextdomain, textdomain};
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, ComboBoxText, CssProvider, Label, Stack,
    gio, glib,
};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

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
    let inhibit_cookie = Rc::new(Cell::new(None));

    application.connect_startup(|_| {
        load_css();
    });

    application.connect_shutdown(glib::clone!(
        #[strong]
        inhibit_cookie,
        move |app| {
            if let Some(cookie) = inhibit_cookie.take() {
                app.uninhibit(cookie);
            }
        }
    ));

    application.connect_activate(move |app| {
        let repo = open_repository(&database_path).map(Rc::new);
        build_ui(app, repo, database_path.clone(), &inhibit_cookie);
    });
    application.run();
}

fn open_repository(database_path: &Path) -> Option<StorageRepository> {
    match StorageRepository::open(database_path) {
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
/// - GResource から各画面の UI 定義を読み込むこと。
/// - 各ページのタイトルや設定画面のラベルを `TranslationManager` に登録すること。
/// - 言語設定の変更を検知し、リポジトリへの保存と UI の動的翻訳更新を行うこと。
/// - サイドバーのホバーによる展開・縮小アニメーションおよびボタンによる画面遷移を実現すること。
/// - アプリケーションウィンドウを表示すること。
fn build_ui(
    application: &Application,
    repository: Option<Rc<StorageRepository>>,
    database_path: PathBuf,
    inhibit_cookie: &Cell<Option<u32>>,
) {
    let builder = gtk::Builder::from_resource("/com/carlogger/CarLogger/ui/window.ui");
    let window: ApplicationWindow = builder
        .object("CarLoggerWindow")
        .expect("Could not find CarLoggerWindow");
    window.set_application(Some(application));

    if inhibit_cookie.get().is_none() {
        let cookie = application.inhibit(
            Some(&window),
            gtk::ApplicationInhibitFlags::IDLE | gtk::ApplicationInhibitFlags::SUSPEND,
            Some("Car Logger is running"),
        );
        if cookie == 0 {
            tracing::warn!("The system did not accept the sleep inhibition request");
        } else {
            inhibit_cookie.set(Some(cookie));
        }
    }

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
    let log_chart_container: GtkBox = builder
        .object("log_chart_container")
        .expect("Could not find log_chart_container");
    let dashboard_container: GtkBox = builder
        .object("dashboard_container")
        .expect("Could not find dashboard_container");
    let main_surface: GtkBox = builder
        .object("main_surface")
        .expect("Could not find main_surface");
    setup_ambient_background(&main_surface);
    let realtime_state = Arc::new(RealtimeState::new());
    setup_transport_header(
        &builder,
        repository.clone(),
        config::log_database_path(&database_path),
        realtime_state.clone(),
    );
    let dashboard_builder = gtk::Builder::from_resource("/com/carlogger/CarLogger/ui/dashboard.ui");
    let dashboard_view: gtk::ScrolledWindow = dashboard_builder
        .object("dashboard_view")
        .expect("Could not find dashboard_view");
    dashboard_container.append(&dashboard_view);
    setup_dashboard_refresh(
        &dashboard_builder,
        realtime_state,
        translation_manager.clone(),
    );

    // 各ページのタイトルラベルを登録（window.ui内）
    if let Some(lbl) = dashboard_builder.object::<Label>("lbl_dash_title") {
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

    let log_charts_view = LogChartsView::setup(translation_manager.clone(), repository.clone());
    log_chart_container.append(log_charts_view.widget());

    // 設定画面の読み込み
    let settings_builder =
        gtk::Builder::from_resource("/com/carlogger/CarLogger/ui/settings_view.ui");
    let settings_view =
        SettingsView::setup(&settings_builder, translation_manager.clone(), repository);
    settings_container.append(settings_view.widget());

    translation_manager.borrow().update_all();

    window.present();
}

fn setup_ambient_background(surface: &GtkBox) {
    let phase = Rc::new(std::cell::Cell::new(0_usize));
    glib::timeout_add_local(
        Duration::from_millis(3600),
        glib::clone!(
            #[strong]
            surface,
            #[strong]
            phase,
            move || {
                const CLASSES: [&str; 3] =
                    ["ambient-phase-1", "ambient-phase-2", "ambient-phase-3"];
                for class in CLASSES {
                    surface.remove_css_class(class);
                }
                let next = (phase.get() + 1) % 4;
                phase.set(next);
                if next > 0 {
                    surface.add_css_class(CLASSES[next - 1]);
                }
                glib::ControlFlow::Continue
            }
        ),
    );
}

fn setup_transport_header(
    builder: &gtk::Builder,
    repository: Option<Rc<StorageRepository>>,
    log_database_path: PathBuf,
    realtime_state: Arc<RealtimeState>,
) {
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

    let active_session: Rc<RefCell<Option<RealtimeLoggingSession>>> = Rc::new(RefCell::new(None));
    let (event_sender, event_receiver) = unbounded::<RealtimeLoggingEvent>();

    glib::timeout_add_local(
        Duration::from_millis(200),
        glib::clone!(
            #[strong]
            connect_button,
            #[strong]
            status_label,
            #[strong]
            active_session,
            move || {
                for event in event_receiver.try_iter() {
                    match event {
                        RealtimeLoggingEvent::Saved { total_frames } => {
                            status_label.set_text(&format!("Logging: {total_frames} frames saved"));
                        }
                        RealtimeLoggingEvent::Decoded { name, value, unit } => {
                            status_label.set_text(&format!(
                                "{name}: {:.1}{}",
                                value,
                                unit.map(|unit| format!(" {unit}")).unwrap_or_default()
                            ));
                        }
                        RealtimeLoggingEvent::ReceiveError(error) => {
                            status_label.set_text(&format!("Receive warning: {error}"));
                        }
                        RealtimeLoggingEvent::SaveError(error) => {
                            status_label.set_text(&format!("Save warning: {error}"));
                        }
                        RealtimeLoggingEvent::Stopped => {
                            active_session.replace(None);
                            connect_button.set_label("Connect");
                            status_label.set_text("Disconnected");
                        }
                    }
                }

                glib::ControlFlow::Continue
            }
        ),
    );

    connect_button.connect_clicked(glib::clone!(
        #[strong]
        connect_button,
        #[strong]
        interface_combo,
        #[strong]
        mode_combo,
        #[strong]
        status_label,
        #[strong]
        active_session,
        #[strong]
        event_sender,
        #[strong]
        repository,
        #[strong]
        log_database_path,
        #[strong]
        realtime_state,
        move |_| {
            if let Some(session) = active_session.borrow_mut().take() {
                session.request_stop();
                connect_button.set_label("Stopping...");
                status_label.set_text("Disconnecting...");
                return;
            }

            let Some(interface_path) = interface_combo.active_id().map(|id| id.to_string()) else {
                status_label.set_text("No interface selected");
                return;
            };
            let interface_label = interface_combo
                .active_text()
                .map(|text| text.to_string())
                .unwrap_or_else(|| "Unknown interface".to_string());
            let mode_label = mode_combo
                .active_text()
                .map(|text| text.to_string())
                .unwrap_or_else(|| "Stream".to_string());
            let mode = connection_mode_from_label(&mode_label);

            match open_transport_source(&interface_path, mode) {
                Ok(source) => {
                    let signal_kind = signal_kind_for_mode(mode);
                    let definitions = repository
                        .as_ref()
                        .and_then(|repository| repository.list_signal_definitions().ok())
                        .map(definition_map)
                        .unwrap_or_default();
                    let session = spawn_realtime_logging(
                        source,
                        signal_kind,
                        definitions,
                        log_database_path.clone(),
                        realtime_state.clone(),
                        event_sender.clone(),
                    );
                    active_session.replace(Some(session));
                    connect_button.set_label("Disconnect");
                    status_label.set_text(&format!("Connected: {interface_label} / {mode_label}"));
                }
                Err(error) => {
                    active_session.replace(None);
                    connect_button.set_label("Connect");
                    status_label.set_text(&format!("Connection failed: {error}"));
                }
            }
        }
    ));
}

fn open_transport_source(
    interface_path: &str,
    mode: ConnectionMode,
) -> anyhow::Result<Box<dyn CanFrameSource>> {
    #[cfg(target_os = "linux")]
    {
        if interface_path.starts_with("can") || interface_path.starts_with("vcan") {
            return Ok(Box::new(SocketCanSource::open(interface_path)?));
        }
    }

    if mode == ConnectionMode::Obd2 {
        return Ok(Box::new(SerialCanSource::open_obd2_auto(interface_path)?));
    }

    let baud_rate = 500_000;
    Ok(Box::new(SerialCanSource::open_with_mode(
        interface_path,
        baud_rate,
        mode,
    )?))
}

fn connection_mode_from_label(label: &str) -> ConnectionMode {
    if label == "OBD-2" {
        ConnectionMode::Obd2
    } else {
        ConnectionMode::Stream
    }
}

fn signal_kind_for_mode(mode: ConnectionMode) -> SignalKind {
    match mode {
        ConnectionMode::Obd2 => SignalKind::Pid,
        ConnectionMode::Stream => SignalKind::CanId,
    }
}
