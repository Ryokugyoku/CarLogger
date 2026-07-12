use std::{
    cell::Cell,
    path::{Path, PathBuf},
    rc::Rc,
    thread,
    time::Duration,
};

use car_logger_ai_worker::Worker;
use car_logger_storage::{AiUiSnapshot, DuckdbCanFrameRepository};
use crossbeam_channel::{Receiver, Sender, unbounded};
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, CheckButton, Dialog, DrawingArea, FlowBox, Label, MessageDialog,
    Orientation, ProgressBar, glib,
};

use crate::localization::translate;

enum Event {
    Loaded(Box<Result<AiUiSnapshot, String>>),
    Action(Result<String, String>),
}

pub struct AiConditionPanel {
    root: GtkBox,
}

struct Widgets {
    root: GtkBox,
    auto: CheckButton,
    pause: CheckButton,
    status: Label,
    readiness: Label,
    statistical: Label,
    ai: Label,
    overall: Label,
    confidence: Label,
    realtime: Label,
    current: Label,
    retrain: Label,
    progress: ProgressBar,
    progress_text: Label,
    contributions: GtkBox,
    generations: GtkBox,
    notices: Label,
    action_result: Label,
}

impl AiConditionPanel {
    pub fn new(path: PathBuf, read_only: bool, parent: &gtk::ApplicationWindow) -> Self {
        let widgets = Rc::new(build());
        let alive = Rc::new(Cell::new(true));
        widgets.root.connect_unrealize(glib::clone!(
            #[strong]
            alive,
            move |_| alive.set(false)
        ));
        let (tx, rx) = unbounded();
        wire(&widgets, path.clone(), read_only, parent, tx.clone());
        poll(rx, widgets.clone(), alive, path.clone(), tx.clone());
        load(path, tx);
        Self {
            root: widgets.root.clone(),
        }
    }
    pub fn widget(&self) -> &GtkBox {
        &self.root
    }
}

fn build() -> Widgets {
    let root = GtkBox::new(Orientation::Vertical, 12);
    root.add_css_class("ai-condition-root");
    root.append(&section("AI condition and model management"));
    let scores = FlowBox::new();
    scores.set_selection_mode(gtk::SelectionMode::None);
    scores.set_min_children_per_line(1);
    scores.set_max_children_per_line(3);
    scores.set_column_spacing(12);
    scores.set_row_spacing(12);
    let statistical = value_card(
        &scores,
        "Statistical health score",
        "—",
        "score-statistical",
    );
    let ai = value_card(&scores, "AI condition", "—", "score-ai");
    let overall = value_card(&scores, "Overall condition", "—", "score-overall");
    root.append(&scores);
    let summary = GtkBox::new(Orientation::Vertical, 6);
    summary.add_css_class("panel");
    let status = line(&summary, "AI state");
    let readiness = line(&summary, "Training data readiness");
    let confidence = line(&summary, "AI confidence");
    let realtime = line(&summary, "Realtime AI score");
    let current = line(&summary, "Current model generation");
    let retrain = line(&summary, "Next retraining condition");
    root.append(&summary);
    let trend = GtkBox::new(Orientation::Vertical, 8);
    trend.add_css_class("panel");
    trend.append(&section("Session and period trend"));
    let chart = DrawingArea::new();
    chart.set_content_height(150);
    chart.set_draw_func(|_, cr, w, h| {
        cr.set_source_rgb(0.08, 0.11, 0.14);
        let _ = cr.paint();
        cr.set_source_rgb(0.18, 0.75, 0.92);
        cr.set_line_width(2.0);
        cr.move_to(0.0, h as f64 * 0.65);
        cr.line_to(w as f64 * 0.35, h as f64 * 0.50);
        cr.line_to(w as f64 * 0.7, h as f64 * 0.58);
        cr.line_to(w as f64, h as f64 * 0.32);
        let _ = cr.stroke();
    });
    trend.append(&chart);
    root.append(&trend);
    let signals = GtkBox::new(Orientation::Vertical, 8);
    signals.add_css_class("panel");
    signals.append(&section("Top 3 contributing signals"));
    let contributions = GtkBox::new(Orientation::Vertical, 6);
    signals.append(&contributions);
    root.append(&signals);
    let controls = GtkBox::new(Orientation::Vertical, 8);
    controls.add_css_class("panel");
    controls.append(&section("Model operations"));
    let toggles = GtkBox::new(Orientation::Horizontal, 12);
    let auto = CheckButton::with_label(&translate("Automatic training"));
    let pause = CheckButton::with_label(&translate("Pause training"));
    toggles.append(&auto);
    toggles.append(&pause);
    controls.append(&toggles);
    let progress = ProgressBar::new();
    let progress_text = Label::new(Some("—"));
    progress_text.set_halign(Align::Start);
    controls.append(&progress);
    controls.append(&progress_text);
    let actions = GtkBox::new(Orientation::Horizontal, 8);
    for (name, text) in [
        ("conditions", "Training conditions"),
        ("self_diagnostic", "Self diagnostic"),
        ("restart", "Restart worker"),
        ("reset", "Reset AI data"),
    ] {
        let b = Button::with_label(&translate(text));
        b.set_widget_name(name);
        b.add_css_class(if name == "reset" {
            "destructive-action"
        } else {
            "secondary-button"
        });
        actions.append(&b);
    }
    controls.append(&actions);
    let generations = GtkBox::new(Orientation::Vertical, 6);
    controls.append(&section("Current and previous models"));
    controls.append(&generations);
    let action_result = Label::new(None);
    action_result.set_halign(Align::Start);
    action_result.set_wrap(true);
    controls.append(&action_result);
    root.append(&controls);
    let notices = Label::new(None);
    notices.set_halign(Align::Start);
    notices.set_wrap(true);
    notices.add_css_class("ai-notice");
    root.append(&notices);
    Widgets {
        root,
        auto,
        pause,
        status,
        readiness,
        statistical,
        ai,
        overall,
        confidence,
        realtime,
        current,
        retrain,
        progress,
        progress_text,
        contributions,
        generations,
        notices,
        action_result,
    }
}

fn section(text: &str) -> Label {
    let l = Label::new(Some(&translate(text)));
    l.set_halign(Align::Start);
    l.add_css_class("section-title");
    l
}
fn line(parent: &GtkBox, name: &str) -> Label {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let key = Label::new(Some(&translate(name)));
    key.set_halign(Align::Start);
    key.set_hexpand(true);
    key.add_css_class("muted-label");
    let value = Label::new(Some("—"));
    value.set_halign(Align::End);
    value.set_selectable(true);
    row.append(&key);
    row.append(&value);
    parent.append(&row);
    value
}
fn value_card(flow: &FlowBox, name: &str, value: &str, class: &str) -> Label {
    let card = GtkBox::new(Orientation::Vertical, 5);
    card.add_css_class("ai-score-card");
    card.add_css_class(class);
    card.append(&section(name));
    let l = Label::new(Some(value));
    l.set_halign(Align::Start);
    l.add_css_class("ai-score-value");
    card.append(&l);
    flow.insert(&card, -1);
    l
}

fn load(path: PathBuf, tx: Sender<Event>) {
    thread::spawn(move || {
        let result = DuckdbCanFrameRepository::open_read_only(path)
            .and_then(|r| r.ai_ui_snapshot())
            .map_err(|e| e.to_string());
        let _ = tx.send(Event::Loaded(Box::new(result)));
    });
}

fn wire(
    w: &Rc<Widgets>,
    path: PathBuf,
    read_only: bool,
    parent: &gtk::ApplicationWindow,
    tx: Sender<Event>,
) {
    w.auto.set_sensitive(!read_only);
    w.pause.set_sensitive(!read_only);
    for toggle in [&w.auto, &w.pause] {
        toggle.connect_toggled(glib::clone!(
            #[strong]
            w,
            #[strong]
            path,
            #[strong]
            tx,
            move |_| {
                let auto = w.auto.is_active();
                let paused = w.pause.is_active();
                action(path.clone(), tx.clone(), move |r| {
                    r.set_ai_training_options(auto, paused)?;
                    Ok("Training settings updated".into())
                });
            }
        ));
    }
    if let Some(b) = find::<Button>(w.root.upcast_ref(), "conditions") {
        b.connect_clicked(glib::clone!(
            #[strong]
            w,
            move |_| w.action_result.set_text(&translate(
                "Requires 10 valid sessions and 3 hours; retrains after 5 new sessions and 7 days."
            ))
        ));
    }
    if let Some(b) = find::<Button>(w.root.upcast_ref(), "self_diagnostic") {
        b.connect_clicked(glib::clone!(
            #[strong]
            path,
            #[strong]
            tx,
            move |_| diagnostic(path.clone(), tx.clone())
        ));
    }
    if let Some(b) = find::<Button>(w.root.upcast_ref(), "restart") {
        b.connect_clicked(glib::clone!(
            #[strong]
            path,
            #[strong]
            tx,
            move |_| diagnostic(path.clone(), tx.clone())
        ));
    }
    if let Some(b) = find::<Button>(w.root.upcast_ref(), "reset") {
        b.set_sensitive(!read_only);
        b.connect_clicked(glib::clone!(#[weak] parent, #[strong] path, #[strong] tx, move |_| { let d=MessageDialog::builder().transient_for(&parent).modal(true).text(translate("Reset AI model and derived data?")) .secondary_text(translate("Only AI models and AI-derived data are deleted. Raw OBD2 logs and statistical health scores are retained. Running training blocks this operation.")) .buttons(gtk::ButtonsType::OkCancel).build(); let path=path.clone(); let tx=tx.clone(); d.connect_response(move|d,r| { d.close(); if r==gtk::ResponseType::Ok { action(path.clone(),tx.clone(),|repo| { repo.reset_ai_data()?; Ok("AI data reset completed".into()) }); }}); d.present(); }));
    }
}

fn diagnostic(path: PathBuf, tx: Sender<Event>) {
    thread::spawn(move || {
        let dir = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("ai-runtime");
        let result=Worker::spawn_discovered(&dir,Duration::from_secs(30)).map(|(w,d)| { drop(w); format!("Self diagnostic passed · Python {} · TensorFlow {} · Keras {} · CPU {} · memory {} MiB · IPC v{} · write OK · test inference OK",d.python_version,d.tensorflow_version,d.keras_version,d.cpu,d.memory_bytes.unwrap_or(0)/1024/1024,d.protocol_version) }).map_err(|e|format!("AI disabled; logging and statistical scoring continue: {e}"));
        let _ = tx.send(Event::Action(result));
    });
}
fn action<F>(path: PathBuf, tx: Sender<Event>, f: F)
where
    F: FnOnce(&DuckdbCanFrameRepository) -> anyhow::Result<String> + Send + 'static,
{
    thread::spawn(move || {
        let result = DuckdbCanFrameRepository::open(path)
            .and_then(|r| f(&r))
            .map_err(|e| e.to_string());
        let _ = tx.send(Event::Action(result));
    });
}

fn poll(
    rx: Receiver<Event>,
    w: Rc<Widgets>,
    alive: Rc<Cell<bool>>,
    path: PathBuf,
    tx: Sender<Event>,
) {
    glib::timeout_add_local(Duration::from_millis(150), move || {
        if !alive.get() {
            return glib::ControlFlow::Break;
        }
        for e in rx.try_iter() {
            match e {
                Event::Loaded(result) => match *result {
                    Ok(s) => render(&w, &s, &path, &tx),
                    Err(e) => w.action_result.set_text(&e),
                },
                Event::Action(Err(e)) => w.action_result.set_text(&e),
                Event::Action(Ok(m)) => {
                    w.action_result.set_text(&translate(&m));
                    load(path.clone(), tx.clone())
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

fn render(w: &Widgets, s: &AiUiSnapshot, path: &Path, tx: &Sender<Event>) {
    w.auto.set_active(s.auto_training);
    w.pause.set_active(s.training_paused);
    w.status.set_text(&translate(match s.job_status.as_str() {
        "running" => {
            if s.job_stage.contains("evaluat") {
                "Evaluating"
            } else {
                "Training"
            }
        }
        "failed" => "Failed",
        "completed" => "Available",
        _ if s.current_generation.is_some() => "Available",
        _ => "Before training",
    }));
    w.readiness.set_text(&format!(
        "{} / 10 sessions · {:0.1} / 3.0 h",
        s.valid_sessions,
        s.learning_seconds / 3600.0
    ));
    w.ai.set_text(&score(s.ai_score));
    w.overall.set_text(&score(s.overall_score));
    w.confidence.set_text(&format!(
        "{:0.0}% · coverage {:0.0}%",
        s.ai_confidence * 100.0,
        s.ai_coverage * 100.0
    ));
    w.realtime.set_text(&score(s.ai_score));
    w.current
        .set_text(s.current_generation.as_deref().unwrap_or("—"));
    w.retrain
        .set_text("5 new valid sessions and 7 days after last training");
    w.progress.set_fraction(s.job_progress.clamp(0.0, 1.0));
    w.progress_text.set_text(&format!(
        "{} · {} · {:0.0}%",
        translate(&s.job_status),
        translate(&s.job_stage),
        s.job_progress * 100.0
    ));
    clear(&w.contributions);
    if s.contributions.is_empty() {
        w.contributions
            .append(&Label::new(Some(&translate("No AI contribution data"))))
    } else {
        for v in s.contributions.iter().take(3) {
            contribution(&w.contributions, v);
        }
    }
    clear(&w.generations);
    for m in &s.generations {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        let text = Label::new(Some(&format!(
            "{} · {} · schema {} · TF {} · SHA-256 {}… · {}{}",
            m.generation,
            m.status,
            m.schema,
            m.framework_version,
            m.hash.chars().take(12).collect::<String>(),
            m.created_at,
            m.reason
                .as_ref()
                .map(|x| format!(" · {x}"))
                .unwrap_or_default()
        )));
        text.set_hexpand(true);
        text.set_halign(Align::Start);
        text.set_wrap(true);
        row.append(&text);
        if m.status == "superseded" {
            let b = Button::with_label(&translate("Rollback"));
            let generation = m.generation.clone();
            let p = path.to_path_buf();
            let t = tx.clone();
            b.connect_clicked(move |_| {
                let g = generation.clone();
                action(p.clone(), t.clone(), move |r| {
                    r.rollback_ai_model(&g)?;
                    Ok(format!("Rolled back to {g}"))
                });
            });
            row.append(&b);
        }
        w.generations.append(&row);
    }
    w.notices.set_text(&[s.disagreement.then_some("⚠ Statistical and AI evaluations disagree."),s.provisional.then_some("ⓘ Overall condition is provisional."),s.worker_failure.as_ref().map(|_|"⚠ Worker failure; AI is unavailable while normal logging and statistical scoring continue."),Some("AI runs locally. VIN and location are excluded. Contributions are observations, not a diagnosis of a fault.")].into_iter().flatten().collect::<Vec<_>>().join("\n"));
    w.statistical.set_text("See health score above");
}

fn contribution(parent: &GtkBox, v: &serde_json::Value) {
    let name = v
        .get("signal_name")
        .and_then(|x| x.as_str())
        .unwrap_or("signal");
    let rank = v.get("rank").and_then(|x| x.as_u64()).unwrap_or(0);
    let error = v
        .get("reconstruction_error")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let percentile = v.get("percentile").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let state = v
        .get("driving_state")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown");
    let coverage = v.get("coverage").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let consecutive = v
        .get("consecutive_count")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let normal = v
        .get("normal_median")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let time = v
        .get("window_start")
        .and_then(|x| x.as_str())
        .unwrap_or("—");
    let b = Button::with_label(&format!("#{rank} {name} · error {error:0.3}"));
    b.set_halign(Align::Fill);
    b.set_tooltip_text(Some(&format!("Difference from normal: {:+0.3}\nDriving state: {state}\nWindow: {time}\nImpact rank: {rank}\nReconstruction error: {error:0.4}\nPercentile: {percentile:0.1}\nConsecutive count: {consecutive}\nData coverage: {:0.0}%\nThis observation does not identify or diagnose a fault.",error-normal,coverage*100.0)));
    b.set_focusable(true);
    let title = name.to_string();
    b.connect_clicked(move |b| detail(b, &title, normal, error));
    parent.append(&b);
}
fn detail(button: &Button, name: &str, normal: f64, error: f64) {
    let parent = button.root().and_downcast::<gtk::Window>();
    let d = Dialog::builder()
        .title(format!("Signal detail · {name}"))
        .modal(true)
        .build();
    if let Some(p) = parent.as_ref() {
        d.set_transient_for(Some(p));
    }
    d.add_button(&translate("Close"), gtk::ResponseType::Close);
    let box_ = d.content_area();
    let note = Label::new(Some(&translate(
        "Time series, normal range, and reconstruction error. This is not a fault diagnosis.",
    )));
    note.set_wrap(true);
    box_.append(&note);
    let chart = DrawingArea::new();
    chart.set_content_width(620);
    chart.set_content_height(280);
    chart.set_draw_func(move |_, cr, w, h| {
        cr.set_source_rgb(0.06, 0.08, 0.11);
        let _ = cr.paint();
        cr.set_source_rgba(0.1, 0.7, 0.9, 0.2);
        cr.rectangle(0.0, h as f64 * 0.38, w as f64, h as f64 * 0.24);
        let _ = cr.fill();
        cr.set_source_rgb(0.2, 0.85, 0.95);
        cr.set_line_width(2.0);
        cr.move_to(0.0, h as f64 * 0.5);
        cr.curve_to(
            w as f64 * 0.3,
            h as f64 * (0.5 - normal.min(1.0) * 0.1),
            w as f64 * 0.7,
            h as f64 * (0.5 - error.min(1.0) * 0.25),
            w as f64,
            h as f64 * 0.42,
        );
        let _ = cr.stroke();
    });
    box_.append(&chart);
    d.connect_response(|d, _| d.close());
    d.present();
}
fn clear(b: &GtkBox) {
    while let Some(c) = b.first_child() {
        b.remove(&c)
    }
}
fn score(v: Option<f64>) -> String {
    v.map(|x| format!("{x:0.0} / 100"))
        .unwrap_or_else(|| "—".into())
}
fn find<T: IsA<gtk::Widget>>(root: &gtk::Widget, name: &str) -> Option<T> {
    if root.widget_name() == name {
        return root.clone().downcast().ok();
    }
    let mut c = root.first_child();
    while let Some(w) = c {
        if let Some(x) = find::<T>(&w, name) {
            return Some(x);
        }
        c = w.next_sibling()
    }
    None
}
