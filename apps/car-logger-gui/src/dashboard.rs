use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

use car_logger_application::vehicle_dashboard::{
    AggregateInput, DashboardPeriod, DataQuality, aggregate, period_range, previous_range,
};
use car_logger_storage::vehicle_data::{
    DEFAULT_VEHICLE_KEY, PeriodTotals, RefuelCandidate, RefuelStatus, VehicleDataRepository,
};
use chrono::{Datelike, Local, NaiveDate, TimeZone, Utc};
use gtk::prelude::*;
use gtk::{
    ApplicationWindow, Box as GtkBox, Button, ComboBoxText, Dialog, Entry, Grid, Label,
    Orientation, ScrolledWindow, SpinButton,
};

struct DashboardWidgets {
    period: ComboBoxText,
    year: SpinButton,
    pending_box: GtkBox,
    pending_label: Label,
    summary: [Label; 4],
    comparisons: [Label; 4],
    quality: Label,
    monthly: GtkBox,
    analysis: [Label; 4],
    error: Label,
}

pub fn create_dashboard(path: PathBuf, parent: &ApplicationWindow) -> ScrolledWindow {
    let root = GtkBox::new(Orientation::Vertical, 14);
    root.add_css_class("dashboard-root");
    let title = Label::new(Some("燃料・走行ダッシュボード"));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("title-label");
    root.append(&title);
    let subtitle = Label::new(Some("選択中の車両 · 確定済みデータを集計"));
    subtitle.set_halign(gtk::Align::Start);
    subtitle.add_css_class("muted-label");
    root.append(&subtitle);
    let controls = GtkBox::new(Orientation::Horizontal, 8);
    let period = ComboBoxText::new();
    for (id, text) in [
        ("6", "直近6か月"),
        ("12", "直近12か月"),
        ("year", "指定年"),
        ("all", "全期間"),
    ] {
        period.append(Some(id), text)
    }
    period.set_active_id(Some("12"));
    let year = SpinButton::with_range(2000.0, 2100.0, 1.0);
    year.set_value(Local::now().year() as f64);
    controls.append(&period);
    controls.append(&year);
    root.append(&controls);
    let pending_box = GtkBox::new(Orientation::Horizontal, 12);
    pending_box.add_css_class("pending-refuel-card");
    let pending_label = Label::new(None);
    pending_label.set_hexpand(true);
    pending_label.set_halign(gtk::Align::Start);
    let pending_button = Button::with_label("確認する");
    pending_box.append(&pending_label);
    pending_box.append(&pending_button);
    root.append(&pending_box);
    let grid = Grid::new();
    grid.set_column_spacing(12);
    grid.set_row_spacing(12);
    grid.set_column_homogeneous(true);
    let titles = ["燃料費", "走行距離", "平均燃費", "1kmあたり燃料費"];
    let summary = std::array::from_fn(|i| {
        let card = GtkBox::new(Orientation::Vertical, 5);
        card.add_css_class("dashboard-summary-card");
        let h = Label::new(Some(titles[i]));
        h.set_halign(gtk::Align::Start);
        h.add_css_class("muted-label");
        let v = Label::new(Some("—"));
        v.set_halign(gtk::Align::Start);
        v.add_css_class("dashboard-summary-value");
        card.append(&h);
        card.append(&v);
        grid.attach(&card, i as i32, 0, 1, 1);
        v
    });
    let comparisons = std::array::from_fn(|i| {
        let v = Label::new(None);
        v.set_halign(gtk::Align::Start);
        v.add_css_class("summary-comparison");
        grid.child_at(i as i32, 0)
            .unwrap()
            .downcast::<GtkBox>()
            .unwrap()
            .append(&v);
        v
    });
    root.append(&grid);
    let quality = Label::new(None);
    quality.set_halign(gtk::Align::Start);
    quality.add_css_class("muted-label");
    root.append(&quality);
    let monthly = GtkBox::new(Orientation::Vertical, 6);
    let chart_title = Label::new(Some("月別推移 · 燃料費"));
    chart_title.set_halign(gtk::Align::Start);
    chart_title.add_css_class("section-title");
    root.append(&chart_title);
    root.append(&monthly);
    let analysis_grid = Grid::new();
    analysis_grid.set_column_spacing(12);
    analysis_grid.set_column_homogeneous(true);
    let analysis_titles = ["給油総額", "総給油量", "加重平均単価", "1kmあたり燃料費"];
    let analysis = std::array::from_fn(|i| {
        let b = GtkBox::new(Orientation::Vertical, 4);
        b.add_css_class("fuel-analysis-card");
        b.append(&Label::new(Some(analysis_titles[i])));
        let l = Label::new(Some("—"));
        l.add_css_class("fuel-analysis-value");
        b.append(&l);
        analysis_grid.attach(&b, i as i32, 0, 1, 1);
        l
    });
    root.append(&Label::new(Some("燃料分析")));
    root.append(&analysis_grid);
    let error = Label::new(None);
    error.add_css_class("error-label");
    root.append(&error);
    let widgets = Rc::new(DashboardWidgets {
        period: period.clone(),
        year: year.clone(),
        pending_box,
        pending_label,
        summary,
        comparisons,
        quality,
        monthly,
        analysis,
        error,
    });
    refresh(&path, &widgets);
    period.connect_changed({
        let w = widgets.clone();
        let p = path.clone();
        move |_| refresh(&p, &w)
    });
    year.connect_value_changed({
        let w = widgets.clone();
        let p = path.clone();
        move |_| refresh(&p, &w)
    });
    pending_button.connect_clicked({
        let w = widgets.clone();
        let p = path.clone();
        let parent = parent.clone();
        move |_| show_pending(&p, &parent, &w)
    });
    let scroll = ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_child(Some(&root));
    scroll
}

fn selected_period(w: &DashboardWidgets) -> DashboardPeriod {
    match w.period.active_id().as_deref() {
        Some("6") => DashboardPeriod::Last6Months,
        Some("year") => DashboardPeriod::Year(w.year.value_as_int()),
        Some("all") => DashboardPeriod::All,
        _ => DashboardPeriod::Last12Months,
    }
}
fn date_time(date: NaiveDate) -> chrono::DateTime<Utc> {
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}
fn totals_input(t: &PeriodTotals) -> AggregateInput {
    AggregateInput {
        fuel_cost_yen: t.fuel_cost_yen,
        refuel_litres: t.refuel_litres,
        distance_km: t.distance_km,
        consumed_litres: t.consumed_litres,
        quality: if t.has_estimates {
            DataQuality::Estimated
        } else {
            DataQuality::Measured
        },
    }
}
fn refresh(path: &PathBuf, w: &DashboardWidgets) {
    w.error.set_text("");
    let repo = match VehicleDataRepository::open(path) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("dashboard load failed: {e}");
            w.error
                .set_text("データを読み込めませんでした。ほかの機能は引き続き利用できます。");
            return;
        }
    };
    let today = Local::now().date_naive();
    let period = selected_period(w);
    let range = period_range(period, today, None);
    let current = match repo.period_totals(
        DEFAULT_VEHICLE_KEY,
        date_time(range.start),
        date_time(range.end_exclusive),
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("dashboard query failed: {e}");
            w.error
                .set_text("集計に失敗しました。しばらくしてから再試行してください。");
            return;
        }
    };
    let a = aggregate(&[totals_input(&current)]);
    w.summary[0].set_text(&format!("¥{:.0}", a.fuel_cost_yen));
    w.summary[1].set_text(
        &a.distance_km
            .map(|v| format!("{v:.0} km"))
            .unwrap_or_else(|| "データなし".into()),
    );
    w.summary[2].set_text(
        &a.average_efficiency
            .map(|v| format!("{v:.1} km/L"))
            .unwrap_or_else(|| "データなし".into()),
    );
    w.summary[3].set_text(
        &a.cost_per_km
            .map(|v| format!("{v:.1} 円/km"))
            .unwrap_or_else(|| "データなし".into()),
    );
    w.analysis[0].set_text(&format!("¥{:.0}", a.fuel_cost_yen));
    w.analysis[1].set_text(&format!("{:.2} L", a.refuel_litres));
    w.analysis[2].set_text(
        &a.weighted_unit_price
            .map(|v| format!("{v:.1} 円/L"))
            .unwrap_or_else(|| "データなし".into()),
    );
    w.analysis[3].set_text(
        &a.cost_per_km
            .map(|v| format!("{v:.1} 円/km"))
            .unwrap_or_else(|| "データなし".into()),
    );
    if let Some(pr) = previous_range(period, range) {
        if let Ok(prev) = repo.period_totals(
            DEFAULT_VEHICLE_KEY,
            date_time(pr.start),
            date_time(pr.end_exclusive),
        ) {
            let pa = aggregate(&[totals_input(&prev)]);
            let current_values = [
                Some(a.fuel_cost_yen),
                a.distance_km,
                a.average_efficiency,
                a.cost_per_km,
            ];
            let previous_values = [
                Some(pa.fuel_cost_yen),
                pa.distance_km,
                pa.average_efficiency,
                pa.cost_per_km,
            ];
            for i in 0..4 {
                w.comparisons[i].set_text(&compare(
                    current_values[i],
                    previous_values[i],
                    i,
                    range.in_progress,
                ));
            }
        }
    } else {
        for l in &w.comparisons {
            l.set_text("")
        }
    }
    w.quality.set_text(if current.pending_count > 0 {
        "一部推定または暫定値 · 未確認データあり"
    } else if current.has_estimates {
        "一部推定"
    } else if a.distance_km.is_none() && a.refuel_litres == 0.0 {
        "走行データがまだありません · 給油を検知すると、ここに記録されます"
    } else {
        "実測"
    });
    w.pending_box.set_visible(current.pending_count > 0);
    w.pending_label
        .set_text(&format!("未確認の給油  {}件", current.pending_count));
    render_months(&repo, w, range.start, range.end_exclusive, today);
}
fn compare(now: Option<f64>, before: Option<f64>, kind: usize, in_progress: bool) -> String {
    let suffix = if in_progress { " · 集計途中" } else { "" };
    match (now, before) {
        (Some(n), Some(b)) if kind < 2 && b.abs() > f64::EPSILON => {
            format!("前期間比 {:+.0}%{suffix}", (n - b) / b * 100.0)
        }
        (Some(n), Some(b)) if kind == 2 => format!("前期間比 {:+.1} km/L{suffix}", n - b),
        (Some(n), Some(b)) => format!("前期間比 {:+.1} 円/km{suffix}", n - b),
        _ => "比較データなし".into(),
    }
}
fn render_months(
    repo: &VehicleDataRepository,
    w: &DashboardWidgets,
    mut month: NaiveDate,
    end: NaiveDate,
    today: NaiveDate,
) {
    while let Some(child) = w.monthly.first_child() {
        w.monthly.remove(&child)
    }
    while month < end {
        let next = if month.month() == 12 {
            NaiveDate::from_ymd_opt(month.year() + 1, 1, 1).unwrap()
        } else {
            NaiveDate::from_ymd_opt(month.year(), month.month() + 1, 1).unwrap()
        };
        let t = repo
            .period_totals(DEFAULT_VEHICLE_KEY, date_time(month), date_time(next))
            .unwrap_or_default();
        let row = Label::new(Some(&format!(
            "{}    {}{}",
            month.format("%Y-%m"),
            if t.refuel_litres > 0.0 {
                format!("¥{:.0}", t.fuel_cost_yen)
            } else {
                "データなし".into()
            },
            if t.pending_count > 0 {
                "  ● 未確認"
            } else if month.year() == today.year() && month.month() == today.month() {
                "  集計途中"
            } else {
                ""
            }
        )));
        row.set_halign(gtk::Align::Start);
        row.add_css_class("monthly-row");
        w.monthly.append(&row);
        month = next
    }
}

fn show_pending(path: &PathBuf, parent: &ApplicationWindow, w: &Rc<DashboardWidgets>) {
    let repo = match VehicleDataRepository::open(path) {
        Ok(v) => v,
        Err(_) => return,
    };
    let rows = repo
        .pending_candidates(DEFAULT_VEHICLE_KEY)
        .unwrap_or_default();
    let dialog = Dialog::builder()
        .title("未確認の給油")
        .transient_for(parent)
        .modal(true)
        .default_width(520)
        .build();
    dialog.add_button("閉じる", gtk::ResponseType::Close);
    let list = GtkBox::new(Orientation::Vertical, 10);
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);
    for candidate in rows {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        let text = Label::new(Some(&format!(
            "{}  {:.1}% → {:.1}%  {}",
            candidate
                .detected_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M"),
            candidate.before_percent,
            candidate.after_percent,
            candidate
                .estimated_litres
                .map(|v| format!("推定 {v:.1} L"))
                .unwrap_or_else(|| "給油量を入力".into())
        )));
        text.set_hexpand(true);
        text.set_halign(gtk::Align::Start);
        let enter = Button::with_label("入力する");
        let reject = Button::with_label("給油ではない");
        row.append(&text);
        row.append(&enter);
        row.append(&reject);
        list.append(&row);
        enter.connect_clicked({
            let p = path.clone();
            let parent = parent.clone();
            let c = candidate.clone();
            let d = dialog.clone();
            let w = w.clone();
            move |_| show_confirm(&p, &parent, &c, &d, &w)
        });
        reject.connect_clicked({
            let p = path.clone();
            let id = candidate.id;
            let row = row.clone();
            move |_| {
                if let Ok(r) = VehicleDataRepository::open(&p)
                    && r.set_candidate_status(id, RefuelStatus::Rejected).is_ok()
                {
                    row.set_visible(false)
                }
            }
        });
    }
    dialog.content_area().append(&list);
    dialog.connect_response(|d, _| d.close());
    dialog.present()
}
fn show_confirm(
    path: &Path,
    parent: &ApplicationWindow,
    c: &RefuelCandidate,
    list_dialog: &Dialog,
    w: &Rc<DashboardWidgets>,
) {
    let candidate_id = c.id;
    let d = Dialog::builder()
        .title("給油を確認")
        .transient_for(parent)
        .modal(true)
        .build();
    d.add_button("あとで", gtk::ResponseType::Cancel);
    d.add_button("保存", gtk::ResponseType::Accept);
    let form = GtkBox::new(Orientation::Vertical, 8);
    form.set_margin_top(12);
    form.set_margin_bottom(12);
    form.set_margin_start(12);
    form.set_margin_end(12);
    form.append(&Label::new(Some(&format!(
        "検知日時: {}",
        c.detected_at.with_timezone(&Local).format("%Y-%m-%d %H:%M")
    ))));
    let litres = Entry::new();
    litres.set_placeholder_text(Some("給油量 (L)"));
    if let Some(v) = c.estimated_litres {
        litres.set_text(&format!("{v:.1}"))
    }
    let price = Entry::new();
    price.set_placeholder_text(Some("1Lあたりの金額 (円)"));
    let total = Label::new(Some("合計金額は保存時に計算します"));
    form.append(&litres);
    form.append(&price);
    form.append(&total);
    d.content_area().append(&form);
    d.connect_response({
        let p = path.to_path_buf();
        let w = w.clone();
        let list = list_dialog.clone();
        move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                let parsed = litres
                    .text()
                    .parse::<f64>()
                    .ok()
                    .zip(price.text().parse::<f64>().ok());
                if let Some((l, pv)) = parsed {
                    if let Ok(mut repo) = VehicleDataRepository::open(&p) {
                        match repo.confirm_candidate(candidate_id, l, pv) {
                            Ok(_) => {
                                dialog.close();
                                list.close();
                                refresh(&p, &w);
                            }
                            Err(e) => total.set_text(&e.to_string()),
                        }
                    }
                } else {
                    total.set_text("給油量と単価を数値で入力してください")
                }
            } else if let Ok(repo) = VehicleDataRepository::open(&p) {
                let _ = repo.set_candidate_status(candidate_id, RefuelStatus::Deferred);
                dialog.close()
            }
        }
    });
    d.present()
}
