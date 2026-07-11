use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use car_logger_domain::{SignalDefinition, SignalKind};
use car_logger_storage::StorageRepository;
use chrono::{DateTime, Utc};
use gtk::cairo::{Context, FontSlant, FontWeight};
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, CheckButton, DrawingArea, Label, Orientation, Paned,
    ScrolledWindow, glib,
};

use crate::localization::translate;
use crate::signal_decoder::evaluate_formula;
use crate::ui::TranslationManager;

const FRAME_LIMIT: u32 = 8_000;
const DEFAULT_SERIES_LIMIT: usize = 4;
const SERIES_COLORS: &[(f64, f64, f64)] = &[
    (0.00, 0.76, 0.56),
    (1.00, 0.69, 0.13),
    (0.36, 0.68, 1.00),
    (1.00, 0.36, 0.36),
    (0.72, 0.47, 1.00),
    (0.47, 0.86, 0.42),
];

pub struct LogChartsView {
    root: ScrolledWindow,
}

#[derive(Clone)]
struct ChartPoint {
    timestamp: DateTime<Utc>,
    value: f64,
}

#[derive(Clone)]
struct ChartSeries {
    name: String,
    unit: Option<String>,
    color: (f64, f64, f64),
    points: Vec<ChartPoint>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChartScaleMode {
    Compare,
    Absolute,
}

impl ChartSeries {
    fn min_value(&self) -> f64 {
        self.points
            .iter()
            .map(|point| point.value)
            .fold(f64::INFINITY, f64::min)
    }

    fn max_value(&self) -> f64 {
        self.points
            .iter()
            .map(|point| point.value)
            .fold(f64::NEG_INFINITY, f64::max)
    }

    fn latest_value(&self) -> Option<f64> {
        self.points.last().map(|point| point.value)
    }

    fn normalized_value(&self, value: f64) -> f64 {
        let min_value = self.min_value();
        let max_value = self.max_value();
        if (max_value - min_value).abs() < f64::EPSILON {
            50.0
        } else {
            ((value - min_value) / (max_value - min_value)) * 100.0
        }
    }
}

impl LogChartsView {
    pub fn setup(
        translation_manager: Rc<RefCell<TranslationManager>>,
        repository: Option<Rc<StorageRepository>>,
    ) -> Self {
        let root = ScrolledWindow::new();
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_hscrollbar_policy(gtk::PolicyType::Never);

        let page = GtkBox::new(Orientation::Vertical, 18);
        page.add_css_class("dashboard-root");
        root.set_child(Some(&page));

        let header = GtkBox::new(Orientation::Horizontal, 12);
        header.add_css_class("manager-header");
        page.append(&header);

        let title_box = GtkBox::new(Orientation::Vertical, 6);
        title_box.set_hexpand(true);
        header.append(&title_box);

        let title = Label::new(Some(&translate("Data Charts")));
        title.set_halign(Align::Start);
        title.add_css_class("title-label");
        title_box.append(&title);

        let caption = Label::new(Some(&translate(
            "Plot DuckDB log data by known PID or CAN ID definitions.",
        )));
        caption.set_halign(Align::Start);
        caption.add_css_class("muted-label");
        title_box.append(&caption);

        {
            let mut tm = translation_manager.borrow_mut();
            tm.add(title, "Data Charts");
            tm.add(
                caption,
                "Plot DuckDB log data by known PID or CAN ID definitions.",
            );
        }

        let mode_box = GtkBox::new(Orientation::Horizontal, 0);
        mode_box.add_css_class("segmented-control");
        header.append(&mode_box);

        let pid_button = CheckButton::with_label("PID");
        pid_button.add_css_class("segment-button");
        pid_button.set_active(true);
        let can_id_button = CheckButton::with_label("CAN ID");
        can_id_button.add_css_class("segment-button");
        can_id_button.set_group(Some(&pid_button));
        mode_box.append(&pid_button);
        mode_box.append(&can_id_button);

        let scale_box = GtkBox::new(Orientation::Horizontal, 0);
        scale_box.add_css_class("segmented-control");
        header.append(&scale_box);

        let compare_button = CheckButton::with_label(&translate("Compare"));
        compare_button.add_css_class("segment-button");
        compare_button.set_active(true);
        let absolute_button = CheckButton::with_label(&translate("Absolute"));
        absolute_button.add_css_class("segment-button");
        absolute_button.set_group(Some(&compare_button));
        scale_box.append(&compare_button);
        scale_box.append(&absolute_button);

        let refresh_button = Button::with_label(&translate("Refresh"));
        refresh_button.add_css_class("secondary-button");
        header.append(&refresh_button);

        let content = Paned::new(Orientation::Horizontal);
        content.set_wide_handle(true);
        content.set_position(310);
        content.set_hexpand(true);
        content.set_vexpand(true);
        page.append(&content);

        let side_panel = GtkBox::new(Orientation::Vertical, 12);
        side_panel.add_css_class("panel");
        content.set_start_child(Some(&side_panel));

        let signal_title = Label::new(Some(&translate("Known Signals")));
        signal_title.set_halign(Align::Start);
        signal_title.add_css_class("section-title");
        side_panel.append(&signal_title);

        let signal_caption = Label::new(Some(&translate(
            "Select signals to overlay on the time axis.",
        )));
        signal_caption.set_halign(Align::Start);
        signal_caption.set_wrap(true);
        signal_caption.add_css_class("caption-label");
        side_panel.append(&signal_caption);

        let signal_scroll = ScrolledWindow::new();
        signal_scroll.set_vexpand(true);
        signal_scroll.set_min_content_width(260);
        signal_scroll.set_min_content_height(420);
        side_panel.append(&signal_scroll);

        let signal_list = GtkBox::new(Orientation::Vertical, 8);
        signal_scroll.set_child(Some(&signal_list));

        let chart_panel = GtkBox::new(Orientation::Vertical, 12);
        chart_panel.add_css_class("panel");
        content.set_end_child(Some(&chart_panel));

        let chart_header = GtkBox::new(Orientation::Horizontal, 12);
        chart_panel.append(&chart_header);

        let chart_title = Label::new(Some(&translate("Time Series")));
        chart_title.set_halign(Align::Start);
        chart_title.add_css_class("section-title");
        chart_title.set_hexpand(true);
        chart_header.append(&chart_title);

        let status_label = Label::new(Some(&translate("No data loaded")));
        status_label.set_halign(Align::End);
        status_label.add_css_class("muted-label");
        chart_header.append(&status_label);

        let scale_hint = Label::new(Some(&translate(
            "Compare scales each selected signal to 0-100% and keeps actual ranges in the legend.",
        )));
        scale_hint.set_halign(Align::Start);
        scale_hint.set_wrap(true);
        scale_hint.add_css_class("caption-label");
        chart_panel.append(&scale_hint);

        let drawing_area = DrawingArea::new();
        drawing_area.set_content_width(780);
        drawing_area.set_content_height(460);
        drawing_area.set_hexpand(true);
        drawing_area.set_vexpand(true);
        drawing_area.add_css_class("chart-canvas");
        chart_panel.append(&drawing_area);

        {
            let mut tm = translation_manager.borrow_mut();
            tm.add(signal_title, "Known Signals");
            tm.add(
                signal_caption,
                "Select signals to overlay on the time axis.",
            );
            tm.add(chart_title, "Time Series");
            tm.add(
                scale_hint,
                "Compare scales each selected signal to 0-100% and keeps actual ranges in the legend.",
            );
            tm.add_check_button(compare_button.clone(), "Compare");
            tm.add_check_button(absolute_button.clone(), "Absolute");
            tm.add_button(refresh_button.clone(), "Refresh");
            tm.add_redraw_area(drawing_area.clone());
        }

        let mode = Rc::new(RefCell::new(SignalKind::Pid));
        let scale_mode = Rc::new(RefCell::new(ChartScaleMode::Compare));
        let selected_ids = Rc::new(RefCell::new(HashSet::<u32>::new()));
        let series = Rc::new(RefCell::new(Vec::<ChartSeries>::new()));

        drawing_area.set_draw_func(glib::clone!(
            #[strong]
            series,
            #[strong]
            scale_mode,
            move |_, context, width, height| {
                draw_chart(
                    context,
                    width,
                    height,
                    &series.borrow(),
                    *scale_mode.borrow(),
                );
            }
        ));

        rebuild_signal_list(
            &signal_list,
            repository.clone(),
            *mode.borrow(),
            selected_ids.clone(),
            series.clone(),
            drawing_area.clone(),
            status_label.clone(),
        );

        refresh_button.connect_clicked(glib::clone!(
            #[strong]
            signal_list,
            #[strong]
            repository,
            #[strong]
            mode,
            #[strong]
            selected_ids,
            #[strong]
            series,
            #[strong]
            drawing_area,
            #[strong]
            status_label,
            move |_| {
                refresh_chart(
                    repository.clone(),
                    *mode.borrow(),
                    selected_ids.clone(),
                    series.clone(),
                    &drawing_area,
                    &status_label,
                );
                if selected_ids.borrow().is_empty() {
                    rebuild_signal_list(
                        &signal_list,
                        repository.clone(),
                        *mode.borrow(),
                        selected_ids.clone(),
                        series.clone(),
                        drawing_area.clone(),
                        status_label.clone(),
                    );
                }
            }
        ));

        compare_button.connect_toggled(glib::clone!(
            #[strong]
            scale_mode,
            #[strong]
            drawing_area,
            move |button| {
                if button.is_active() {
                    *scale_mode.borrow_mut() = ChartScaleMode::Compare;
                    drawing_area.queue_draw();
                }
            }
        ));

        absolute_button.connect_toggled(glib::clone!(
            #[strong]
            scale_mode,
            #[strong]
            drawing_area,
            move |button| {
                if button.is_active() {
                    *scale_mode.borrow_mut() = ChartScaleMode::Absolute;
                    drawing_area.queue_draw();
                }
            }
        ));

        pid_button.connect_toggled(glib::clone!(
            #[strong]
            signal_list,
            #[strong]
            repository,
            #[strong]
            mode,
            #[strong]
            selected_ids,
            #[strong]
            series,
            #[strong]
            drawing_area,
            #[strong]
            status_label,
            move |button| {
                if button.is_active() {
                    *mode.borrow_mut() = SignalKind::Pid;
                    selected_ids.borrow_mut().clear();
                    rebuild_signal_list(
                        &signal_list,
                        repository.clone(),
                        SignalKind::Pid,
                        selected_ids.clone(),
                        series.clone(),
                        drawing_area.clone(),
                        status_label.clone(),
                    );
                }
            }
        ));

        can_id_button.connect_toggled(glib::clone!(
            #[strong]
            signal_list,
            #[strong]
            repository,
            #[strong]
            mode,
            #[strong]
            selected_ids,
            #[strong]
            series,
            #[strong]
            drawing_area,
            #[strong]
            status_label,
            move |button| {
                if button.is_active() {
                    *mode.borrow_mut() = SignalKind::CanId;
                    selected_ids.borrow_mut().clear();
                    rebuild_signal_list(
                        &signal_list,
                        repository.clone(),
                        SignalKind::CanId,
                        selected_ids.clone(),
                        series.clone(),
                        drawing_area.clone(),
                        status_label.clone(),
                    );
                }
            }
        ));

        Self { root }
    }

    pub fn widget(&self) -> &ScrolledWindow {
        &self.root
    }
}

fn rebuild_signal_list(
    signal_list: &GtkBox,
    repository: Option<Rc<StorageRepository>>,
    kind: SignalKind,
    selected_ids: Rc<RefCell<HashSet<u32>>>,
    series: Rc<RefCell<Vec<ChartSeries>>>,
    drawing_area: DrawingArea,
    status_label: Label,
) {
    clear_box(signal_list);

    let definitions = signal_definitions(repository.clone(), kind);
    if definitions.is_empty() {
        signal_list.append(&empty_label(match kind {
            SignalKind::Pid => "No known PIDs",
            SignalKind::CanId => "No known CAN IDs",
        }));
        series.borrow_mut().clear();
        status_label.set_text(&translate("No known signal definitions"));
        drawing_area.queue_draw();
        return;
    }

    if selected_ids.borrow().is_empty() {
        selected_ids.borrow_mut().extend(
            definitions
                .iter()
                .take(DEFAULT_SERIES_LIMIT)
                .map(|definition| definition.id),
        );
    }

    for definition in definitions {
        let row = CheckButton::with_label(&format!(
            "{}  {}",
            format_signal_id(kind, definition.id),
            definition.name
        ));
        row.add_css_class("signal-check");
        row.set_active(selected_ids.borrow().contains(&definition.id));
        signal_list.append(&row);

        row.connect_toggled(glib::clone!(
            #[strong]
            repository,
            #[strong]
            selected_ids,
            #[strong]
            series,
            #[strong]
            drawing_area,
            #[strong]
            status_label,
            move |button| {
                if button.is_active() {
                    selected_ids.borrow_mut().insert(definition.id);
                } else {
                    selected_ids.borrow_mut().remove(&definition.id);
                }
                refresh_chart(
                    repository.clone(),
                    kind,
                    selected_ids.clone(),
                    series.clone(),
                    &drawing_area,
                    &status_label,
                );
            }
        ));
    }

    refresh_chart(
        repository,
        kind,
        selected_ids,
        series,
        &drawing_area,
        &status_label,
    );
}

fn refresh_chart(
    repository: Option<Rc<StorageRepository>>,
    kind: SignalKind,
    selected_ids: Rc<RefCell<HashSet<u32>>>,
    series: Rc<RefCell<Vec<ChartSeries>>>,
    drawing_area: &DrawingArea,
    status_label: &Label,
) {
    let Some(repository) = repository else {
        series.borrow_mut().clear();
        status_label.set_text(&translate("Repository is unavailable"));
        drawing_area.queue_draw();
        return;
    };

    let selected_ids = selected_ids.borrow().clone();
    if selected_ids.is_empty() {
        series.borrow_mut().clear();
        status_label.set_text(&translate("Select signals"));
        drawing_area.queue_draw();
        return;
    }

    let definitions = signal_definitions(Some(repository.clone()), kind)
        .into_iter()
        .filter(|definition| selected_ids.contains(&definition.id))
        .collect::<Vec<_>>();

    match repository.list_recent_log_frames(FRAME_LIMIT) {
        Ok(frames) => {
            let mut chart_series = Vec::new();
            for (index, definition) in definitions.into_iter().enumerate() {
                let points = frames
                    .iter()
                    .filter(|frame| frame.id == definition.id)
                    .filter_map(|frame| {
                        evaluate_formula(&definition.formula, &frame.data).map(|value| ChartPoint {
                            timestamp: frame.received_at,
                            value,
                        })
                    })
                    .collect::<Vec<_>>();

                if !points.is_empty() {
                    chart_series.push(ChartSeries {
                        name: definition.name,
                        unit: definition.unit,
                        color: SERIES_COLORS[index % SERIES_COLORS.len()],
                        points,
                    });
                }
            }

            let point_count = chart_series
                .iter()
                .map(|series| series.points.len())
                .sum::<usize>();
            status_label.set_text(&format!(
                "{} {} / {} {}",
                chart_series.len(),
                translate("series"),
                point_count,
                translate("points"),
            ));
            *series.borrow_mut() = chart_series;
        }
        Err(error) => {
            series.borrow_mut().clear();
            status_label.set_text(&format!("{}: {error}", translate("Failed to load")));
        }
    }

    drawing_area.queue_draw();
}

fn signal_definitions(
    repository: Option<Rc<StorageRepository>>,
    kind: SignalKind,
) -> Vec<SignalDefinition> {
    repository
        .and_then(|repository| repository.list_signal_definitions().ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|definition| definition.kind == kind)
        .collect()
}

fn draw_chart(
    context: &Context,
    width: i32,
    height: i32,
    series: &[ChartSeries],
    scale_mode: ChartScaleMode,
) {
    let width = f64::from(width);
    let height = f64::from(height);
    let left = 64.0;
    let right = 24.0;
    let top = 34.0;
    let bottom = 104.0;
    let plot_width = (width - left - right).max(1.0);
    let plot_height = (height - top - bottom).max(1.0);

    context.set_source_rgb(0.08, 0.09, 0.11);
    let _ = context.paint();

    context.set_source_rgb(0.18, 0.22, 0.26);
    context.rectangle(left, top, plot_width, plot_height);
    let _ = context.stroke();

    if series.is_empty() {
        draw_text(
            context,
            &translate("No selected signal data"),
            left + 18.0,
            top + 34.0,
            13.0,
        );
        return;
    }

    let mut min_time = i64::MAX;
    let mut max_time = i64::MIN;
    for point in series.iter().flat_map(|series| series.points.iter()) {
        let timestamp = point.timestamp.timestamp_millis();
        min_time = min_time.min(timestamp);
        max_time = max_time.max(timestamp);
    }

    if min_time == max_time {
        max_time += 1;
    }

    let (min_value, max_value) = match scale_mode {
        ChartScaleMode::Compare => (0.0, 100.0),
        ChartScaleMode::Absolute => absolute_value_range(series),
    };

    draw_grid(context, left, top, plot_width, plot_height);

    for item in series {
        context.set_source_rgb(item.color.0, item.color.1, item.color.2);
        context.set_line_width(2.0);

        for (index, point) in item.points.iter().enumerate() {
            let x = left
                + ((point.timestamp.timestamp_millis() - min_time) as f64
                    / (max_time - min_time) as f64)
                    * plot_width;
            let plotted_value = match scale_mode {
                ChartScaleMode::Compare => item.normalized_value(point.value),
                ChartScaleMode::Absolute => point.value,
            };
            let y =
                top + (1.0 - ((plotted_value - min_value) / (max_value - min_value))) * plot_height;

            if index == 0 {
                context.move_to(x, y);
            } else {
                context.line_to(x, y);
            }
        }

        let _ = context.stroke();
    }

    context.set_source_rgb(0.62, 0.68, 0.72);
    match scale_mode {
        ChartScaleMode::Compare => {
            draw_text(context, "100%", 16.0, top + 8.0, 10.0);
            draw_text(context, "50%", 22.0, top + plot_height / 2.0, 10.0);
            draw_text(context, "0%", 28.0, top + plot_height, 10.0);
            draw_text(
                context,
                &translate("normalized per signal"),
                left,
                top - 12.0,
                10.0,
            );
        }
        ChartScaleMode::Absolute => {
            draw_text(context, &format!("{max_value:.1}"), 12.0, top + 8.0, 10.0);
            draw_text(
                context,
                &format!("{min_value:.1}"),
                12.0,
                top + plot_height,
                10.0,
            );
            draw_text(
                context,
                &translate("absolute values"),
                left,
                top - 12.0,
                10.0,
            );
        }
    }

    let start = DateTime::<Utc>::from_timestamp_millis(min_time)
        .map(|value| value.format("%H:%M:%S").to_string())
        .unwrap_or_default();
    let end = DateTime::<Utc>::from_timestamp_millis(max_time)
        .map(|value| value.format("%H:%M:%S").to_string())
        .unwrap_or_default();
    draw_text(context, &start, left, height - 22.0, 10.0);
    draw_text(context, &end, width - right - 74.0, height - 22.0, 10.0);

    let mut legend_x = left;
    let mut legend_y = height - 64.0;
    for (index, item) in series.iter().take(6).enumerate() {
        if index == 3 {
            legend_x = left;
            legend_y += 26.0;
        }
        context.set_source_rgb(item.color.0, item.color.1, item.color.2);
        context.rectangle(legend_x, legend_y - 8.0, 14.0, 3.0);
        let _ = context.fill();
        context.set_source_rgb(0.82, 0.87, 0.90);
        let label = series_legend_label(item, scale_mode);
        draw_text(context, &label, legend_x + 20.0, legend_y, 10.0);
        legend_x += 225.0;
    }
}

fn absolute_value_range(series: &[ChartSeries]) -> (f64, f64) {
    let mut min_value = f64::INFINITY;
    let mut max_value = f64::NEG_INFINITY;

    for point in series.iter().flat_map(|series| series.points.iter()) {
        min_value = min_value.min(point.value);
        max_value = max_value.max(point.value);
    }

    if (max_value - min_value).abs() < f64::EPSILON {
        (min_value - 1.0, max_value + 1.0)
    } else {
        let padding = (max_value - min_value) * 0.06;
        (min_value - padding, max_value + padding)
    }
}

fn series_legend_label(series: &ChartSeries, scale_mode: ChartScaleMode) -> String {
    let unit = series
        .unit
        .as_deref()
        .filter(|unit| !unit.is_empty())
        .unwrap_or("");
    let latest = series.latest_value().unwrap_or_default();

    match scale_mode {
        ChartScaleMode::Compare => format!(
            "{}  {:.1}-{:.1}{}  {} {:.1}{}",
            series.name,
            series.min_value(),
            series.max_value(),
            unit,
            translate("now"),
            latest,
            unit
        ),
        ChartScaleMode::Absolute => {
            if unit.is_empty() {
                format!("{}  {} {:.1}", series.name, translate("now"), latest)
            } else {
                format!(
                    "{} ({unit})  {} {:.1}{unit}",
                    series.name,
                    translate("now"),
                    latest
                )
            }
        }
    }
}

fn draw_grid(context: &Context, left: f64, top: f64, width: f64, height: f64) {
    context.set_source_rgb(0.14, 0.17, 0.20);
    context.set_line_width(1.0);

    for index in 1..5 {
        let y = top + height * f64::from(index) / 5.0;
        context.move_to(left, y);
        context.line_to(left + width, y);
    }

    for index in 1..6 {
        let x = left + width * f64::from(index) / 6.0;
        context.move_to(x, top);
        context.line_to(x, top + height);
    }

    let _ = context.stroke();
}

fn draw_text(context: &Context, text: &str, x: f64, y: f64, size: f64) {
    context.select_font_face("Sans", FontSlant::Normal, FontWeight::Normal);
    context.set_font_size(size);
    context.move_to(x, y);
    let _ = context.show_text(text);
}

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn empty_label(text: &str) -> Label {
    let label = Label::new(Some(&translate(text)));
    label.set_halign(Align::Start);
    label.add_css_class("table-empty");
    label
}

fn format_signal_id(kind: SignalKind, id: u32) -> String {
    match kind {
        SignalKind::Pid => format!("0x{id:02X}"),
        SignalKind::CanId => format!("0x{id:03X}"),
    }
}
