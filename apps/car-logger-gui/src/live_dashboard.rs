use std::cell::Cell;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use car_logger_application::{
    DiagnosticDashboardData, DiagnosticRepository, HealthDashboardData, HealthService,
    RealtimeSignalState, RealtimeState, ScoreDomain, ScoreGranularity, ScoreStatus,
};
use car_logger_storage::DuckdbCanFrameRepository;
use chrono::{Local, TimeDelta, Utc};
use crossbeam_channel::unbounded;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Grid, Label, Orientation, ProgressBar, glib};

use crate::localization::translate;
use crate::signal_decoder::find_metric;
use crate::ui::TranslationManager;

struct MetricBinding {
    label_id: &'static str,
    names: &'static [&'static str],
    fallback_unit: &'static str,
}

const TEXT_METRICS: &[MetricBinding] = &[
    MetricBinding {
        label_id: "metric_rpm",
        names: &["engine rpm"],
        fallback_unit: "rpm",
    },
    MetricBinding {
        label_id: "metric_speed",
        names: &["vehicle speed"],
        fallback_unit: "km/h",
    },
    MetricBinding {
        label_id: "metric_intake_pressure",
        names: &["intake manifold absolute pressure"],
        fallback_unit: "kPa",
    },
    MetricBinding {
        label_id: "metric_maf",
        names: &["mass air flow rate"],
        fallback_unit: "g/s",
    },
    MetricBinding {
        label_id: "metric_timing_advance",
        names: &["timing advance"],
        fallback_unit: "deg",
    },
    MetricBinding {
        label_id: "metric_voltage",
        names: &["control module voltage"],
        fallback_unit: "V",
    },
    MetricBinding {
        label_id: "metric_coolant",
        names: &["engine coolant temperature"],
        fallback_unit: "degC",
    },
    MetricBinding {
        label_id: "metric_intake_temp",
        names: &["intake air temperature"],
        fallback_unit: "degC",
    },
    MetricBinding {
        label_id: "metric_ambient_temp",
        names: &["ambient air temperature"],
        fallback_unit: "degC",
    },
    MetricBinding {
        label_id: "metric_oil_temp",
        names: &["engine oil temperature"],
        fallback_unit: "degC",
    },
    MetricBinding {
        label_id: "metric_catalyst_temp",
        names: &["catalyst temperature"],
        fallback_unit: "degC",
    },
    MetricBinding {
        label_id: "metric_distance_mil",
        names: &["distance with mil on"],
        fallback_unit: "km",
    },
    MetricBinding {
        label_id: "metric_distance_dtc",
        names: &["distance since dtcs cleared"],
        fallback_unit: "km",
    },
    MetricBinding {
        label_id: "metric_run_time",
        names: &["run time since engine start"],
        fallback_unit: "s",
    },
];

struct RatioMetric {
    title: &'static str,
    names: &'static [&'static str],
    min: f64,
    max: f64,
}

const RATIO_METRICS: &[RatioMetric] = &[
    RatioMetric {
        title: "Engine load",
        names: &["calculated engine load"],
        min: 0.0,
        max: 100.0,
    },
    RatioMetric {
        title: "Throttle",
        names: &["throttle position"],
        min: 0.0,
        max: 100.0,
    },
    RatioMetric {
        title: "Accelerator",
        names: &["accelerator pedal position d"],
        min: 0.0,
        max: 100.0,
    },
    RatioMetric {
        title: "Commanded throttle",
        names: &["commanded throttle actuator"],
        min: 0.0,
        max: 100.0,
    },
    RatioMetric {
        title: "Short fuel trim",
        names: &["short term fuel trim"],
        min: -100.0,
        max: 100.0,
    },
    RatioMetric {
        title: "Long fuel trim",
        names: &["long term fuel trim"],
        min: -100.0,
        max: 100.0,
    },
    RatioMetric {
        title: "Fuel level",
        names: &["fuel tank level input"],
        min: 0.0,
        max: 100.0,
    },
];

#[derive(Clone)]
struct RatioChart {
    metric: &'static RatioMetric,
    progress: ProgressBar,
    value_label: Label,
    current: Rc<Cell<f64>>,
    target: Rc<Cell<f64>>,
}

const DASHBOARD_LABELS: &[(&str, &str)] = &[
    ("lbl_dashboard_subtitle", "Realtime vehicle telemetry"),
    ("lbl_dashboard_live", "● LIVE"),
    ("lbl_dashboard_engine", "ENGINE"),
    ("lbl_dashboard_speed", "Vehicle speed"),
    ("lbl_dashboard_intake_pressure", "Intake pressure"),
    ("lbl_dashboard_timing", "Timing advance"),
    ("lbl_dashboard_voltage", "Voltage"),
    ("lbl_dashboard_ratios", "LIVE RATIOS"),
    ("lbl_dashboard_temperatures", "TEMPERATURES"),
    ("lbl_dashboard_coolant", "Coolant"),
    ("lbl_dashboard_intake_air", "Intake air"),
    ("lbl_dashboard_ambient", "Ambient"),
    ("lbl_dashboard_oil", "Engine oil"),
    ("lbl_dashboard_catalyst", "Catalyst"),
    ("lbl_dashboard_trip", "TRIP & DIAGNOSTICS"),
    ("lbl_dashboard_mil_distance", "MIL distance"),
    ("lbl_dashboard_dtc_distance", "DTC cleared distance"),
    ("lbl_dashboard_run_time", "Engine run time"),
    ("lbl_dashboard_known", "Known Signals"),
    ("lbl_dashboard_signal", "Signal"),
    ("lbl_dashboard_value", "Value"),
    ("lbl_dashboard_source", "Source"),
    ("lbl_dashboard_unknown", "Unknown CAN IDs"),
    ("lbl_dashboard_can_id", "CAN ID"),
    ("lbl_dashboard_payload", "Payload"),
    ("lbl_dashboard_count", "Count"),
];

pub fn setup_dashboard_refresh(
    builder: &gtk::Builder,
    realtime_state: Arc<RealtimeState>,
    translation_manager: Rc<RefCell<TranslationManager>>,
    log_database_path: PathBuf,
    is_connected: Arc<AtomicBool>,
    vehicle_id: i64,
) {
    setup_offline_summary(builder, log_database_path, vehicle_id);
    let mode_stack: gtk::Stack = builder
        .object("dashboard_mode_stack")
        .expect("Could not find dashboard_mode_stack");
    mode_stack.set_visible_child_name("offline");
    {
        let mut tm = translation_manager.borrow_mut();
        for (label_id, msgid) in DASHBOARD_LABELS {
            let label: Label = builder
                .object(*label_id)
                .unwrap_or_else(|| panic!("Could not find {label_id}"));
            tm.add(label, msgid);
        }
    }
    let last_seen_label: Label = builder
        .object("lbl_last_seen")
        .expect("Could not find lbl_last_seen");
    let metric_labels = TEXT_METRICS
        .iter()
        .map(|metric| {
            let label: Label = builder
                .object(metric.label_id)
                .unwrap_or_else(|| panic!("Could not find {}", metric.label_id));
            (metric, label)
        })
        .collect::<Vec<_>>();
    let ratio_container: GtkBox = builder
        .object("ratio_chart_container")
        .expect("Could not find ratio_chart_container");
    let ratio_charts = RATIO_METRICS
        .iter()
        .map(|metric| create_ratio_chart(&ratio_container, metric, &translation_manager))
        .collect::<Vec<_>>();
    let known_signal_table: Grid = builder
        .object("known_signal_table")
        .expect("Could not find known_signal_table");
    let unknown_can_table: Grid = builder
        .object("unknown_can_table")
        .expect("Could not find unknown_can_table");
    let engine_card: GtkBox = builder
        .object("engine_card")
        .expect("Could not find engine_card");
    let smoothed_rpm = Rc::new(Cell::new(0.0));

    glib::timeout_add_local(
        Duration::from_millis(250),
        glib::clone!(
            #[strong]
            last_seen_label,
            #[strong]
            metric_labels,
            #[strong]
            ratio_charts,
            #[strong]
            known_signal_table,
            #[strong]
            unknown_can_table,
            #[strong]
            engine_card,
            #[strong]
            smoothed_rpm,
            #[strong]
            mode_stack,
            #[strong]
            is_connected,
            move || {
                mode_stack.set_visible_child_name(if is_connected.load(Ordering::Relaxed) {
                    "live"
                } else {
                    "offline"
                });
                let snapshot = realtime_state
                    .snapshot()
                    .into_iter()
                    .map(|(_, state)| state)
                    .collect::<Vec<_>>();

                for (metric, label) in &metric_labels {
                    update_metric_label(label, &snapshot, metric.names, metric.fallback_unit);
                }
                update_ratio_charts(&ratio_charts, &snapshot);
                update_last_seen_label(&last_seen_label, &snapshot);
                update_known_signal_table(&known_signal_table, &snapshot);
                update_unknown_can_table(&unknown_can_table, &snapshot);
                update_engine_accent(&engine_card, &smoothed_rpm, &snapshot);

                glib::ControlFlow::Continue
            }
        ),
    );

    glib::timeout_add_local(
        Duration::from_millis(16),
        glib::clone!(
            #[strong]
            ratio_charts,
            move || {
                for chart in &ratio_charts {
                    let current = chart.current.get();
                    let target = chart.target.get();
                    let next = current + (target - current) * 0.14;
                    chart.current.set(next);
                    chart.progress.set_fraction(next.clamp(0.0, 1.0));
                }
                glib::ControlFlow::Continue
            }
        ),
    );
}

struct OfflineData {
    health: HealthDashboardData,
    diagnostics: DiagnosticDashboardData,
}

fn setup_offline_summary(builder: &gtk::Builder, path: PathBuf, vehicle_id: i64) {
    let score: Label = builder
        .object("offline_health_score")
        .expect("offline_health_score");
    let state: Label = builder
        .object("offline_health_state")
        .expect("offline_health_state");
    let meta: Label = builder
        .object("offline_health_meta")
        .expect("offline_health_meta");
    let diagnostic_state: Label = builder
        .object("offline_diagnostic_state")
        .expect("offline_diagnostic_state");
    let diagnostic_meta: Label = builder
        .object("offline_diagnostic_meta")
        .expect("offline_diagnostic_meta");
    let learning: Label = builder
        .object("offline_learning")
        .expect("offline_learning");
    let domains = [
        (
            ScoreDomain::Thermal,
            builder.object::<Label>("offline_domain_thermal").unwrap(),
        ),
        (
            ScoreDomain::Electrical,
            builder
                .object::<Label>("offline_domain_electrical")
                .unwrap(),
        ),
        (
            ScoreDomain::AirFuel,
            builder.object::<Label>("offline_domain_air_fuel").unwrap(),
        ),
        (
            ScoreDomain::RunningStability,
            builder.object::<Label>("offline_domain_stability").unwrap(),
        ),
    ];
    let (sender, receiver) = unbounded();
    thread::spawn(move || {
        let result = (|| {
            let mut repository = DuckdbCanFrameRepository::open_read_only(path)?;
            repository.select_vehicle(vehicle_id);
            let diagnostics = repository.diagnostic_dashboard(20)?;
            let now = Utc::now();
            let health = HealthService::new(repository).dashboard(
                ScoreGranularity::Day,
                now - TimeDelta::days(30),
                now,
                31,
            )?;
            anyhow::Ok(OfflineData {
                health,
                diagnostics,
            })
        })()
        .map_err(|error: anyhow::Error| error.to_string());
        let _ = sender.send(result);
    });
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let Ok(result) = receiver.try_recv() else {
            return glib::ControlFlow::Continue;
        };
        match result {
            Ok(data) => render_offline_summary(
                &score,
                &state,
                &meta,
                &diagnostic_state,
                &diagnostic_meta,
                &learning,
                &domains,
                &data,
            ),
            Err(error) => {
                state.set_text(&translate("Vehicle history unavailable"));
                meta.set_text(&error);
                diagnostic_state.set_text(&translate("Diagnostics unavailable"));
                learning.set_text(&translate("No learning data available"));
            }
        }
        glib::ControlFlow::Break
    });
}

#[allow(clippy::too_many_arguments)]
fn render_offline_summary(
    score: &Label,
    state: &Label,
    meta: &Label,
    diagnostic_state: &Label,
    diagnostic_meta: &Label,
    learning: &Label,
    domains: &[(ScoreDomain, Label)],
    data: &OfflineData,
) {
    if let Some(latest) = &data.health.latest {
        score.set_text(
            &latest
                .score
                .map(|value| format!("{value:.0}"))
                .unwrap_or_else(|| "—".into()),
        );
        let state_text = match (latest.status, latest.score) {
            (ScoreStatus::Scored, Some(value)) if value < 40.0 => "Needs attention",
            (ScoreStatus::Scored, Some(value)) if value < 70.0 => "Watch",
            (ScoreStatus::Scored, Some(_)) => "Healthy",
            (ScoreStatus::Scored, None) => "No data",
            (ScoreStatus::Learning, _) => "Learning",
            (ScoreStatus::InsufficientData, _) => "Insufficient data",
            (ScoreStatus::NoData, _) => "No data",
            (ScoreStatus::CalculationFailed, _) => "Calculation failed",
        };
        state.set_text(&translate(state_text));
        let difference = data
            .health
            .previous
            .as_ref()
            .and_then(|previous| previous.score)
            .zip(latest.score)
            .map(|(previous, current)| {
                format!(" · {} {:+.1}", translate("vs previous"), current - previous)
            })
            .unwrap_or_default();
        meta.set_text(&format!(
            "{} {:.0}%{} · {} {}",
            translate("Confidence"),
            latest.confidence,
            difference,
            translate("Last updated"),
            latest
                .calculated_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
        ));
        learning.set_text(&format!(
            "{}: {} · {}: {:.1}h · {}: {:.0}%",
            translate("Valid sessions"),
            latest.session_count,
            translate("Evaluated time"),
            latest.evaluated_seconds / 3600.0,
            translate("Data coverage"),
            latest.coverage * 100.0
        ));
    } else {
        state.set_text(&translate("No data"));
        meta.set_text(&translate(
            "Drive with logging enabled to build a health score.",
        ));
        learning.set_text(&translate("No learning data available"));
    }
    for (domain, label) in domains {
        let value = data
            .health
            .components
            .iter()
            .find(|component| component.domain == *domain)
            .and_then(|component| component.score);
        label.set_text(
            &value
                .map(|value| format!("{value:.0}"))
                .unwrap_or_else(|| "—".into()),
        );
    }
    let diagnostics = &data.diagnostics;
    diagnostic_state.set_text(&match diagnostics.mil_on {
        Some(true) => format!("MIL ON · {} DTC", diagnostics.active.len()),
        Some(false) => format!("MIL OFF · {} DTC", diagnostics.active.len()),
        None => translate("Not observed yet"),
    });
    diagnostic_meta.set_text(
        &diagnostics
            .last_observed_at
            .map(|at| {
                format!(
                    "{} {}",
                    translate("Last acquisition"),
                    at.with_timezone(&Local).format("%Y-%m-%d %H:%M")
                )
            })
            .unwrap_or_else(|| translate("No diagnostic observation yet")),
    );
}

fn update_engine_accent(
    engine_card: &GtkBox,
    smoothed_rpm: &Cell<f64>,
    snapshot: &[RealtimeSignalState],
) {
    let target = find_metric(snapshot, &["engine rpm"])
        .map(|value| value.value.max(0.0))
        .unwrap_or(0.0);
    let rpm = smoothed_rpm.get() + (target - smoothed_rpm.get()) * 0.22;
    smoothed_rpm.set(rpm);

    for class in ["rpm-idle", "rpm-cruise", "rpm-high"] {
        engine_card.remove_css_class(class);
    }
    engine_card.add_css_class(if rpm >= 4_500.0 {
        "rpm-high"
    } else if rpm >= 2_000.0 {
        "rpm-cruise"
    } else {
        "rpm-idle"
    });
}

fn create_ratio_chart(
    container: &GtkBox,
    metric: &'static RatioMetric,
    translation_manager: &Rc<RefCell<TranslationManager>>,
) -> RatioChart {
    let row = GtkBox::new(Orientation::Vertical, 5);
    row.add_css_class("ratio-chart");

    let header = GtkBox::new(Orientation::Horizontal, 8);
    let title = Label::new(Some(&translate(metric.title)));
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    title.add_css_class("ratio-chart-label");
    translation_manager
        .borrow_mut()
        .add(title.clone(), metric.title);
    let value_label = Label::new(Some("-- %"));
    value_label.set_halign(gtk::Align::End);
    value_label.add_css_class("ratio-chart-value");
    header.append(&title);
    header.append(&value_label);

    let progress = ProgressBar::new();
    progress.set_hexpand(true);
    progress.add_css_class("ratio-progress");
    row.append(&header);
    row.append(&progress);
    container.append(&row);

    RatioChart {
        metric,
        progress,
        value_label,
        current: Rc::new(Cell::new(0.0)),
        target: Rc::new(Cell::new(0.0)),
    }
}

fn update_ratio_charts(charts: &[RatioChart], snapshot: &[RealtimeSignalState]) {
    for chart in charts {
        if let Some(value) = find_metric(snapshot, chart.metric.names) {
            let range = chart.metric.max - chart.metric.min;
            chart
                .target
                .set(((value.value - chart.metric.min) / range).clamp(0.0, 1.0));
            chart.value_label.set_text(&format!("{:.1} %", value.value));
        } else {
            chart.target.set(0.0);
            chart.value_label.set_text("-- %");
        }
    }
}

fn update_metric_label(
    label: &Label,
    snapshot: &[RealtimeSignalState],
    names: &[&str],
    fallback_unit: &str,
) {
    if let Some(value) = find_metric(snapshot, names) {
        let unit = value.unit.as_deref().unwrap_or(fallback_unit);
        label.set_text(&format!("{:.1} {unit}", value.value));
    } else {
        label.set_text(&format!("-- {fallback_unit}"));
    }
}

fn update_last_seen_label(label: &Label, snapshot: &[RealtimeSignalState]) {
    let latest = snapshot.iter().map(|state| state.last_seen).max();
    if let Some(latest) = latest {
        label.set_text(&latest.format("%H:%M:%S%.3f UTC").to_string());
    } else {
        label.set_text(&translate("No frames"));
    }
}

fn update_known_signal_table(table: &Grid, snapshot: &[RealtimeSignalState]) {
    clear_grid_rows(table);

    let mut row = 1;
    for state in snapshot.iter().filter(|state| state.is_known).take(20) {
        for decoded in &state.decoded_values {
            table.attach(&table_label(&decoded.name, gtk::Align::Start), 0, row, 1, 1);
            table.attach(
                &table_label(
                    &format!(
                        "{:.2}{}",
                        decoded.value,
                        decoded
                            .unit
                            .as_ref()
                            .map(|unit| format!(" {unit}"))
                            .unwrap_or_default()
                    ),
                    gtk::Align::End,
                ),
                1,
                row,
                1,
                1,
            );
            table.attach(
                &table_label(
                    &format!("0x{:03X}", state.latest_frame.id),
                    gtk::Align::Start,
                ),
                2,
                row,
                1,
                1,
            );
            row += 1;
        }
    }

    if row == 1 {
        table.attach(
            &table_label(
                &translate("Waiting for decoded CAN/PID values"),
                gtk::Align::Start,
            ),
            0,
            row,
            3,
            1,
        );
    }
}

fn update_unknown_can_table(table: &Grid, snapshot: &[RealtimeSignalState]) {
    clear_grid_rows(table);

    let mut row = 1;
    for state in snapshot.iter().filter(|state| !state.is_known).take(20) {
        table.attach(
            &table_label(
                &format!("0x{:03X}", state.latest_frame.id),
                gtk::Align::Start,
            ),
            0,
            row,
            1,
            1,
        );
        table.attach(
            &table_label(&format_payload(&state.latest_frame.data), gtk::Align::Start),
            1,
            row,
            1,
            1,
        );
        table.attach(
            &table_label(&state.count.to_string(), gtk::Align::End),
            2,
            row,
            1,
            1,
        );
        row += 1;
    }

    if row == 1 {
        table.attach(
            &table_label(&translate("No unknown frames"), gtk::Align::Start),
            0,
            row,
            3,
            1,
        );
    }
}

fn clear_grid_rows(grid: &Grid) {
    let mut child = grid.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if grid.child_at(0, 0).as_ref() != Some(&widget)
            && grid.child_at(1, 0).as_ref() != Some(&widget)
            && grid.child_at(2, 0).as_ref() != Some(&widget)
        {
            grid.remove(&widget);
        }
    }
}

fn table_label(text: &str, align: gtk::Align) -> Label {
    let label = Label::new(Some(text));
    label.set_halign(align);
    label.add_css_class("table-cell");
    label
}

fn format_payload(payload: &[u8]) -> String {
    payload
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
