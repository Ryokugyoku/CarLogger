use crate::localization::translate;
use crate::ui::TranslationManager;
use car_logger_domain::{CanIdObservation, SignalDefinition, SignalKind};
use car_logger_storage::StorageRepository;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, CheckButton, Entry, Grid, Label, ScrolledWindow, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub struct CanIdManagerView {
    root: ScrolledWindow,
}

impl CanIdManagerView {
    pub fn setup(
        builder: &gtk::Builder,
        translation_manager: Rc<RefCell<TranslationManager>>,
        repository: Option<Rc<StorageRepository>>,
    ) -> Self {
        let root: ScrolledWindow = builder
            .object("can_id_manager_view")
            .expect("Could not find can_id_manager_view");
        let known_list: GtkBox = builder
            .object("known_can_id_list")
            .expect("Could not find known_can_id_list");
        let unknown_list: GtkBox = builder
            .object("unknown_can_id_list")
            .expect("Could not find unknown_can_id_list");
        let mode_box: GtkBox = builder
            .object("id_manager_mode_box")
            .expect("Could not find id_manager_mode_box");
        let refresh_button: Button = builder
            .object("btn_refresh_can_ids")
            .expect("Could not find btn_refresh_can_ids");
        let known_title: Label = builder
            .object("lbl_known_can_ids")
            .expect("Could not find lbl_known_can_ids");
        let unknown_title: Label = builder
            .object("lbl_unknown_can_ids")
            .expect("Could not find lbl_unknown_can_ids");
        let known_id_header: Label = builder
            .object("lbl_known_id_header")
            .expect("Could not find lbl_known_id_header");
        let unknown_id_header: Label = builder
            .object("lbl_unknown_id_header")
            .expect("Could not find lbl_unknown_id_header");

        for (id, msgid) in [
            ("lbl_can_id_manager_title", "ID Manager"),
            (
                "lbl_can_id_manager_caption",
                "Review known definitions and promote unknown IDs.",
            ),
        ] {
            if let Some(label) = builder.object::<Label>(id) {
                translation_manager.borrow_mut().add(label, msgid);
            }
        }

        let mode = Rc::new(RefCell::new(SignalKind::Pid));

        let pid_button = CheckButton::with_label("PID");
        pid_button.add_css_class("segment-button");
        pid_button.set_active(true);
        let can_id_button = CheckButton::with_label("CAN ID");
        can_id_button.add_css_class("segment-button");
        can_id_button.set_group(Some(&pid_button));
        mode_box.append(&pid_button);
        mode_box.append(&can_id_button);

        refresh_lists(
            &known_list,
            &unknown_list,
            &known_title,
            &unknown_title,
            &known_id_header,
            &unknown_id_header,
            SignalKind::Pid,
            repository.clone(),
        );

        refresh_button.connect_clicked(glib::clone!(
            #[strong]
            known_list,
            #[strong]
            unknown_list,
            #[strong]
            known_title,
            #[strong]
            unknown_title,
            #[strong]
            known_id_header,
            #[strong]
            unknown_id_header,
            #[strong]
            mode,
            #[strong]
            repository,
            move |_| {
                refresh_lists(
                    &known_list,
                    &unknown_list,
                    &known_title,
                    &unknown_title,
                    &known_id_header,
                    &unknown_id_header,
                    *mode.borrow(),
                    repository.clone(),
                );
            }
        ));

        pid_button.connect_toggled(glib::clone!(
            #[strong]
            known_list,
            #[strong]
            unknown_list,
            #[strong]
            known_title,
            #[strong]
            unknown_title,
            #[strong]
            known_id_header,
            #[strong]
            unknown_id_header,
            #[strong]
            mode,
            #[strong]
            repository,
            move |button| {
                if button.is_active() {
                    *mode.borrow_mut() = SignalKind::Pid;
                    refresh_lists(
                        &known_list,
                        &unknown_list,
                        &known_title,
                        &unknown_title,
                        &known_id_header,
                        &unknown_id_header,
                        SignalKind::Pid,
                        repository.clone(),
                    );
                }
            }
        ));

        can_id_button.connect_toggled(glib::clone!(
            #[strong]
            known_list,
            #[strong]
            unknown_list,
            #[strong]
            known_title,
            #[strong]
            unknown_title,
            #[strong]
            known_id_header,
            #[strong]
            unknown_id_header,
            #[strong]
            mode,
            #[strong]
            repository,
            move |button| {
                if button.is_active() {
                    *mode.borrow_mut() = SignalKind::CanId;
                    refresh_lists(
                        &known_list,
                        &unknown_list,
                        &known_title,
                        &unknown_title,
                        &known_id_header,
                        &unknown_id_header,
                        SignalKind::CanId,
                        repository.clone(),
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

fn refresh_lists(
    known_list: &GtkBox,
    unknown_list: &GtkBox,
    known_title: &Label,
    unknown_title: &Label,
    known_id_header: &Label,
    unknown_id_header: &Label,
    kind: SignalKind,
    repository: Option<Rc<StorageRepository>>,
) {
    clear_box(known_list);
    clear_box(unknown_list);
    update_mode_labels(
        known_title,
        unknown_title,
        known_id_header,
        unknown_id_header,
        kind,
    );

    let Some(repository) = repository else {
        known_list.append(&empty_label("Repository is unavailable"));
        unknown_list.append(&empty_label("Repository is unavailable"));
        return;
    };

    match repository.list_signal_definitions_by_kind(kind) {
        Ok(definitions) if definitions.is_empty() => {
            known_list.append(&empty_label(empty_known_message(kind)));
        }
        Ok(definitions) => {
            for definition in definitions {
                known_list.append(&known_row(
                    definition,
                    repository.clone(),
                    known_list,
                    unknown_list,
                    known_title,
                    unknown_title,
                    known_id_header,
                    unknown_id_header,
                    kind,
                ));
            }
        }
        Err(error) => known_list.append(&empty_label(&format!(
            "{}: {error}",
            translate("Failed to load")
        ))),
    }

    match repository.list_unknown_observations(kind) {
        Ok(observations) if observations.is_empty() => {
            unknown_list.append(&empty_label(empty_unknown_message(kind)));
        }
        Ok(observations) => {
            for observation in observations {
                unknown_list.append(&unknown_row(
                    observation,
                    repository.clone(),
                    known_list,
                    unknown_list,
                    known_title,
                    unknown_title,
                    known_id_header,
                    unknown_id_header,
                    kind,
                ));
            }
        }
        Err(error) => unknown_list.append(&empty_label(&format!(
            "{}: {error}",
            translate("Failed to load")
        ))),
    }
}

fn known_row(
    definition: SignalDefinition,
    repository: Rc<StorageRepository>,
    known_list: &GtkBox,
    unknown_list: &GtkBox,
    known_title: &Label,
    unknown_title: &Label,
    known_id_header: &Label,
    unknown_id_header: &Label,
    kind: SignalKind,
) -> Grid {
    let row = manager_row_grid();
    row.attach(
        &mono_label(&format_signal_id(kind, definition.id)),
        0,
        0,
        1,
        1,
    );

    let name_entry = manager_entry(&definition.name, 220);
    let formula_entry = manager_entry(&definition.formula, 320);
    let save_button = row_button(&translate("Save"));

    row.attach(&name_entry, 1, 0, 1, 1);
    row.attach(&formula_entry, 2, 0, 1, 1);
    row.attach(&save_button, 3, 0, 1, 1);

    save_button.connect_clicked(glib::clone!(
        #[strong]
        repository,
        #[strong]
        known_list,
        #[strong]
        unknown_list,
        #[strong]
        known_title,
        #[strong]
        unknown_title,
        #[strong]
        known_id_header,
        #[strong]
        unknown_id_header,
        #[strong]
        name_entry,
        #[strong]
        formula_entry,
        move |_| {
            let definition = SignalDefinition {
                kind,
                id: definition.id,
                name: name_entry.text().to_string(),
                unit: definition.unit.clone(),
                formula: formula_entry.text().to_string(),
            };
            if let Err(error) = repository.upsert_signal_definition(&definition) {
                tracing::error!("Failed to save ID definition: {error}");
            }
            refresh_lists(
                &known_list,
                &unknown_list,
                &known_title,
                &unknown_title,
                &known_id_header,
                &unknown_id_header,
                kind,
                Some(repository.clone()),
            );
        }
    ));

    row
}

fn unknown_row(
    observation: CanIdObservation,
    repository: Rc<StorageRepository>,
    known_list: &GtkBox,
    unknown_list: &GtkBox,
    known_title: &Label,
    unknown_title: &Label,
    known_id_header: &Label,
    unknown_id_header: &Label,
    kind: SignalKind,
) -> Grid {
    let row = manager_row_grid();
    row.attach(
        &mono_label(&format_signal_id(kind, observation.id)),
        0,
        0,
        1,
        1,
    );
    row.attach(
        &mono_label(&format_payload(&observation.raw_payload)),
        1,
        0,
        1,
        1,
    );
    row.attach(&mono_label(&observation.count.to_string()), 2, 0, 1, 1);

    let editor_box = GtkBox::new(gtk::Orientation::Horizontal, 8);
    let name_entry = manager_entry("", 200);
    name_entry.set_placeholder_text(Some(&translate("Name")));
    let formula_entry = manager_entry("", 260);
    formula_entry.set_placeholder_text(Some(&translate("Formula")));
    let save_button = row_button(&translate("Promote"));
    editor_box.append(&name_entry);
    editor_box.append(&formula_entry);
    editor_box.append(&save_button);
    row.attach(&editor_box, 3, 0, 1, 1);

    save_button.connect_clicked(glib::clone!(
        #[strong]
        repository,
        #[strong]
        known_list,
        #[strong]
        unknown_list,
        #[strong]
        known_title,
        #[strong]
        unknown_title,
        #[strong]
        known_id_header,
        #[strong]
        unknown_id_header,
        #[strong]
        name_entry,
        #[strong]
        formula_entry,
        move |_| {
            let definition = SignalDefinition {
                kind,
                id: observation.id,
                name: name_entry.text().to_string(),
                unit: None,
                formula: formula_entry.text().to_string(),
            };
            if let Err(error) = repository.upsert_signal_definition(&definition) {
                tracing::error!("Failed to promote unknown ID: {error}");
            }
            refresh_lists(
                &known_list,
                &unknown_list,
                &known_title,
                &unknown_title,
                &known_id_header,
                &unknown_id_header,
                kind,
                Some(repository.clone()),
            );
        }
    ));

    row
}

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn manager_row_grid() -> Grid {
    let row = Grid::new();
    row.set_column_spacing(12);
    row.set_row_spacing(8);
    row.add_css_class("manager-row");
    row
}

fn manager_entry(text: &str, width: i32) -> Entry {
    let entry = Entry::new();
    entry.set_text(text);
    entry.set_width_request(width);
    entry.add_css_class("manager-entry");
    entry
}

fn row_button(label: &str) -> Button {
    let button = Button::with_label(label);
    button.add_css_class("small-action-button");
    button
}

fn mono_label(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.add_css_class("mono-cell");
    label
}

fn empty_label(text: &str) -> Label {
    let label = Label::new(Some(&translate(text)));
    label.set_halign(gtk::Align::Start);
    label.add_css_class("table-empty");
    label
}

fn update_mode_labels(
    known_title: &Label,
    unknown_title: &Label,
    known_id_header: &Label,
    unknown_id_header: &Label,
    kind: SignalKind,
) {
    known_title.set_text(&translate(known_title_message(kind)));
    unknown_title.set_text(&translate(unknown_title_message(kind)));
    known_id_header.set_text(id_header_message(kind));
    unknown_id_header.set_text(id_header_message(kind));
}

fn known_title_message(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::Pid => "Known PIDs",
        SignalKind::CanId => "Known CAN IDs",
    }
}

fn unknown_title_message(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::Pid => "Unknown PIDs",
        SignalKind::CanId => "Unknown CAN IDs",
    }
}

fn empty_known_message(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::Pid => "No known PIDs",
        SignalKind::CanId => "No known CAN IDs",
    }
}

fn empty_unknown_message(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::Pid => "No unknown PIDs",
        SignalKind::CanId => "No unknown CAN IDs",
    }
}

fn id_header_message(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::Pid => "PID",
        SignalKind::CanId => "CAN ID",
    }
}

fn format_signal_id(kind: SignalKind, id: u32) -> String {
    match kind {
        SignalKind::Pid => format!("0x{id:02X}"),
        SignalKind::CanId => format!("0x{id:03X}"),
    }
}

fn format_payload(payload: &[u8]) -> String {
    payload
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
