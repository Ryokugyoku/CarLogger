use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use car_logger_application::{
    DiagnosticDashboardData, DiagnosticRepository, HealthDashboardData, HealthService,
    ScoreGranularity, ScoreStatus, StoredComponent, StoredHealthScore,
};
use car_logger_storage::DuckdbCanFrameRepository;
use chrono::{DateTime, Local, TimeDelta, Utc};
use crossbeam_channel::{Receiver, Sender, unbounded};
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, DrawingArea, FlowBox, Label, MessageDialog, Orientation,
    ProgressBar, ScrolledWindow, ToggleButton, glib,
};

use crate::localization::translate;
use crate::ui::TranslationManager;
use crate::ui::ai_condition::AiConditionPanel;

const MAX_GRAPH_POINTS: usize = 180;
const RECALCULATE_CHUNK_SIZE: usize = 20_000;

#[derive(Debug, Clone)]
enum WorkerEvent {
    Loaded(u64, Box<Result<DashboardData, String>>),
    Recalculated(Result<(), String>),
}

#[derive(Debug, Clone)]
struct DashboardData {
    health: HealthDashboardData,
    diagnostics: DiagnosticDashboardData,
}

#[derive(Debug, Clone)]
struct Selection {
    granularity: ScoreGranularity,
    anchor: DateTime<Utc>,
    generation: u64,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            granularity: ScoreGranularity::Day,
            anchor: Utc::now(),
            generation: 0,
        }
    }
}

impl Selection {
    fn select(&mut self, granularity: ScoreGranularity) -> u64 {
        self.granularity = granularity;
        self.generation += 1;
        self.generation
    }

    fn move_window(&mut self, direction: i32) -> u64 {
        self.anchor += window_span(self.granularity) * direction;
        self.generation += 1;
        self.generation
    }

    fn accepts(&self, generation: u64) -> bool {
        self.generation == generation
    }

    fn range(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        let span = window_span(self.granularity);
        (self.anchor - span, self.anchor)
    }
}

fn window_span(granularity: ScoreGranularity) -> TimeDelta {
    match granularity {
        ScoreGranularity::Hour => TimeDelta::hours(24),
        ScoreGranularity::Day => TimeDelta::days(30),
        ScoreGranularity::Week => TimeDelta::weeks(26),
        ScoreGranularity::Month => TimeDelta::days(365 * 2),
        ScoreGranularity::Year => TimeDelta::days(365 * 10),
        ScoreGranularity::Session => TimeDelta::days(30),
    }
}

fn state_text(score: &StoredHealthScore) -> &'static str {
    match score.status {
        ScoreStatus::Learning => "Learning",
        ScoreStatus::InsufficientData => "Insufficient data",
        ScoreStatus::NoData => "No data",
        ScoreStatus::CalculationFailed => "Calculation failed",
        ScoreStatus::Scored => match score.score {
            Some(value) if value < 40.0 => "Critical decline",
            Some(value) if value < 70.0 => "Attention",
            Some(_) => "Healthy",
            None => "Unavailable",
        },
    }
}

fn score_text(score: Option<f64>) -> String {
    score
        .map(|value| format!("{value:.0}"))
        .unwrap_or_else(|| "—".to_string())
}

fn domain_name(domain: car_logger_application::ScoreDomain) -> &'static str {
    match domain {
        car_logger_application::ScoreDomain::Thermal => "Thermal",
        car_logger_application::ScoreDomain::Electrical => "Electrical",
        car_logger_application::ScoreDomain::AirFuel => "Air / fuel",
        car_logger_application::ScoreDomain::RunningStability => "Running stability",
    }
}

struct Widgets {
    root: ScrolledWindow,
    score: Label,
    state: Label,
    comparison: Label,
    confidence: Label,
    learning: Label,
    updated: Label,
    range: Label,
    chart: DrawingArea,
    domain_cards: FlowBox,
    reasons: GtkBox,
    statistics: Label,
    progress_box: GtkBox,
    progress: ProgressBar,
    progress_text: Label,
    recalculate: Button,
    recalculate_result: Label,
    error: Label,
    diagnostics: Label,
}

pub struct HealthView {
    widgets: Rc<Widgets>,
}

impl HealthView {
    pub fn setup(
        translation_manager: Rc<RefCell<TranslationManager>>,
        database_path: PathBuf,
        read_only: bool,
        parent: &gtk::ApplicationWindow,
    ) -> Self {
        let widgets = Rc::new(build_widgets(&translation_manager));
        let ai_panel = AiConditionPanel::new(database_path.clone(), read_only, parent);
        widgets
            .root
            .child()
            .and_downcast::<GtkBox>()
            .expect("health page")
            .append(ai_panel.widget());
        let selection = Rc::new(RefCell::new(Selection::default()));
        let series = Rc::new(RefCell::new(Vec::<StoredHealthScore>::new()));
        let recalculating = Rc::new(Cell::new(false));
        let alive = Rc::new(Cell::new(true));
        let (sender, receiver) = unbounded();

        widgets.root.connect_unrealize(glib::clone!(
            #[strong]
            alive,
            move |_| alive.set(false)
        ));

        configure_chart(&widgets.chart, series.clone());
        setup_granularity_buttons(
            &widgets,
            selection.clone(),
            database_path.clone(),
            sender.clone(),
        );
        setup_navigation(
            &widgets,
            selection.clone(),
            database_path.clone(),
            sender.clone(),
        );
        let initial_sender = sender.clone();
        setup_recalculation(
            &widgets,
            database_path.clone(),
            read_only,
            parent,
            sender,
            recalculating.clone(),
        );
        poll_worker(
            receiver,
            widgets.clone(),
            selection.clone(),
            series,
            recalculating,
            alive,
            database_path.clone(),
            initial_sender.clone(),
        );

        if read_only {
            widgets.recalculate.set_sensitive(false);
            widgets.recalculate.set_tooltip_text(Some(&translate(
                "Read-only database; recalculation is disabled",
            )));
            widgets.recalculate_result.set_text(&translate("Read-only"));
        }
        request_load(&database_path, &selection.borrow(), initial_sender);

        Self { widgets }
    }

    pub fn widget(&self) -> &ScrolledWindow {
        &self.widgets.root
    }
}

fn build_widgets(tm: &Rc<RefCell<TranslationManager>>) -> Widgets {
    let root = ScrolledWindow::new();
    root.set_hexpand(true);
    root.set_vexpand(true);
    let page = GtkBox::new(Orientation::Vertical, 16);
    page.add_css_class("health-root");
    root.set_child(Some(&page));

    let header = GtkBox::new(Orientation::Horizontal, 12);
    let title = label("Vehicle health", "title-label");
    title.set_hexpand(true);
    header.append(&title);
    let recalculate = Button::with_label(&translate("Recalculate all"));
    recalculate.add_css_class("secondary-button");
    header.append(&recalculate);
    page.append(&header);

    let controls = GtkBox::new(Orientation::Horizontal, 8);
    controls.set_halign(Align::Start);
    controls.add_css_class("segmented-control");
    let mut group_leader: Option<ToggleButton> = None;
    for (id, text) in [
        ("hour", "Hour"),
        ("day", "Day"),
        ("week", "Week"),
        ("month", "Month"),
        ("year", "Year"),
    ] {
        let button = ToggleButton::with_label(&translate(text));
        if let Some(leader) = &group_leader {
            button.set_group(Some(leader));
        } else {
            group_leader = Some(button.clone());
        }
        button.set_widget_name(id);
        button.add_css_class("segment-button");
        if id == "day" {
            button.set_active(true);
        }
        controls.append(&button);
    }
    let previous = Button::with_label("‹");
    previous.set_widget_name("health_previous");
    previous.set_tooltip_text(Some(&translate("Previous period")));
    controls.append(&previous);
    let next = Button::with_label("›");
    next.set_widget_name("health_next");
    next.set_tooltip_text(Some(&translate("Next period")));
    controls.append(&next);
    let range = label("", "muted-label");
    controls.append(&range);
    page.append(&controls);

    let summary = GtkBox::new(Orientation::Horizontal, 18);
    summary.add_css_class("health-summary");
    let score = label("—", "health-score");
    summary.append(&score);
    let summary_text = GtkBox::new(Orientation::Vertical, 5);
    let state = label(&translate("No data"), "health-state");
    let comparison = label("—", "muted-label");
    let confidence = label("—", "muted-label");
    let learning = label("", "health-learning");
    let updated = label("—", "muted-label");
    for item in [&state, &comparison, &confidence, &learning, &updated] {
        summary_text.append(item);
    }
    summary.append(&summary_text);
    page.append(&summary);

    let diagnostic_panel = GtkBox::new(Orientation::Vertical, 8);
    diagnostic_panel.add_css_class("panel");
    diagnostic_panel.append(&label(
        "Diagnostics (separate from health score)",
        "section-title",
    ));
    let diagnostics = label("No diagnostic observation yet", "health-diagnostics");
    diagnostics.set_wrap(true);
    diagnostic_panel.append(&diagnostics);
    page.append(&diagnostic_panel);

    let chart_panel = GtkBox::new(Orientation::Vertical, 8);
    chart_panel.add_css_class("panel");
    chart_panel.append(&label(&translate("Score trend"), "section-title"));
    let chart = DrawingArea::new();
    chart.set_content_height(260);
    chart.set_hexpand(true);
    chart.add_css_class("health-chart");
    chart_panel.append(&chart);
    page.append(&chart_panel);

    page.append(&label(&translate("Areas"), "section-title"));
    let domain_cards = FlowBox::new();
    domain_cards.set_selection_mode(gtk::SelectionMode::None);
    domain_cards.set_max_children_per_line(4);
    domain_cards.set_min_children_per_line(1);
    domain_cards.set_column_spacing(12);
    domain_cards.set_row_spacing(12);
    page.append(&domain_cards);

    let details = GtkBox::new(Orientation::Vertical, 12);
    let reason_panel = GtkBox::new(Orientation::Vertical, 8);
    reason_panel.set_hexpand(true);
    reason_panel.add_css_class("panel");
    reason_panel.append(&label(&translate("Why the score changed"), "section-title"));
    let reasons = GtkBox::new(Orientation::Vertical, 8);
    reason_panel.append(&reasons);
    details.append(&reason_panel);
    let stats_panel = GtkBox::new(Orientation::Vertical, 8);
    stats_panel.add_css_class("panel");
    stats_panel.append(&label(&translate("Period data"), "section-title"));
    let statistics = label("—", "muted-label");
    stats_panel.append(&statistics);
    details.append(&stats_panel);
    page.append(&details);

    let progress_box = GtkBox::new(Orientation::Vertical, 6);
    progress_box.add_css_class("health-progress");
    progress_box.append(&label(
        &translate("Backfill / recalculation"),
        "section-title",
    ));
    let progress = ProgressBar::new();
    let progress_text = label("—", "muted-label");
    progress_box.append(&progress);
    progress_box.append(&progress_text);
    page.append(&progress_box);
    let recalculate_result = label(&translate("No recalculation result"), "muted-label");
    page.append(&recalculate_result);
    let error = label("", "health-error");
    error.set_wrap(true);
    page.append(&error);

    {
        let mut tm = tm.borrow_mut();
        tm.add(title, "Vehicle health");
        tm.add_button(recalculate.clone(), "Recalculate all");
        tm.add_redraw_area(chart.clone());
    }
    Widgets {
        root,
        score,
        state,
        comparison,
        confidence,
        learning,
        updated,
        range,
        chart,
        domain_cards,
        reasons,
        statistics,
        progress_box,
        progress,
        progress_text,
        recalculate,
        recalculate_result,
        error,
        diagnostics,
    }
}

fn label(text: &str, class: &str) -> Label {
    let label = Label::new(Some(text));
    label.set_halign(Align::Start);
    label.add_css_class(class);
    label
}

fn setup_granularity_buttons(
    widgets: &Rc<Widgets>,
    selection: Rc<RefCell<Selection>>,
    path: PathBuf,
    sender: Sender<WorkerEvent>,
) {
    for (id, granularity) in [
        ("hour", ScoreGranularity::Hour),
        ("day", ScoreGranularity::Day),
        ("week", ScoreGranularity::Week),
        ("month", ScoreGranularity::Month),
        ("year", ScoreGranularity::Year),
    ] {
        if let Some(button) = find_widget::<ToggleButton>(widgets.root.upcast_ref(), id) {
            button.connect_toggled(glib::clone!(
                #[strong]
                selection,
                #[strong]
                path,
                #[strong]
                sender,
                move |button| {
                    if button.is_active() {
                        selection.borrow_mut().select(granularity);
                        request_load(&path, &selection.borrow(), sender.clone());
                    }
                }
            ));
        }
    }
}

fn find_widget<T: IsA<gtk::Widget>>(root: &gtk::Widget, name: &str) -> Option<T> {
    if root.widget_name() == name {
        return root.clone().downcast::<T>().ok();
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_widget::<T>(&widget, name) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn setup_navigation(
    widgets: &Rc<Widgets>,
    selection: Rc<RefCell<Selection>>,
    path: PathBuf,
    sender: Sender<WorkerEvent>,
) {
    for (name, direction) in [("health_previous", -1), ("health_next", 1)] {
        if let Some(button) = find_widget::<Button>(widgets.root.upcast_ref(), name) {
            button.connect_clicked(glib::clone!(
                #[strong]
                selection,
                #[strong]
                path,
                #[strong]
                sender,
                move |_| {
                    selection.borrow_mut().move_window(direction);
                    request_load(&path, &selection.borrow(), sender.clone());
                }
            ));
        }
    }
}

fn request_load(path: &Path, selection: &Selection, sender: Sender<WorkerEvent>) {
    let path = path.to_path_buf();
    let generation = selection.generation;
    let granularity = selection.granularity;
    let (start, end) = selection.range();
    thread::spawn(move || {
        let result = (|| {
            let repository = DuckdbCanFrameRepository::open_read_only(path)?;
            let diagnostics = repository.diagnostic_dashboard(100)?;
            let health = HealthService::new(repository).dashboard(
                granularity,
                start,
                end,
                MAX_GRAPH_POINTS,
            )?;
            Ok(DashboardData {
                health,
                diagnostics,
            })
        })()
        .map_err(|error: anyhow::Error| error.to_string());
        let _ = sender.send(WorkerEvent::Loaded(generation, Box::new(result)));
    });
}

fn setup_recalculation(
    widgets: &Rc<Widgets>,
    path: PathBuf,
    read_only: bool,
    parent: &gtk::ApplicationWindow,
    sender: Sender<WorkerEvent>,
    running: Rc<Cell<bool>>,
) {
    widgets.recalculate.connect_clicked(glib::clone!(#[weak] parent, #[strong] running, #[strong] widgets, move |_| {
        if read_only || running.get() { return; }
        let dialog = MessageDialog::builder()
            .transient_for(&parent).modal(true)
            .text(translate("Recalculate all health scores?"))
            .secondary_text(translate("Raw logs will not be deleted or changed. Processing continues in the background."))
            .buttons(gtk::ButtonsType::OkCancel).build();
        let path = path.clone();
        let sender = sender.clone();
        let running = running.clone();
        let widgets = widgets.clone();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response != gtk::ResponseType::Ok || running.replace(true) { return; }
            widgets.recalculate.set_sensitive(false);
            widgets.recalculate_result.set_text(&translate("Recalculation in progress"));
            let path = path.clone();
            let sender = sender.clone();
            thread::spawn(move || {
                let result = DuckdbCanFrameRepository::open(path)
                    .map(HealthService::new)
                    .and_then(|mut service| service.recalculate_all(RECALCULATE_CHUNK_SIZE))
                    .map(|_| ()).map_err(|error| error.to_string());
                let _ = sender.send(WorkerEvent::Recalculated(result));
            });
        });
        dialog.present();
    }));
}

#[allow(clippy::too_many_arguments)]
fn poll_worker(
    receiver: Receiver<WorkerEvent>,
    widgets: Rc<Widgets>,
    selection: Rc<RefCell<Selection>>,
    series: Rc<RefCell<Vec<StoredHealthScore>>>,
    recalculating: Rc<Cell<bool>>,
    alive: Rc<Cell<bool>>,
    path: PathBuf,
    sender: Sender<WorkerEvent>,
) {
    glib::timeout_add_local(Duration::from_millis(120), move || {
        if !alive.get() {
            return glib::ControlFlow::Break;
        }
        for event in receiver.try_iter() {
            match event {
                WorkerEvent::Loaded(generation, data) if selection.borrow().accepts(generation) => {
                    match *data {
                        Ok(data) => {
                            render(&widgets, &data.health);
                            render_diagnostics(&widgets.diagnostics, &data.diagnostics);
                            *series.borrow_mut() = data.health.series;
                            widgets.chart.queue_draw();
                        }
                        Err(error) => {
                            widgets
                                .error
                                .set_text(&format!("{}: {error}", translate("Database error")));
                            widgets.state.set_text(&translate("Database error"));
                        }
                    }
                }
                WorkerEvent::Recalculated(result) => {
                    recalculating.set(false);
                    widgets.recalculate.set_sensitive(true);
                    match result {
                        Ok(()) => {
                            widgets
                                .recalculate_result
                                .set_text(&translate("Recalculation completed"));
                            selection.borrow_mut().generation += 1;
                            request_load(&path, &selection.borrow(), sender.clone());
                        }
                        Err(error) => {
                            widgets
                                .recalculate_result
                                .set_text(&translate("Recalculation failed; retry is available"));
                            widgets.error.set_text(&error);
                        }
                    }
                }
                _ => {}
            }
        }
        if recalculating.get() {
            widgets.progress_box.set_visible(true);
            widgets.progress.pulse();
            widgets
                .progress_text
                .set_text(&translate("Recalculation in progress"));
        }
        glib::ControlFlow::Continue
    });
}

fn render_diagnostics(label: &Label, data: &DiagnosticDashboardData) {
    let support = match data.supported {
        Some(false) => "DTC acquisition is not supported by this ECU/adapter".to_string(),
        None if data.last_observed_at.is_none() => "No diagnostic observation yet".to_string(),
        _ => format!(
            "MIL: {} · Active DTCs: {}",
            data.mil_on
                .map_or("unknown", |on| if on { "ON" } else { "OFF" }),
            data.active.len()
        ),
    };
    let active = data
        .active
        .iter()
        .map(|dtc| {
            format!(
                "{} · active · first {} · last {}",
                dtc.code,
                dtc.first_detected_at
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M"),
                dtc.last_detected_at
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
            )
        })
        .collect::<Vec<_>>();
    let history = data
        .history
        .iter()
        .filter(|dtc| !dtc.active)
        .take(10)
        .map(|dtc| {
            format!(
                "{} · cleared {} · occurrence {}",
                dtc.code,
                dtc.cleared_at
                    .map(|at| at
                        .with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string())
                    .unwrap_or_else(|| "unknown".into()),
                dtc.occurrence
            )
        })
        .collect::<Vec<_>>();
    let last = data.last_observed_at.map(|at| {
        format!(
            "Last acquisition: {}",
            at.with_timezone(&Local).format("%Y-%m-%d %H:%M")
        )
    });
    let error = data
        .last_error
        .as_ref()
        .map(|error| format!("Last acquisition failed (logging continues): {error}"));
    let mut lines = vec![
        support,
        "DTC absence does not prove that the vehicle has no fault.".into(),
    ];
    lines.extend(last);
    lines.extend(error);
    lines.extend(active);
    if !history.is_empty() {
        lines.push("History:".into());
        lines.extend(history);
    }
    label.set_text(&lines.join("\n"));
}

fn render(w: &Widgets, data: &HealthDashboardData) {
    w.error.set_text("");
    let Some(latest) = &data.latest else {
        w.score.set_text("—");
        w.state.set_text(&translate("No data"));
        w.comparison
            .set_text(&translate("No driving data in this period"));
        clear_box(&w.reasons);
        clear_flow(&w.domain_cards);
        return;
    };
    w.score.set_text(&score_text(latest.score));
    for class in [
        "health-state-ok",
        "health-state-warning",
        "health-state-critical",
    ] {
        w.state.remove_css_class(class);
    }
    match latest.score {
        Some(value) if latest.status == ScoreStatus::Scored && value < 40.0 => {
            w.state.add_css_class("health-state-critical")
        }
        Some(value) if latest.status == ScoreStatus::Scored && value < 70.0 => {
            w.state.add_css_class("health-state-warning")
        }
        Some(_) if latest.status == ScoreStatus::Scored => w.state.add_css_class("health-state-ok"),
        _ => w.state.add_css_class("health-state-warning"),
    }
    w.state.set_text(&translate(state_text(latest)));
    let comparison = match (
        latest.score,
        data.previous.as_ref().and_then(|item| item.score),
    ) {
        (Some(now), Some(before)) => format!("{} {:+.1}", translate("vs previous"), now - before),
        _ => translate("Previous comparison unavailable"),
    };
    w.comparison.set_text(&comparison);
    w.confidence.set_text(&format!(
        "{} {:.0}%",
        translate("Confidence"),
        latest.confidence
    ));
    w.updated.set_text(&format!(
        "{} {}",
        translate("Last updated"),
        latest
            .calculated_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
    ));
    w.range.set_text(&format!(
        "{} – {}",
        latest.period_start.with_timezone(&Local).format("%Y-%m-%d"),
        latest.period_end.with_timezone(&Local).format("%Y-%m-%d")
    ));
    if latest.status == ScoreStatus::Learning {
        w.learning.set_text(&format!(
            "{} · {} {}/10 · {} {:.1}/3h · {}",
            translate("Learning"),
            translate("Valid sessions"),
            latest.session_count.min(10),
            translate("Learning time"),
            (latest.evaluated_seconds / 3600.0).min(3.0),
            translate("Current score is provisional")
        ));
    } else {
        w.learning.set_text("");
    }
    w.statistics.set_text(&format!(
        "{}: {}\n{}: {:.1}h\n{}: {}\n{}: {:.0}%",
        translate("Trips"),
        latest.session_count,
        translate("Evaluated time"),
        latest.evaluated_seconds / 3600.0,
        translate("Samples"),
        latest.sample_count,
        translate("Data coverage"),
        latest.coverage * 100.0
    ));
    render_components(&w.domain_cards, &data.components);
    render_reasons(&w.reasons, data);
    if let Some(progress) = &data.progress {
        let fraction = if progress.total_rows == 0 {
            if progress.completed { 1.0 } else { 0.0 }
        } else {
            progress.processed_rows as f64 / progress.total_rows as f64
        };
        w.progress.set_fraction(fraction.clamp(0.0, 1.0));
        w.progress_text.set_text(&format!(
            "{} / {} ({:.0}%) · {} {}",
            progress.processed_rows,
            progress.total_rows,
            fraction * 100.0,
            translate(if progress.completed {
                "Completed"
            } else {
                "Processing"
            }),
            progress
                .updated_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
        ));
        w.progress_box.set_visible(true);
        if progress.operation == "recalculate" && progress.completed {
            w.recalculate_result.set_text(&format!(
                "{} · {}",
                translate("Recalculation completed"),
                progress
                    .updated_at
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
            ));
        }
    } else {
        w.progress_box.set_visible(false);
    }
}

fn render_components(flow: &FlowBox, components: &[StoredComponent]) {
    clear_flow(flow);
    for domain in [
        car_logger_application::ScoreDomain::Thermal,
        car_logger_application::ScoreDomain::Electrical,
        car_logger_application::ScoreDomain::AirFuel,
        car_logger_application::ScoreDomain::RunningStability,
    ] {
        let component = components.iter().find(|item| item.domain == domain);
        let card = GtkBox::new(Orientation::Vertical, 5);
        card.add_css_class("health-domain-card");
        card.append(&label(&translate(domain_name(domain)), "metric-label"));
        let value = component.and_then(|item| item.score);
        card.append(&label(&score_text(value), "health-domain-score"));
        let detail = component
            .map(|item| {
                format!(
                    "{} {:.0}% · {} {:.0}%",
                    translate("Confidence"),
                    item.confidence,
                    translate("Coverage"),
                    item.coverage * 100.0
                )
            })
            .unwrap_or_else(|| translate("Unavailable"));
        card.append(&label(&detail, "muted-label"));
        flow.insert(&card, -1);
    }
}

fn render_reasons(box_: &GtkBox, data: &HealthDashboardData) {
    clear_box(box_);
    if data.reasons.is_empty() {
        box_.append(&label(
            &translate("No significant change reason"),
            "muted-label",
        ));
        return;
    }
    for reason in data.reasons.iter().take(8) {
        let row = label(
            &format!(
                "{} · {} · {} {:.1} · {}",
                translate(domain_name(reason.domain)),
                reason.feature,
                translate("Impact"),
                -reason.impact,
                reason.message
            ),
            "health-reason",
        );
        row.set_wrap(true);
        box_.append(&row);
    }
}

fn clear_box(box_: &GtkBox) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
}
fn clear_flow(flow: &FlowBox) {
    while let Some(child) = flow.first_child() {
        flow.remove(&child);
    }
}

fn configure_chart(area: &DrawingArea, series: Rc<RefCell<Vec<StoredHealthScore>>>) {
    area.set_draw_func(move |_, cr, width, height| {
        let points: Vec<_> = series
            .borrow()
            .iter()
            .filter_map(|item| item.score)
            .collect();
        cr.set_source_rgb(0.16, 0.20, 0.24);
        cr.set_line_width(1.0);
        for i in 0..=4 {
            let y = 12.0 + (height as f64 - 24.0) * i as f64 / 4.0;
            cr.move_to(12.0, y);
            cr.line_to(width as f64 - 12.0, y);
        }
        let _ = cr.stroke();
        if points.len() < 2 {
            return;
        }
        cr.set_source_rgb(0.20, 0.82, 0.95);
        cr.set_line_width(2.5);
        for (index, value) in points.iter().enumerate() {
            let x = 12.0 + (width as f64 - 24.0) * index as f64 / (points.len() - 1) as f64;
            let y = 12.0 + (height as f64 - 24.0) * (1.0 - *value / 100.0);
            if index == 0 {
                cr.move_to(x, y);
            } else {
                cr.line_to(x, y);
            }
        }
        let _ = cr.stroke();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn score(value: Option<f64>, status: ScoreStatus) -> StoredHealthScore {
        StoredHealthScore {
            id: 1,
            granularity: ScoreGranularity::Day,
            period_start: Utc::now(),
            period_end: Utc::now(),
            score: value,
            confidence: 80.0,
            status,
            session_count: 1,
            evaluated_seconds: 1.0,
            sample_count: 1,
            coverage: 1.0,
            algorithm_version: "a".into(),
            baseline_version: "b".into(),
            feature_schema_version: "f".into(),
            calculated_at: Utc::now(),
        }
    }
    #[test]
    fn state_boundaries_are_explicit() {
        assert_eq!(
            state_text(&score(Some(39.99), ScoreStatus::Scored)),
            "Critical decline"
        );
        assert_eq!(
            state_text(&score(Some(40.0), ScoreStatus::Scored)),
            "Attention"
        );
        assert_eq!(
            state_text(&score(Some(70.0), ScoreStatus::Scored)),
            "Healthy"
        );
        assert_eq!(
            state_text(&score(Some(20.0), ScoreStatus::Learning)),
            "Learning"
        );
        assert_eq!(score_text(None), "—");
    }
    #[test]
    fn stale_async_results_are_rejected() {
        let mut selection = Selection::default();
        let old = selection.generation;
        selection.select(ScoreGranularity::Month);
        assert!(!selection.accepts(old));
        assert!(selection.accepts(old + 1));
    }
    #[test]
    fn every_visible_granularity_has_a_bounded_window() {
        for item in [
            ScoreGranularity::Hour,
            ScoreGranularity::Day,
            ScoreGranularity::Week,
            ScoreGranularity::Month,
            ScoreGranularity::Year,
        ] {
            assert!(window_span(item) > TimeDelta::zero());
        }
    }
}
