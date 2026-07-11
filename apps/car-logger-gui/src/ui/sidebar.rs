use crate::ui::TranslationManager;
use gtk::prelude::*;
use gtk::{Box as GtkBox, EventControllerMotion, Label, Stack, ToggleButton, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub struct Sidebar {
    root: GtkBox,
}

impl Sidebar {
    pub fn setup(
        builder: &gtk::Builder,
        main_stack: Stack,
        translation_manager: Rc<RefCell<TranslationManager>>,
    ) -> Self {
        let root: GtkBox = builder
            .object("side_bar_root")
            .expect("Could not find side_bar_root");

        // サイドバーの各ボタンを取得
        let btn_dashboard: ToggleButton = builder
            .object("btn_dashboard")
            .expect("Could not find btn_dashboard");
        let btn_logs: ToggleButton = builder.object("btn_logs").expect("Could not find btn_logs");
        let btn_charts: ToggleButton = builder
            .object("btn_charts")
            .expect("Could not find btn_charts");
        let btn_maintenance: ToggleButton = builder
            .object("btn_maintenance")
            .expect("Could not find btn_maintenance");
        let btn_can_ids: ToggleButton = builder
            .object("btn_can_ids")
            .expect("Could not find btn_can_ids");
        let btn_settings: ToggleButton = builder
            .object("btn_settings")
            .expect("Could not find btn_settings");

        let lbl_dashboard: Label = builder
            .object("lbl_dashboard")
            .expect("Could not find lbl_dashboard");
        let lbl_logs: Label = builder.object("lbl_logs").expect("Could not find lbl_logs");
        let lbl_charts: Label = builder
            .object("lbl_charts")
            .expect("Could not find lbl_charts");
        let lbl_maintenance: Label = builder
            .object("lbl_maintenance")
            .expect("Could not find lbl_maintenance");
        let lbl_can_ids: Label = builder
            .object("lbl_can_ids")
            .expect("Could not find lbl_can_ids");
        let lbl_settings: Label = builder
            .object("lbl_settings")
            .expect("Could not find lbl_settings");

        {
            let mut tm = translation_manager.borrow_mut();
            tm.add(lbl_dashboard.clone(), "Dashboard");
            tm.add(lbl_logs.clone(), "Log Analysis");
            tm.add(lbl_charts.clone(), "Data Charts");
            tm.add(lbl_maintenance.clone(), "Maintenance");
            tm.add(lbl_can_ids.clone(), "IDs");
            tm.add(lbl_settings.clone(), "Settings");
        }

        let labels = vec![
            lbl_dashboard,
            lbl_logs,
            lbl_charts,
            lbl_maintenance,
            lbl_can_ids,
            lbl_settings,
        ];

        // ホバーによる拡大・縮小ロジック
        let motion_controller = EventControllerMotion::new();
        root.add_controller(motion_controller.clone());

        motion_controller.connect_enter(glib::clone!(
            #[strong]
            root,
            #[strong]
            labels,
            move |_, _, _| {
                root.set_width_request(200);
                for lbl in &labels {
                    lbl.set_visible(true);
                }
            }
        ));

        motion_controller.connect_leave(glib::clone!(
            #[strong]
            root,
            #[strong]
            labels,
            move |_| {
                root.set_width_request(60);
                for lbl in &labels {
                    lbl.set_visible(false);
                }
            }
        ));

        let buttons = vec![
            (btn_dashboard.clone(), "dashboard"),
            (btn_logs.clone(), "logs"),
            (btn_charts.clone(), "charts"),
            (btn_maintenance.clone(), "maintenance"),
            (btn_can_ids.clone(), "can_ids"),
            (btn_settings.clone(), "settings"),
        ];

        // ボタンの状態管理と遷移
        for (btn, name) in buttons.clone() {
            btn.connect_toggled(glib::clone!(
                #[strong]
                main_stack,
                #[strong]
                buttons,
                move |clicked_btn| {
                    if clicked_btn.is_active() {
                        main_stack.set_visible_child_name(name);
                        // 他のボタンを解除
                        for (other_btn, other_name) in &buttons {
                            if *other_name != name {
                                other_btn.set_active(false);
                            }
                        }
                    }
                }
            ));
        }

        // デフォルトでダッシュボードを選択
        btn_dashboard.set_active(true);

        Self { root }
    }

    pub fn widget(&self) -> &GtkBox {
        &self.root
    }
}
