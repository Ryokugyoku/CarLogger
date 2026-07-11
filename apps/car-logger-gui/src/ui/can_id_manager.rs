use crate::ui::TranslationManager;
use car_logger_domain::{CanIdObservation, SignalDefinition, SignalKind};
use car_logger_storage::SqliteCanFrameRepository;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Entry, Grid, Label, ScrolledWindow, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub struct CanIdManagerView {
    root: ScrolledWindow,
}

impl CanIdManagerView {
    pub fn setup(
        builder: &gtk::Builder,
        translation_manager: Rc<RefCell<TranslationManager>>,
        repository: Option<Rc<SqliteCanFrameRepository>>,
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
        let refresh_button: Button = builder
            .object("btn_refresh_can_ids")
            .expect("Could not find btn_refresh_can_ids");

        for (id, msgid) in [
            ("lbl_can_id_manager_title", "CAN ID Manager"),
            (
                "lbl_can_id_manager_caption",
                "Review known definitions and promote unknown CAN IDs.",
            ),
            ("lbl_known_can_ids", "Known CAN IDs"),
            ("lbl_unknown_can_ids", "Unknown CAN IDs"),
        ] {
            if let Some(label) = builder.object::<Label>(id) {
                translation_manager.borrow_mut().add(label, msgid);
            }
        }

        refresh_lists(&known_list, &unknown_list, repository.clone());

        refresh_button.connect_clicked(glib::clone!(
            #[strong]
            known_list,
            #[strong]
            unknown_list,
            #[strong]
            repository,
            move |_| {
                refresh_lists(&known_list, &unknown_list, repository.clone());
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
    repository: Option<Rc<SqliteCanFrameRepository>>,
) {
    clear_box(known_list);
    clear_box(unknown_list);

    let Some(repository) = repository else {
        known_list.append(&empty_label("Repository is unavailable"));
        unknown_list.append(&empty_label("Repository is unavailable"));
        return;
    };

    match repository.list_can_signal_definitions() {
        Ok(definitions) if definitions.is_empty() => {
            known_list.append(&empty_label("No known CAN IDs"));
        }
        Ok(definitions) => {
            for definition in definitions {
                known_list.append(&known_row(
                    definition,
                    repository.clone(),
                    known_list,
                    unknown_list,
                ));
            }
        }
        Err(error) => known_list.append(&empty_label(&format!("Failed to load: {error}"))),
    }

    match repository.list_unknown_can_id_observations() {
        Ok(observations) if observations.is_empty() => {
            unknown_list.append(&empty_label("No unknown CAN IDs"));
        }
        Ok(observations) => {
            for observation in observations {
                unknown_list.append(&unknown_row(
                    observation,
                    repository.clone(),
                    known_list,
                    unknown_list,
                ));
            }
        }
        Err(error) => unknown_list.append(&empty_label(&format!("Failed to load: {error}"))),
    }
}

fn known_row(
    definition: SignalDefinition,
    repository: Rc<SqliteCanFrameRepository>,
    known_list: &GtkBox,
    unknown_list: &GtkBox,
) -> Grid {
    let row = manager_row_grid();
    row.attach(&mono_label(&format_can_id(definition.id)), 0, 0, 1, 1);

    let name_entry = manager_entry(&definition.name, 220);
    let formula_entry = manager_entry(&definition.formula, 320);
    let save_button = row_button("Save");

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
        name_entry,
        #[strong]
        formula_entry,
        move |_| {
            let definition = SignalDefinition {
                kind: SignalKind::CanId,
                id: definition.id,
                name: name_entry.text().to_string(),
                unit: definition.unit.clone(),
                formula: formula_entry.text().to_string(),
            };
            if let Err(error) = repository.upsert_signal_definition(&definition) {
                tracing::error!("Failed to save CAN ID definition: {error}");
            }
            refresh_lists(&known_list, &unknown_list, Some(repository.clone()));
        }
    ));

    row
}

fn unknown_row(
    observation: CanIdObservation,
    repository: Rc<SqliteCanFrameRepository>,
    known_list: &GtkBox,
    unknown_list: &GtkBox,
) -> Grid {
    let row = manager_row_grid();
    row.attach(&mono_label(&format_can_id(observation.id)), 0, 0, 1, 1);
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
    name_entry.set_placeholder_text(Some("Name"));
    let formula_entry = manager_entry("", 260);
    formula_entry.set_placeholder_text(Some("Formula"));
    let save_button = row_button("Promote");
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
        name_entry,
        #[strong]
        formula_entry,
        move |_| {
            let definition = SignalDefinition {
                kind: SignalKind::CanId,
                id: observation.id,
                name: name_entry.text().to_string(),
                unit: None,
                formula: formula_entry.text().to_string(),
            };
            if let Err(error) = repository.upsert_signal_definition(&definition) {
                tracing::error!("Failed to promote unknown CAN ID: {error}");
            }
            refresh_lists(&known_list, &unknown_list, Some(repository.clone()));
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
    let label = Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.add_css_class("table-empty");
    label
}

fn format_can_id(id: u32) -> String {
    format!("0x{id:03X}")
}

fn format_payload(payload: &[u8]) -> String {
    payload
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
