mod config;
mod dashboard;
mod live_dashboard;
mod localization;
mod realtime_logging;
mod signal_decoder;
mod ui;
mod updater;

use crate::dashboard::create_dashboard;
use crate::live_dashboard::setup_dashboard_refresh;
use crate::localization::{LANGUAGE_SETTING_KEY, Language, apply_language};
use crate::realtime_logging::{
    RealtimeLoggingConfig, RealtimeLoggingEvent, RealtimeLoggingSession, spawn_realtime_logging,
};
use crate::signal_decoder::definition_map;
use crate::ui::TranslationManager;
use crate::ui::can_id_manager::CanIdManagerView;
use crate::ui::health::HealthView;
use crate::ui::log_charts::LogChartsView;
use crate::ui::settings::SettingsView;
use crate::ui::sidebar::Sidebar;
use car_logger_application::CanFrameSource;
use car_logger_application::connection::{
    ConnectionTarget, IdentificationOutcome, identify_vehicle,
};
use car_logger_application::pid_scan::PidScanConfig;
use car_logger_domain::{FuelType, RealtimeState, SignalKind};
use car_logger_storage::{NewVehicle, StorageRepository};
#[cfg(target_os = "linux")]
use car_logger_transport::SocketCanSource;
use car_logger_transport::{ConnectionMode, SerialCanSource, list_connected_interfaces};
use crossbeam_channel::unbounded;
use gettextrs::{bindtextdomain, textdomain};
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, ComboBoxText, CssProvider, Dialog,
    Entry, Label, MessageDialog, SpinButton, Stack, gio, glib,
};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

const APPLICATION_ID: &str = "com.carlogger.CarLogger";
const APPLICATION_NAME: &str = "APEX//TRACE";
const APPLICATION_ICON_NAME: &str = "apex-trace";

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

    glib::set_application_name(APPLICATION_NAME);

    let application = Application::builder()
        .application_id(APPLICATION_ID)
        .build();
    let inhibit_cookie = Rc::new(Cell::new(None));

    application.connect_startup(|_| {
        let display = gtk::gdk::Display::default().expect("Could not connect to a display.");
        gtk::IconTheme::for_display(&display).add_resource_path("/com/carlogger/CarLogger/icons");
        gtk::Window::set_default_icon_name(APPLICATION_ICON_NAME);
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
    window.set_icon_name(Some(APPLICATION_ICON_NAME));

    if inhibit_cookie.get().is_none() {
        let cookie = application.inhibit(
            Some(&window),
            gtk::ApplicationInhibitFlags::IDLE | gtk::ApplicationInhibitFlags::SUSPEND,
            Some("APEX//TRACE is running"),
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
    let health_container: GtkBox = builder
        .object("health_container")
        .expect("Could not find health_container");
    let main_surface: GtkBox = builder
        .object("main_surface")
        .expect("Could not find main_surface");
    setup_ambient_background(&main_surface);
    let realtime_state = Arc::new(RealtimeState::new());
    let is_connected = Arc::new(AtomicBool::new(false));
    let (update_sender, update_receiver) = mpsc::channel::<updater::UpdateEvent>();
    setup_transport_header(
        &builder,
        &window,
        repository.clone(),
        database_path.clone(),
        config::log_database_path(&database_path),
        realtime_state.clone(),
        is_connected.clone(),
    );
    let dashboard_builder = gtk::Builder::from_resource("/com/carlogger/CarLogger/ui/dashboard.ui");
    let dashboard_view: gtk::ScrolledWindow = dashboard_builder
        .object("dashboard_view")
        .expect("Could not find dashboard_view");
    let dashboard_stack: gtk::Stack = dashboard_builder
        .object("dashboard_mode_stack")
        .expect("Could not find dashboard_mode_stack");
    if let Some(previous_offline) = dashboard_stack.child_by_name("offline") {
        dashboard_stack.remove(&previous_offline);
    }
    let fuel_dashboard = create_dashboard(database_path.clone(), &window);
    dashboard_stack.add_named(&fuel_dashboard, Some("offline"));
    dashboard_container.append(&dashboard_view);
    setup_dashboard_refresh(
        &dashboard_builder,
        realtime_state,
        translation_manager.clone(),
        config::log_database_path(&database_path),
        is_connected.clone(),
        repository
            .as_ref()
            .and_then(|repo| repo.vehicles(false).ok()?.first().map(|v| v.id))
            .unwrap_or(0),
    );
    if let Some(lbl) = builder.object::<Label>("lbl_logs_title") {
        translation_manager.borrow_mut().add(lbl, "Log Analysis");
    }
    if let Some(lbl) = builder.object::<Label>("lbl_maint_title") {
        translation_manager.borrow_mut().add(lbl, "Maintenance");
    }

    // サイドバーの読み込み
    let sidebar_builder = gtk::Builder::from_resource("/com/carlogger/CarLogger/ui/sidebar.ui");
    let sidebar = Sidebar::setup(
        &sidebar_builder,
        main_stack.clone(),
        translation_manager.clone(),
    );
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

    let health_view = HealthView::setup(
        translation_manager.clone(),
        config::log_database_path(&database_path),
        repository
            .as_ref()
            .is_some_and(|repo| repo.is_log_read_only()),
        &window,
        repository
            .as_ref()
            .and_then(|repo| repo.vehicles(false).ok())
            .unwrap_or_default(),
    );
    health_container.append(health_view.widget());

    // 設定画面の読み込み
    let settings_builder =
        gtk::Builder::from_resource("/com/carlogger/CarLogger/ui/settings_view.ui");
    let settings_view = SettingsView::setup(
        &settings_builder,
        translation_manager.clone(),
        repository.clone(),
        update_sender.clone(),
    );
    settings_container.append(settings_view.widget());

    setup_updater_ui(
        &builder,
        &window,
        &main_stack,
        database_path,
        is_connected,
        update_sender,
        update_receiver,
    );

    translation_manager.borrow().update_all();

    window.present();
}

fn setup_updater_ui(
    builder: &gtk::Builder,
    window: &ApplicationWindow,
    main_stack: &Stack,
    database_path: PathBuf,
    important_work: Arc<AtomicBool>,
    sender: mpsc::Sender<updater::UpdateEvent>,
    receiver: mpsc::Receiver<updater::UpdateEvent>,
) {
    let header: GtkBox = builder
        .object("transport_header")
        .expect("Could not find transport_header");
    let indicator = Button::with_label("更新確認中");
    indicator.add_css_class("update-indicator");
    indicator.set_tooltip_text(Some("更新の詳細を表示"));
    header.append(&indicator);

    let last_event = Rc::new(RefCell::new(None::<updater::UpdateEvent>));
    indicator.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        last_event,
        move |_| {
            let event = last_event.borrow();
            let (target, phase, notes) = event
                .as_ref()
                .map(|e| {
                    (
                        e.target_version.as_deref().unwrap_or("—"),
                        update_phase_label(&e.phase),
                        e.notes.as_deref().unwrap_or("リリースノートはありません"),
                    )
                })
                .unwrap_or(("—", "待機中".into(), "更新情報はまだありません"));
            let dialog = MessageDialog::builder()
                .transient_for(&window)
                .modal(true)
                .message_type(gtk::MessageType::Info)
                .text(format!(
                    "APEX//TRACE {} → {target}",
                    updater::current_version()
                ))
                .secondary_text(format!("状態: {phase}\n\n{notes}"))
                .buttons(gtk::ButtonsType::Close)
                .build();
            dialog.connect_response(|dialog, _| dialog.close());
            dialog.present();
        }
    ));

    let countdown = Rc::new(Cell::new(None::<u8>));
    let countdown_ticks = Rc::new(Cell::new(0_u8));
    let restart_dialog = Rc::new(RefCell::new(None::<Dialog>));
    glib::timeout_add_local(
        Duration::from_millis(200),
        glib::clone!(
            #[strong]
            window,
            #[strong]
            indicator,
            #[strong]
            last_event,
            #[strong]
            important_work,
            #[strong]
            countdown,
            #[strong]
            countdown_ticks,
            #[strong]
            restart_dialog,
            #[strong]
            main_stack,
            #[strong]
            database_path,
            move || {
                while let Ok(event) = receiver.try_recv() {
                    indicator.set_sensitive(true);
                    indicator.set_label(&update_phase_label(&event.phase));
                    indicator.set_visible(
                        !matches!(event.phase, updater::UpdatePhase::Idle) || event.manual,
                    );
                    if matches!(event.phase, updater::UpdatePhase::WaitingForSafeExit)
                        && !important_work.load(Ordering::Relaxed)
                    {
                        countdown.set(Some(5));
                        countdown_ticks.set(0);
                        window.add_css_class("update-restart-overlay");
                        let dialog = Dialog::builder()
                            .transient_for(&window)
                            .modal(true)
                            .decorated(false)
                            .default_width(360)
                            .default_height(280)
                            .build();
                        dialog.add_css_class("update-logo-dialog");
                        let content = GtkBox::new(gtk::Orientation::Vertical, 14);
                        content.set_valign(gtk::Align::Center);
                        content.set_halign(gtk::Align::Center);
                        let spinner = gtk::Spinner::new();
                        spinner.set_size_request(150, 150);
                        if gtk::Settings::default()
                            .is_none_or(|settings| settings.is_gtk_enable_animations())
                        {
                            spinner.start();
                        }
                        spinner.add_css_class("update-logo-spinner");
                        let logo = gtk::Image::from_resource(
                            "/com/carlogger/CarLogger/icons/apex-trace.svg",
                        );
                        logo.set_pixel_size(112);
                        let overlay = gtk::Overlay::new();
                        overlay.set_child(Some(&spinner));
                        overlay.add_overlay(&logo);
                        let label = Label::new(Some("安全に保存して更新しています"));
                        label.add_css_class("section-title");
                        content.append(&overlay);
                        content.append(&label);
                        dialog.content_area().append(&content);
                        dialog.present();
                        restart_dialog.replace(Some(dialog));
                    }
                    if let updater::UpdatePhase::Failed(ref error) = event.phase {
                        indicator.add_css_class("update-failed");
                        if event.manual {
                            show_update_message(&window, "更新を確認できませんでした", error);
                        }
                    } else {
                        indicator.remove_css_class("update-failed");
                    }
                    if event.manual && matches!(event.phase, updater::UpdatePhase::Idle) {
                        show_update_message(
                            &window,
                            "最新バージョンです",
                            "利用可能な正式版の更新はありません。",
                        );
                    }
                    last_event.replace(Some(event));
                }
                if let Some(value) = countdown.get() {
                    if important_work.load(Ordering::Relaxed) {
                        countdown.set(None);
                        indicator.set_label("更新待機中");
                        window.remove_css_class("update-restart-overlay");
                        if let Some(dialog) = restart_dialog.borrow_mut().take() {
                            dialog.close();
                        }
                    } else if value > 0 {
                        indicator.set_label(&format!("{value}秒後に再起動"));
                        let ticks = countdown_ticks.get() + 1;
                        countdown_ticks.set(ticks);
                        if ticks >= 5 {
                            countdown_ticks.set(0);
                            countdown.set(Some(value - 1));
                        }
                    } else if let Some(staged) = updater::take_staged() {
                        let state_path = database_path.with_file_name("update-ui-state.json");
                        let page = main_stack
                            .visible_child_name()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "dashboard".into());
                        let _ = updater::save_ui_state(
                            &state_path,
                            &updater::RestorableUiState {
                                page,
                                width: window.width(),
                                height: window.height(),
                            },
                        );
                        indicator.set_label("更新適用中");
                        if let Err(error) = updater::apply_and_restart(staged) {
                            countdown.set(None);
                            window.remove_css_class("update-restart-overlay");
                            show_update_message(&window, "更新に失敗しました", &error);
                            if let Some(dialog) = restart_dialog.borrow_mut().take() {
                                dialog.close();
                            }
                        } else if let Some(app) = window.application() {
                            app.quit();
                        }
                    }
                }
                glib::ControlFlow::Continue
            }
        ),
    );
    updater::spawn_check(false, sender.clone());
    glib::timeout_add_local(Duration::from_secs(24 * 60 * 60), move || {
        updater::spawn_check(false, sender.clone());
        glib::ControlFlow::Continue
    });

    let state_path = database_path.with_file_name("update-ui-state.json");
    if let Some(state) = updater::load_ui_state(&state_path) {
        main_stack.set_visible_child_name(&state.page);
        window.set_default_size(state.width.max(640), state.height.max(480));
    }
}

fn update_phase_label(phase: &updater::UpdatePhase) -> String {
    match phase {
        updater::UpdatePhase::Idle => "最新".into(),
        updater::UpdatePhase::Checking => "更新確認中".into(),
        updater::UpdatePhase::Downloading(progress) => format!("ダウンロード中 {progress}%"),
        updater::UpdatePhase::Verifying => "検証中".into(),
        updater::UpdatePhase::WaitingForSafeExit => "更新待機中".into(),
        updater::UpdatePhase::Failed(_) => "更新失敗".into(),
    }
}

fn show_update_message(window: &ApplicationWindow, title: &str, detail: &str) {
    let dialog = MessageDialog::builder()
        .transient_for(window)
        .modal(true)
        .text(title)
        .secondary_text(detail)
        .buttons(gtk::ButtonsType::Close)
        .build();
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
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
    window: &ApplicationWindow,
    repository: Option<Rc<StorageRepository>>,
    master_database_path: PathBuf,
    log_database_path: PathBuf,
    realtime_state: Arc<RealtimeState>,
    is_connected: Arc<AtomicBool>,
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
    let vehicle_button: Button = builder
        .object("btn_vehicle_profile")
        .expect("Could not find btn_vehicle_profile");
    let vehicle_name: Label = builder
        .object("lbl_vehicle_name")
        .expect("Could not find lbl_vehicle_name");
    let vehicle_detail: Label = builder
        .object("lbl_vehicle_detail")
        .expect("Could not find lbl_vehicle_detail");
    let scan_button = Button::with_label("積極PID探索");
    scan_button.set_sensitive(false);
    if let Some(header) = builder.object::<GtkBox>("transport_header") {
        header.append(&scan_button);
    }
    render_vehicle_header(repository.as_deref(), &vehicle_name, &vehicle_detail);
    vehicle_button.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        repository,
        #[strong]
        vehicle_name,
        #[strong]
        vehicle_detail,
        move |_| {
            show_vehicle_manager(&window, repository.clone(), &vehicle_name, &vehicle_detail);
        }
    ));

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
    let reconnect_attempts = Rc::new(Cell::new(0_u8));
    let connection_lost = Rc::new(Cell::new(false));
    let scan_running = Rc::new(Cell::new(false));
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
            #[strong]
            is_connected,
            #[strong]
            realtime_state,
            #[strong]
            reconnect_attempts,
            #[strong]
            connection_lost,
            #[strong]
            scan_button,
            #[strong]
            scan_running,
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
                        RealtimeLoggingEvent::ConnectionLost(error) => {
                            connection_lost.set(true);
                            status_label.set_text(&format!("切断: {error}"));
                        }
                        RealtimeLoggingEvent::VehicleChanged => {
                            connection_lost.set(true);
                            status_label
                                .set_text("車両識別情報が変化したためセッションを終了しました");
                        }
                        RealtimeLoggingEvent::ScanProgress(progress) => {
                            status_label.set_text(&format!(
                                "PID探索: {}/{} 応答 {} エラー {}",
                                progress.scanned, 33, progress.responses, progress.errors
                            ));
                        }
                        RealtimeLoggingEvent::ScanFinished(progress) => {
                            scan_running.set(false);
                            scan_button.set_label("積極PID探索");
                            status_label.set_text(&format!(
                                "PID探索終了: 探索 {} 応答 {} エラー {}",
                                progress.scanned, progress.responses, progress.errors
                            ));
                        }
                        RealtimeLoggingEvent::SaveError(error) => {
                            status_label.set_text(&format!("Save warning: {error}"));
                        }
                        RealtimeLoggingEvent::Stopped => {
                            active_session.replace(None);
                            is_connected.store(false, Ordering::Relaxed);
                            realtime_state.clear();
                            connect_button.set_label("Connect");
                            scan_button.set_sensitive(false);
                            status_label.set_text("Disconnected");
                            if connection_lost.replace(false) {
                                let attempt = reconnect_attempts.get().saturating_add(1);
                                reconnect_attempts.set(attempt);
                                if attempt <= 3 {
                                    status_label
                                        .set_text(&format!("5秒後に再接続します ({attempt}/3)"));
                                    glib::timeout_add_local_once(
                                        Duration::from_secs(5),
                                        glib::clone!(
                                            #[strong]
                                            connect_button,
                                            #[strong]
                                            status_label,
                                            move || {
                                                status_label
                                                    .set_text(&format!("再接続中 ({attempt}/3)"));
                                                connect_button.emit_clicked();
                                            }
                                        ),
                                    );
                                } else {
                                    status_label
                                        .set_text("再接続に3回失敗しました。手動接続してください");
                                }
                            }
                        }
                    }
                }

                glib::ControlFlow::Continue
            }
        ),
    );

    let auto_target = repository
        .as_ref()
        .and_then(|repo| repo.last_connection_target().ok().flatten())
        .filter(|target| {
            interfaces
                .iter()
                .any(|interface| interface.path == target.adapter)
        });
    if let Some(target) = &auto_target {
        interface_combo.set_active_id(Some(&target.adapter));
        if target.interface == "OBD-2" {
            mode_combo.set_active_id(Some("obd2"));
        }
        status_label.set_text(&format!("自動接続待機中: {}", target.adapter));
    }

    scan_button.connect_clicked(glib::clone!(#[strong] active_session, #[strong] scan_running, #[weak] window, move |button| {
        if scan_running.replace(true) {
            if let Some(session) = active_session.borrow().as_ref() { session.cancel_pid_scan(); }
            button.set_label("停止中…");
            return;
        }
        let confirmation = MessageDialog::builder().transient_for(&window).modal(true).message_type(gtk::MessageType::Warning)
            .text("積極的PID探索を開始しますか？")
            .secondary_text("安全な場所に停車し、必ずパーキング状態で実行してください。読み取り専用サービス01、PID 00〜20を100ms間隔で探索します。いつでも停止できます。")
            .buttons(gtk::ButtonsType::OkCancel).build();
        confirmation.connect_response(glib::clone!(#[strong] active_session, #[strong] scan_running, #[weak] button, move |dialog,response| {
            dialog.close();
            if response == gtk::ResponseType::Ok {
                if let Some(session)=active_session.borrow().as_ref(){ session.start_pid_scan(PidScanConfig::default()); button.set_label("PID探索を停止"); }
                else { scan_running.set(false); }
            } else { scan_running.set(false); }
        }));
        confirmation.present();
    }));

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
        master_database_path,
        #[strong]
        realtime_state,
        #[strong]
        is_connected,
        #[strong]
        reconnect_attempts,
        #[strong]
        connection_lost,
        #[strong]
        vehicle_name,
        #[strong]
        vehicle_detail,
        #[weak]
        window,
        move |_| {
            if let Some(session) = active_session.borrow_mut().take() {
                reconnect_attempts.set(0);
                connection_lost.set(false);
                is_connected.store(false, Ordering::Relaxed);
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
                Ok(mut source) => {
                    let observed = source.vehicle_vin().ok().flatten();
                    let Some(repo) = repository.as_ref() else {
                        status_label.set_text("Database unavailable — logging blocked");
                        return;
                    };
                    let vehicles = repo.vehicles(false).unwrap_or_default();
                    let vehicle_id = match identify_vehicle(observed.as_deref(), &vehicles) {
                        IdentificationOutcome::ExistingVehicle { vehicle_id } => vehicle_id,
                        IdentificationOutcome::SelectionRequired => {
                            match choose_vehicle_without_vin(&window, &vehicles) {
                                VehicleConnectionChoice::Existing(id) => id,
                                VehicleConnectionChoice::New => {
                                    show_vehicle_dialog(
                                        &window,
                                        repository.clone(),
                                        &vehicle_name,
                                        &vehicle_detail,
                                        None,
                                    );
                                    status_label
                                        .set_text("新しい車両の登録完了後に再接続してください");
                                    return;
                                }
                                VehicleConnectionChoice::Cancelled => {
                                    status_label.set_text("車両選択をキャンセルしました");
                                    return;
                                }
                            }
                        }
                        IdentificationOutcome::RegistrationRequired { normalized_vin } => {
                            show_vehicle_dialog(
                                &window,
                                repository.clone(),
                                &vehicle_name,
                                &vehicle_detail,
                                normalized_vin,
                            );
                            status_label
                                .set_text("Vehicle registration required — logging blocked");
                            return;
                        }
                        IdentificationOutcome::InvalidVin(error) => {
                            show_registration_required(&window, &format!("VINが不正です: {error}"));
                            status_label.set_text("Invalid VIN — logging blocked");
                            return;
                        }
                    };
                    let expected_vin = vehicles
                        .iter()
                        .find(|vehicle| vehicle.id == vehicle_id)
                        .and_then(|vehicle| vehicle.normalized_vin.clone());
                    let target = ConnectionTarget {
                        interface: mode_label.clone(),
                        adapter: interface_path.clone(),
                        safe_settings_json: format!(
                            "{{\"mode\":\"{}\"}}",
                            if mode == ConnectionMode::Obd2 {
                                "obd2"
                            } else {
                                "stream"
                            }
                        ),
                    };
                    let now = chrono::Utc::now();
                    let connection_session_id =
                        match repo.start_connection_session(&target, now).and_then(|id| {
                            repo.identify_connection_session(id, vehicle_id, now)
                                .map(|_| id)
                        }) {
                            Ok(id) => id,
                            Err(error) => {
                                status_label.set_text(&format!("Session error: {error}"));
                                return;
                            }
                        };
                    let signal_kind = signal_kind_for_mode(mode);
                    let definitions = repository
                        .as_ref()
                        .and_then(|repository| repository.list_signal_definitions().ok())
                        .map(definition_map)
                        .unwrap_or_default();
                    let session = spawn_realtime_logging(
                        source,
                        RealtimeLoggingConfig {
                            signal_kind,
                            definitions,
                            log_database_path: log_database_path.clone(),
                            master_database_path: master_database_path.clone(),
                            vehicle_id,
                            connection_session_id,
                            expected_vin,
                            realtime_state: realtime_state.clone(),
                            events: event_sender.clone(),
                        },
                    );
                    active_session.replace(Some(session));
                    is_connected.store(true, Ordering::Relaxed);
                    scan_button.set_sensitive(mode == ConnectionMode::Obd2);
                    reconnect_attempts.set(0);
                    connect_button.set_label("Disconnect");
                    status_label.set_text(&format!("Connected: {interface_label} / {mode_label}"));
                }
                Err(error) => {
                    active_session.replace(None);
                    is_connected.store(false, Ordering::Relaxed);
                    connect_button.set_label("Connect");
                    status_label.set_text(&format!("Connection failed: {error}"));
                    let attempt = reconnect_attempts.get();
                    if attempt > 0 && attempt < 3 {
                        let next = attempt + 1;
                        reconnect_attempts.set(next);
                        status_label
                            .set_text(&format!("再接続失敗。5秒後に再試行します ({next}/3)"));
                        glib::timeout_add_local_once(
                            Duration::from_secs(5),
                            glib::clone!(
                                #[strong]
                                connect_button,
                                move || connect_button.emit_clicked()
                            ),
                        );
                    } else if attempt >= 3 {
                        status_label.set_text("再接続に3回失敗しました。手動接続してください");
                    }
                }
            }
        }
    ));
    if auto_target.is_some() {
        glib::idle_add_local_once(glib::clone!(
            #[strong]
            connect_button,
            #[strong]
            status_label,
            move || {
                status_label.set_text("最後に成功した接続先へ自動接続中（キャンセルは切断ボタン）");
                connect_button.emit_clicked();
            }
        ));
    }
}

enum VehicleConnectionChoice {
    Existing(i64),
    New,
    Cancelled,
}

fn choose_vehicle_without_vin(
    window: &ApplicationWindow,
    vehicles: &[car_logger_domain::Vehicle],
) -> VehicleConnectionChoice {
    let dialog = Dialog::builder()
        .title("VINを取得できませんでした")
        .transient_for(window)
        .modal(true)
        .default_width(460)
        .build();
    dialog.add_button("キャンセル", gtk::ResponseType::Cancel);
    dialog.add_button("新しい車両として登録", gtk::ResponseType::Other(1));
    dialog.add_button("選択した車両として接続", gtk::ResponseType::Accept);
    let content = GtkBox::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    let explanation = Label::new(Some(
        "VINから車両を一意に判断できないため、推測では紐付けません。登録済み車両を明示的に選択するか、新規登録してください。",
    ));
    explanation.set_wrap(true);
    content.append(&explanation);
    let selector = ComboBoxText::new();
    for vehicle in vehicles {
        selector.append(Some(&vehicle.id.to_string()), &vehicle.display_name);
    }
    selector.set_active((!vehicles.is_empty()).then_some(0));
    content.append(&selector);
    dialog.content_area().append(&content);

    let response = Rc::new(Cell::<i32>::new(gtk::ResponseType::None.into()));
    let event_loop = glib::MainLoop::new(None, false);
    dialog.connect_response(glib::clone!(
        #[strong]
        response,
        #[strong]
        event_loop,
        move |dialog, value| {
            response.set(value.into());
            dialog.close();
            event_loop.quit();
        }
    ));
    dialog.present();
    event_loop.run();
    match gtk::ResponseType::from(response.get()) {
        gtk::ResponseType::Accept => selector
            .active_id()
            .and_then(|id| id.parse().ok())
            .map(VehicleConnectionChoice::Existing)
            .unwrap_or(VehicleConnectionChoice::Cancelled),
        gtk::ResponseType::Other(1) => VehicleConnectionChoice::New,
        _ => VehicleConnectionChoice::Cancelled,
    }
}

fn show_registration_required(window: &ApplicationWindow, detail: &str) {
    let dialog = MessageDialog::builder()
        .transient_for(window)
        .modal(true)
        .message_type(gtk::MessageType::Warning)
        .text("車両の確定が必要です")
        .secondary_text(detail)
        .buttons(gtk::ButtonsType::Close)
        .build();
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
}

fn render_vehicle_header(repository: Option<&StorageRepository>, name: &Label, detail: &Label) {
    let vehicles = repository
        .and_then(|repo| repo.vehicles(false).ok())
        .unwrap_or_default();
    match vehicles.first() {
        Some(vehicle) => {
            name.set_text(&vehicle.display_name);
            let year = vehicle
                .model_year
                .map(|x| x.to_string())
                .unwrap_or_else(|| "—".into());
            let vin = vehicle
                .normalized_vin
                .as_deref()
                .map(|x| format!("VIN …{}", &x[11..]))
                .unwrap_or_else(|| "VIN未登録".into());
            detail.set_text(&format!(
                "{} {} · {} · {}",
                vehicle.manufacturer.as_deref().unwrap_or("—"),
                vehicle.model.as_deref().unwrap_or("—"),
                year,
                vin
            ));
            if vehicles.len() > 1 {
                detail.set_text(&format!(
                    "{} · 登録車両 {}台",
                    detail.text(),
                    vehicles.len()
                ));
            }
        }
        None => {
            name.set_text("車両未設定");
            detail.set_text("クリックして車両情報を設定");
        }
    }
}

fn show_vehicle_dialog(
    window: &ApplicationWindow,
    repository: Option<Rc<StorageRepository>>,
    header_name: &Label,
    header_detail: &Label,
    prefill_vin: Option<String>,
) {
    let dialog = Dialog::builder()
        .title("新しい車両を登録")
        .transient_for(window)
        .modal(true)
        .default_width(440)
        .build();
    dialog.add_button("キャンセル", gtk::ResponseType::Cancel);
    dialog.add_button("保存", gtk::ResponseType::Accept);
    let form = GtkBox::new(gtk::Orientation::Vertical, 8);
    form.set_margin_top(12);
    form.set_margin_bottom(12);
    form.set_margin_start(12);
    form.set_margin_end(12);
    let display_name = vehicle_entry(&form, "表示名（必須）", "");
    let manufacturer = vehicle_entry(&form, "メーカー", "");
    let model = vehicle_entry(&form, "車種", "");
    let year_label = Label::new(Some("年式"));
    year_label.set_halign(gtk::Align::Start);
    form.append(&year_label);
    let year = SpinButton::with_range(1886.0, 9999.0, 1.0);
    year.set_value(2026.0);
    form.append(&year);
    let fuel_label = Label::new(Some("燃料種別（必須）"));
    fuel_label.set_halign(gtk::Align::Start);
    form.append(&fuel_label);
    let fuel = ComboBoxText::new();
    for (id, label) in [
        ("gasoline", "ガソリン"),
        ("diesel", "ディーゼル"),
        ("hybrid", "ハイブリッド"),
        ("electric", "電気"),
        ("other", "その他"),
    ] {
        fuel.append(Some(id), label);
    }
    fuel.set_active_id(Some("gasoline"));
    form.append(&fuel);
    let displacement_label = Label::new(Some("排気量 L（必須）"));
    displacement_label.set_halign(gtk::Align::Start);
    form.append(&displacement_label);
    let displacement = SpinButton::with_range(0.1, 20.0, 0.1);
    displacement.set_value(2.0);
    form.append(&displacement);
    let tank_label = Label::new(Some("燃料タンク容量 L（必須）"));
    tank_label.set_halign(gtk::Align::Start);
    form.append(&tank_label);
    let tank = SpinButton::with_range(0.1, 500.0, 0.1);
    tank.set_value(50.0);
    form.append(&tank);
    let vin = vehicle_entry(
        &form,
        "VIN（車両変更検知に使用する17桁）",
        prefill_vin.as_deref().unwrap_or(""),
    );
    let note = Label::new(Some(
        "VINは照合だけに使用し、AI入力やエクスポートには含めません。VIN登録後は接続車両を確認できない場合もログ取得を停止します。",
    ));
    note.set_wrap(true);
    note.add_css_class("muted-label");
    form.append(&note);
    dialog.content_area().append(&form);
    dialog.connect_response(glib::clone!(
        #[strong]
        repository,
        #[strong]
        header_name,
        #[strong]
        header_detail,
        move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                if let Some(repo) = &repository {
                    let profile = NewVehicle {
                        display_name: display_name.text().to_string(),
                        manufacturer: (!manufacturer.text().trim().is_empty())
                            .then(|| manufacturer.text().to_string()),
                        model: (!model.text().trim().is_empty()).then(|| model.text().to_string()),
                        model_year: Some(year.value_as_int() as u16),
                        vin: (!vin.text().trim().is_empty()).then(|| vin.text().to_string()),
                        fuel_type: match fuel.active_id().as_deref() {
                            Some("diesel") => FuelType::Diesel,
                            Some("hybrid") => FuelType::Hybrid,
                            Some("electric") => FuelType::Electric,
                            Some("other") => FuelType::Other,
                            _ => FuelType::Gasoline,
                        },
                        displacement_l: displacement.value(),
                        tank_capacity_l: tank.value(),
                        engine: None,
                        odometer_km: None,
                        notes: None,
                    };
                    match repo.create_vehicle(&profile, chrono::Utc::now()) {
                        Ok(_) => {
                            render_vehicle_header(Some(repo), &header_name, &header_detail);
                            dialog.close();
                        }
                        Err(error) => {
                            let warning = MessageDialog::builder()
                                .transient_for(dialog)
                                .modal(true)
                                .message_type(gtk::MessageType::Error)
                                .text("車両情報を保存できません")
                                .secondary_text(error.to_string())
                                .buttons(gtk::ButtonsType::Close)
                                .build();
                            warning.connect_response(|d, _| d.close());
                            warning.present();
                        }
                    }
                }
            } else {
                dialog.close();
            }
        }
    ));
    dialog.present();
}

fn show_vehicle_manager(
    window: &ApplicationWindow,
    repository: Option<Rc<StorageRepository>>,
    header_name: &Label,
    header_detail: &Label,
) {
    let dialog = Dialog::builder()
        .title("車両管理")
        .transient_for(window)
        .modal(true)
        .default_width(620)
        .default_height(480)
        .build();
    dialog.add_button("閉じる", gtk::ResponseType::Close);
    let content = GtkBox::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    let add = Button::with_label("新しい車両を登録");
    content.append(&add);
    let Some(repo) = repository.as_ref() else {
        content.append(&Label::new(Some("データベースを利用できません")));
        dialog.content_area().append(&content);
        dialog.present();
        return;
    };
    match repo.vehicles(true) {
        Ok(vehicles) => {
            for vehicle in vehicles {
                let row = GtkBox::new(gtk::Orientation::Horizontal, 10);
                let text = if let Some(deleted) = vehicle.deleted_at {
                    format!(
                        "{}  削除: {}  完全削除予定: {}",
                        vehicle.display_name,
                        deleted.format("%Y-%m-%d"),
                        vehicle
                            .purge_after
                            .map(|v| v.format("%Y-%m-%d").to_string())
                            .unwrap_or_default()
                    )
                } else {
                    format!(
                        "{}  {}",
                        vehicle.display_name,
                        vehicle.normalized_vin.as_deref().unwrap_or("VIN未取得")
                    )
                };
                let label = Label::new(Some(&text));
                label.set_hexpand(true);
                label.set_halign(gtk::Align::Start);
                row.append(&label);
                if vehicle.deleted_at.is_some() {
                    let restore = Button::with_label("復元");
                    restore.connect_clicked(glib::clone!(
                        #[strong]
                        repository,
                        #[strong]
                        header_name,
                        #[strong]
                        header_detail,
                        #[weak]
                        dialog,
                        move |_| {
                            if let Some(repo) = &repository {
                                let _ = repo.restore_vehicle(vehicle.id, chrono::Utc::now());
                                render_vehicle_header(Some(repo), &header_name, &header_detail);
                            }
                            dialog.close();
                        }
                    ));
                    row.append(&restore);
                    let purge = Button::with_label("完全削除");
                    purge.add_css_class("destructive-action");
                    let vehicle_name = vehicle.display_name.clone();
                    purge.connect_clicked(glib::clone!(
                        #[strong]
                        repository,
                        #[strong]
                        header_name,
                        #[strong]
                        header_detail,
                        #[weak]
                        window,
                        #[weak]
                        dialog,
                        move |_| {
                            if confirm_permanent_vehicle_delete(&window, &vehicle_name) {
                                if let Some(repo) = &repository {
                                    let _ =
                                        repo.permanently_delete_vehicle(vehicle.id, &vehicle_name);
                                    render_vehicle_header(Some(repo), &header_name, &header_detail);
                                }
                                dialog.close();
                            }
                        }
                    ));
                    row.append(&purge);
                } else {
                    let remove = Button::with_label("削除");
                    remove.connect_clicked(glib::clone!(
                        #[strong]
                        repository,
                        #[strong]
                        header_name,
                        #[strong]
                        header_detail,
                        #[weak]
                        dialog,
                        move |_| {
                            if let Some(repo) = &repository {
                                let _ = repo.soft_delete_vehicle(vehicle.id, chrono::Utc::now());
                                render_vehicle_header(Some(repo), &header_name, &header_detail);
                            }
                            dialog.close();
                        }
                    ));
                    row.append(&remove);
                }
                content.append(&row);
            }
        }
        Err(error) => content.append(&Label::new(Some(&format!(
            "車両一覧を読み込めません: {error}"
        )))),
    }
    add.connect_clicked(glib::clone!(
        #[strong]
        repository,
        #[strong]
        header_name,
        #[strong]
        header_detail,
        #[weak]
        window,
        #[weak]
        dialog,
        move |_| {
            dialog.close();
            show_vehicle_dialog(
                &window,
                repository.clone(),
                &header_name,
                &header_detail,
                None,
            );
        }
    ));
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.content_area().append(&content);
    dialog.present();
}

fn confirm_permanent_vehicle_delete(window: &ApplicationWindow, vehicle_name: &str) -> bool {
    let dialog = Dialog::builder()
        .title("車両を完全削除")
        .transient_for(window)
        .modal(true)
        .build();
    dialog.add_button("キャンセル", gtk::ResponseType::Cancel);
    dialog.add_button("完全削除", gtk::ResponseType::Accept);
    let box_ = GtkBox::new(gtk::Orientation::Vertical, 10);
    box_.set_margin_top(16);
    box_.set_margin_bottom(16);
    box_.set_margin_start(16);
    box_.set_margin_end(16);
    let warning = Label::new(Some(
        "車両本体、ログ、健康・診断結果、PID、CAN ID、信号定義、全CANフレームを削除します。取り消せません。確認のため車両名を入力してください。",
    ));
    warning.set_wrap(true);
    box_.append(&warning);
    let entry = Entry::new();
    entry.set_placeholder_text(Some(vehicle_name));
    box_.append(&entry);
    dialog.content_area().append(&box_);
    let response = Rc::new(Cell::<i32>::new(gtk::ResponseType::None.into()));
    let loop_ = glib::MainLoop::new(None, false);
    dialog.connect_response(glib::clone!(
        #[strong]
        response,
        #[strong]
        loop_,
        move |dialog, value| {
            response.set(value.into());
            dialog.close();
            loop_.quit();
        }
    ));
    dialog.present();
    loop_.run();
    gtk::ResponseType::from(response.get()) == gtk::ResponseType::Accept
        && entry.text().as_str() == vehicle_name
}

fn vehicle_entry(form: &GtkBox, label: &str, value: &str) -> Entry {
    let l = Label::new(Some(label));
    l.set_halign(gtk::Align::Start);
    form.append(&l);
    let entry = Entry::new();
    entry.set_text(value);
    form.append(&entry);
    entry
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
