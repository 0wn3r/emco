//! Progress of one transfer. Cancel stops the worker; Close appears when it is done.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/_0wn3r/Tm02Link/transfer-dialog.ui")]
    pub struct TransferDialog {
        #[template_child]
        pub state_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub progress_bar: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub detail_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub action_button: TemplateChild<gtk::Button>,
        pub cancel: Arc<AtomicBool>,
        pub finished: std::cell::Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TransferDialog {
        const NAME: &'static str = "Tm02TransferDialog";
        type Type = super::TransferDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TransferDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj().downgrade();
            self.action_button.connect_clicked(move |_| {
                if let Some(d) = obj.upgrade() {
                    d.cancel_or_close();
                }
            });
        }
    }
    impl WidgetImpl for TransferDialog {}
    impl AdwDialogImpl for TransferDialog {}
}

glib::wrapper! {
    pub struct TransferDialog(ObjectSubclass<imp::TransferDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TransferDialog {
    pub fn new(title: &str, state: &str, detail: &str) -> Self {
        let d: Self = glib::Object::builder().property("title", title).build();
        d.set_state(state);
        d.set_detail(detail);
        d
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.imp().cancel.clone()
    }

    pub fn set_state(&self, text: &str) {
        self.imp().state_label.set_label(text);
    }

    pub fn set_detail(&self, text: &str) {
        self.imp().detail_label.set_label(text);
        self.imp().detail_label.set_visible(!text.is_empty());
    }

    pub fn set_progress(&self, fraction: Option<f64>, text: &str) {
        let bar = &self.imp().progress_bar;
        match fraction {
            Some(f) => bar.set_fraction(f.clamp(0.0, 1.0)),
            None => bar.pulse(),
        }
        bar.set_text(Some(text));
    }

    pub fn finish(&self, ok: bool, state: &str, detail: &str) {
        let imp = self.imp();
        imp.finished.set(true);
        self.set_state(state);
        self.set_detail(detail);
        if ok {
            imp.progress_bar.set_fraction(1.0);
        } else if imp.progress_bar.fraction() == 0.0 {
            imp.progress_bar.set_visible(false);
        }
        imp.action_button.set_label("_Close");
        imp.action_button.add_css_class("suggested-action");
        self.set_can_close(true);
        imp.action_button.grab_focus();
    }

    fn cancel_or_close(&self) {
        let imp = self.imp();
        if imp.finished.get() {
            self.close();
        } else {
            imp.cancel.store(true, Ordering::Relaxed);
            self.set_state("Stopping…");
            imp.action_button.set_sensitive(false);
        }
    }
}
