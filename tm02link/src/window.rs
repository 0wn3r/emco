//! Main window: Send, Receive and Connection pages. Transfers run in a worker thread and report
//! back over a channel; the transfer dialog shows progress and can cancel.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, gio, glib};
use serialport::SerialPort;

use crate::link::{self, Handshake, ParityKind, Prepared, ReceiveMode, ReceivedProgram};
use crate::settings::Settings;
use crate::transfer_dialog::TransferDialog;

const BAUDS: [u32; 7] = [150, 300, 600, 1200, 2400, 4800, 9600];

enum Msg {
    Opened,
    Sent(usize, usize),
    Received(usize),
    Done(Result<Vec<u8>, String>),
}

pub struct Program {
    text: String,
}

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/_0wn3r/Tm02Link/window.ui")]
    pub struct Window {
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub send_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub file_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub number_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub ctrlz_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub preview_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub preview_view: TemplateChild<gtk::TextView>,
        #[template_child]
        pub several_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub received_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub received_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub port_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub cts_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub dsr_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub cd_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub baud_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub bits_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub parity_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub stop_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub handshake_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub idle_row: TemplateChild<adw::SpinRow>,

        pub settings: RefCell<Settings>,
        pub program: RefCell<Option<Program>>,
        pub prepared: RefCell<Option<Prepared>>,
        pub received: RefCell<Vec<ReceivedProgram>>,
        pub received_raw: RefCell<Vec<u8>>,
        pub ports: RefCell<Vec<String>>,
        pub busy: Cell<bool>,
        pub loading: Cell<bool>,
        pub watch_port: RefCell<Option<Box<dyn SerialPort>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "Tm02Window";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.install_action_async("win.open", None, |win, _, _| async move { win.open().await });
            klass.install_action_async("win.send", None, |win, _, _| async move { win.send().await });
            klass.install_action_async("win.receive", None, |win, _, _| async move { win.receive().await });
            klass.install_action_async("win.save-all", None, |win, _, _| async move { win.save_all().await });
            klass.install_action_async("win.loopback", None, |win, _, _| async move { win.loopback().await });
            klass.install_action("win.rescan", None, |win, _, _| win.rescan_ports());
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Window {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }
    }

    impl WidgetImpl for Window {}
    impl WindowImpl for Window {
        fn close_request(&self) -> glib::Propagation {
            self.settings.borrow().save();
            self.parent_close_request()
        }
    }
    impl ApplicationWindowImpl for Window {}
    impl AdwApplicationWindowImpl for Window {}
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl Window {
    pub fn present_for(app: &adw::Application) -> Self {
        let win = app.active_window().and_downcast::<Window>().unwrap_or_else(|| {
            glib::Object::builder().property("application", app).build()
        });
        win.present();
        win
    }

    fn toast(&self, text: &str) {
        let t = adw::Toast::new(text);
        t.set_timeout(5);
        self.imp().toast_overlay.add_toast(t);
    }

    fn setup(&self) {
        let imp = self.imp();
        *imp.settings.borrow_mut() = Settings::load();
        imp.loading.set(true);
        {
            let s = imp.settings.borrow();
            imp.ctrlz_row.set_active(s.ctrl_z);
            imp.several_row.set_active(s.several);
            imp.idle_row.set_value(s.idle_secs as f64);
            let p = &s.port;
            imp.baud_row.set_selected(BAUDS.iter().position(|&b| b == p.baud).unwrap_or(4) as u32);
            imp.bits_row.set_selected(u32::from(p.data_bits == 8));
            imp.parity_row.set_selected(match p.parity {
                ParityKind::Even => 0,
                ParityKind::Odd => 1,
                ParityKind::None => 2,
            });
            imp.stop_row.set_selected(u32::from(p.stop_bits == 2));
            imp.handshake_row.set_selected(match p.handshake {
                Handshake::RtsCts => 0,
                Handshake::XonXoff => 1,
                Handshake::None => 2,
            });
        }
        self.rescan_ports();
        imp.loading.set(false);

        for row in [&*imp.baud_row, &*imp.bits_row, &*imp.parity_row, &*imp.stop_row, &*imp.handshake_row] {
            row.connect_selected_notify(glib::clone!(#[weak(rename_to = win)] self, move |_| win.read_port_rows()));
        }
        imp.port_row.connect_selected_notify(glib::clone!(#[weak(rename_to = win)] self, move |_| win.read_port_rows()));
        imp.idle_row.connect_value_notify(glib::clone!(#[weak(rename_to = win)] self, move |r| {
            win.imp().settings.borrow_mut().idle_secs = r.value() as u32;
            win.save_settings();
        }));
        imp.several_row.connect_active_notify(glib::clone!(#[weak(rename_to = win)] self, move |r| {
            win.imp().settings.borrow_mut().several = r.is_active();
            win.save_settings();
        }));
        imp.ctrlz_row.connect_active_notify(glib::clone!(#[weak(rename_to = win)] self, move |r| {
            win.imp().settings.borrow_mut().ctrl_z = r.is_active();
            win.save_settings();
            win.update_program();
        }));
        imp.number_row.connect_value_notify(glib::clone!(#[weak(rename_to = win)] self, move |_| win.update_program()));

        // Watch the control lines only while the Connection page is shown.
        imp.stack.connect_visible_child_name_notify(glib::clone!(#[weak(rename_to = win)] self, move |_| win.update_watch()));
        glib::timeout_add_local(Duration::from_millis(400), glib::clone!(#[weak(rename_to = win)] self, #[upgrade_or] glib::ControlFlow::Break, move || {
            win.poll_lines();
            glib::ControlFlow::Continue
        }));

        self.action_set_enabled("win.send", false);
        self.action_set_enabled("win.save-all", false);
    }

    fn save_settings(&self) {
        if !self.imp().loading.get() {
            self.imp().settings.borrow().save();
        }
    }

    fn rescan_ports(&self) {
        let imp = self.imp();
        let was_loading = imp.loading.replace(true);
        let current = imp.settings.borrow().port.path.clone();
        let mut found = link::list_ports();
        if !found.iter().any(|(p, _)| *p == current) {
            found.insert(0, (current.clone(), "Not connected".into()));
        }
        let labels: Vec<String> = found.iter().map(|(p, l)| format!("{l} ({p})")).collect();
        let model = gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        imp.port_row.set_model(Some(&model));
        imp.port_row.set_selected(found.iter().position(|(p, _)| *p == current).unwrap_or(0) as u32);
        *imp.ports.borrow_mut() = found.into_iter().map(|(p, _)| p).collect();
        imp.loading.set(was_loading);
        *imp.watch_port.borrow_mut() = None;
        self.update_watch();
    }

    fn read_port_rows(&self) {
        let imp = self.imp();
        if imp.loading.get() {
            return;
        }
        {
            let mut s = imp.settings.borrow_mut();
            let p = &mut s.port;
            if let Some(path) = imp.ports.borrow().get(imp.port_row.selected() as usize) {
                p.path = path.clone();
            }
            p.baud = BAUDS[(imp.baud_row.selected() as usize).min(BAUDS.len() - 1)];
            p.data_bits = if imp.bits_row.selected() == 1 { 8 } else { 7 };
            p.parity = [ParityKind::Even, ParityKind::Odd, ParityKind::None][imp.parity_row.selected().min(2) as usize];
            p.stop_bits = if imp.stop_row.selected() == 1 { 2 } else { 1 };
            p.handshake = [Handshake::RtsCts, Handshake::XonXoff, Handshake::None][imp.handshake_row.selected().min(2) as usize];
        }
        self.save_settings();
        *imp.watch_port.borrow_mut() = None;
        self.update_watch();
    }

    fn update_watch(&self) {
        let imp = self.imp();
        let on_page = imp.stack.visible_child_name().as_deref() == Some("connection");
        if !on_page || imp.busy.get() {
            *imp.watch_port.borrow_mut() = None;
            self.show_lines(None);
            return;
        }
        if imp.watch_port.borrow().is_none() {
            let cfg = imp.settings.borrow().port.clone();
            *imp.watch_port.borrow_mut() = link::open(&cfg).ok();
        }
    }

    fn poll_lines(&self) {
        let imp = self.imp();
        let lines = imp.watch_port.borrow_mut().as_mut().and_then(|p| link::modem_lines(&mut **p));
        if imp.watch_port.borrow().is_some() || lines.is_some() {
            self.show_lines(lines);
        }
    }

    fn show_lines(&self, lines: Option<link::ModemLines>) {
        let imp = self.imp();
        for (label, on) in [
            (&*imp.cts_label, lines.map(|l| l.cts)),
            (&*imp.dsr_label, lines.map(|l| l.dsr)),
            (&*imp.cd_label, lines.map(|l| l.cd)),
        ] {
            if on == Some(true) {
                label.add_css_class("on");
            } else {
                label.remove_css_class("on");
            }
            let name = label.label();
            label.set_tooltip_text(Some(&match on {
                Some(true) => format!("{name} on"),
                Some(false) => format!("{name} off"),
                None => format!("{name} unknown: the port is closed or busy"),
            }));
        }
    }

    // ---- Send ----

    async fn open(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("NC Programs"));
        for pat in ["*.nc", "*.NC", "*.txt", "*.TXT", "O*", "[0-9]*"] {
            filter.add_pattern(pat);
        }
        let all = gtk::FileFilter::new();
        all.set_name(Some("All Files"));
        all.add_pattern("*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        filters.append(&all);
        let dialog = gtk::FileDialog::builder().title("Open Program").modal(true).filters(&filters).build();
        if let Some(dir) = self.imp().settings.borrow().last_folder.clone() {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        if let Ok(file) = dialog.open_future(Some(self)).await {
            self.load_file(file);
        }
    }

    pub fn load_file(&self, file: gio::File) {
        glib::spawn_future_local(glib::clone!(#[weak(rename_to = win)] self, async move {
            match file.load_contents_future().await {
                Ok((bytes, _)) => {
                    if let Some(dir) = file.parent().and_then(|d| d.path()) {
                        win.imp().settings.borrow_mut().last_folder = Some(dir.to_string_lossy().into());
                        win.save_settings();
                    }
                    win.set_program(file, link::decode(&bytes));
                }
                Err(e) => win.toast(&format!("Could not open the file: {}", e.message())),
            }
        }));
    }

    fn set_program(&self, file: gio::File, text: String) {
        let imp = self.imp();
        let name = file.basename().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        imp.file_row.set_title(&name);
        imp.file_row.set_subtitle(&file.parent().and_then(|d| d.path()).map(|p| p.display().to_string()).unwrap_or_default());
        if let Some(n) = link::detect_number(&text) {
            imp.number_row.set_value(n as f64);
        }
        *imp.program.borrow_mut() = Some(Program { text });
        imp.send_stack.set_visible_child_name("program");
        imp.stack.set_visible_child_name("send");
        self.update_program();
    }

    fn update_program(&self) {
        let imp = self.imp();
        let Some(prog) = &*imp.program.borrow() else { return };
        let number = Some(imp.number_row.value() as u16);
        let result = link::prepare(&prog.text, number, imp.ctrlz_row.is_active());
        let buffer = imp.preview_view.buffer();
        match &result {
            Ok(p) => {
                let shown = String::from_utf8_lossy(&p.bytes).replace('\r', "").replace('\x1a', "␚");
                buffer.set_text(&shown);
                let cfg = &imp.settings.borrow().port;
                let secs = (p.bytes.len() as f64 * cfg.char_time()).ceil();
                let mut desc = format!("{} blocks, {} characters, about {} s at {}", p.blocks, p.bytes.len(), secs, cfg.summary());
                if link::detect_number(&prog.text).is_none() {
                    desc.push_str(". The file has no program number: check the number above.");
                }
                if !p.has_m30 {
                    desc.push_str(". No M30: the control will wait for more after the last block.");
                }
                imp.preview_group.set_description(Some(&desc));
            }
            Err(e) => {
                buffer.set_text(&prog.text);
                imp.preview_group.set_description(Some(&e.to_string()));
            }
        }
        self.action_set_enabled("win.send", result.is_ok() && !imp.busy.get());
        *imp.prepared.borrow_mut() = result.ok();
    }

    async fn send(&self) {
        let imp = self.imp();
        self.update_program();
        let Some(p) = imp.prepared.borrow().clone() else { return };
        let cfg = imp.settings.borrow().port.clone();
        let secs = (p.bytes.len() as f64 * cfg.char_time()).ceil();
        let confirm = adw::AlertDialog::new(
            Some(&format!("Send Program %{:04}?", p.number)),
            Some(&format!(
                "{} blocks, about {} s at {}.\n\nThe control must be waiting first: O {} INP.",
                p.blocks, secs, cfg.summary(), p.number
            )),
        );
        confirm.add_responses(&[("cancel", "_Cancel"), ("send", "_Send")]);
        confirm.set_response_appearance("send", adw::ResponseAppearance::Suggested);
        confirm.set_default_response(Some("send"));
        confirm.set_close_response("cancel");
        if confirm.choose_future(Some(self)).await != "send" {
            return;
        }

        let dlg = TransferDialog::new(&format!("Sending %{:04}", p.number), "Opening port…", &cfg.path);
        let cancel = dlg.cancel_flag();
        let bytes = p.bytes.clone();
        let total = bytes.len();
        let rx = self.run_worker(move |tx| {
            let mut port = link::open(&cfg).map_err(|e| e.to_string())?;
            let _ = tx.send_blocking(Msg::Opened);
            link::send(&mut *port, &bytes, Duration::from_secs(cfg.stall_secs), &cancel, |s, t| {
                let _ = tx.send_blocking(Msg::Sent(s, t));
            })
            .map_err(|e| match e {
                link::LinkError::Cancelled { done } => format!("Stopped after {done} of {total} characters."),
                e => e.to_string(),
            })?;
            Ok(Vec::new())
        });
        dlg.present(Some(self));
        while let Ok(msg) = rx.recv().await {
            match msg {
                Msg::Opened => dlg.set_state("Sending…"),
                Msg::Sent(s, t) => {
                    dlg.set_progress(Some(s as f64 / t.max(1) as f64), &format!("{s} of {t} characters"));
                    if s == 0 {
                        dlg.set_detail("Waiting for the control to accept data");
                    } else {
                        dlg.set_detail("");
                    }
                }
                Msg::Done(Ok(_)) => dlg.finish(true, "Program Sent", "Check that the control shows the program."),
                Msg::Done(Err(e)) => dlg.finish(false, "Not Sent", &e),
                Msg::Received(_) => {}
            }
        }
        self.set_busy(false);
    }

    // ---- Receive ----

    async fn receive(&self) {
        let imp = self.imp();
        let cfg = imp.settings.borrow().port.clone();
        let mode = if imp.several_row.is_active() { ReceiveMode::Several } else { ReceiveMode::One };
        let idle = Duration::from_secs(imp.settings.borrow().idle_secs.max(1) as u64);
        let dlg = TransferDialog::new("Receiving", "Opening port…", &cfg.path);
        let cancel = dlg.cancel_flag();
        let rx = self.run_worker(move |tx| {
            let mut port = link::open(&cfg).map_err(|e| e.to_string())?;
            let _ = tx.send_blocking(Msg::Opened);
            link::receive(&mut *port, mode, idle, &cancel, |n| {
                let _ = tx.send_blocking(Msg::Received(n));
            })
            .map_err(|e| e.to_string())
        });
        dlg.present(Some(self));
        while let Ok(msg) = rx.recv().await {
            match msg {
                Msg::Opened => {
                    dlg.set_state("Waiting for the Control…");
                    dlg.set_detail("Start the output on the control now");
                    dlg.set_progress(None, "No data yet");
                }
                Msg::Received(n) => {
                    dlg.set_state("Receiving…");
                    dlg.set_detail("");
                    dlg.set_progress(None, &format!("{n} bytes"));
                }
                Msg::Done(Ok(raw)) => {
                    let programs = link::finish_receive(&raw, mode);
                    let bad = link::unexpected_bytes(&link::clean_received(&raw));
                    let detail = if !bad.is_empty() {
                        "Some characters were garbled. Check the baud rate and parity.".to_string()
                    } else {
                        String::new()
                    };
                    match programs.len() {
                        _ if raw.iter().all(|&c| c == 0) => dlg.finish(false, "Nothing Received", "No data came from the control."),
                        0 => dlg.finish(false, "No Program Found", "Data arrived but had no %NNNN start. Check the baud rate and parity."),
                        1 => dlg.finish(true, &format!("Received %{:04}", programs[0].number), &detail),
                        n => dlg.finish(true, &format!("Received {n} Programs"), &detail),
                    }
                    if !raw.iter().all(|&c| c == 0) {
                        self.set_received(programs, raw);
                    }
                }
                Msg::Done(Err(e)) => dlg.finish(false, "Receiving Failed", &e),
                Msg::Sent(..) => {}
            }
        }
        self.set_busy(false);
    }

    fn set_received(&self, programs: Vec<ReceivedProgram>, raw: Vec<u8>) {
        let imp = self.imp();
        imp.received_list.remove_all();
        if programs.is_empty() {
            let row = adw::ActionRow::builder()
                .title("Unrecognized Data")
                .subtitle(format!("{} bytes without a %NNNN start", raw.len()))
                .build();
            let data = link::clean_received(&raw);
            row.add_suffix(&self.save_button("received.txt", data));
            imp.received_list.append(&row);
        }
        for p in &programs {
            let row = adw::ActionRow::builder()
                .title(format!("Program %{:04}", p.number))
                .subtitle(format!("{} blocks, {} bytes", p.blocks(), p.bytes.len()))
                .build();
            let send = gtk::Button::builder()
                .icon_name("document-send-symbolic")
                .tooltip_text("Open in Send")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let bytes = p.bytes.clone();
            let number = p.number;
            send.connect_clicked(glib::clone!(#[weak(rename_to = win)] self, move |_| {
                let file = gio::File::for_path(glib::user_cache_dir().join(format!("{number:04}.nc")));
                win.set_program(file.clone(), link::decode(&bytes));
                win.imp().file_row.set_title(&format!("%{number:04} (received, not saved)"));
                win.imp().file_row.set_subtitle("");
            }));
            row.add_suffix(&send);
            row.add_suffix(&self.save_button(&format!("{:04}.nc", p.number), p.bytes.clone()));
            imp.received_list.append(&row);
        }
        imp.received_group.set_visible(true);
        imp.stack.set_visible_child_name("receive");
        self.action_set_enabled("win.save-all", programs.len() > 1);
        *imp.received.borrow_mut() = programs;
        *imp.received_raw.borrow_mut() = raw;
    }

    fn save_button(&self, name: &str, data: Vec<u8>) -> gtk::Button {
        let b = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Save…")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let name = name.to_string();
        b.connect_clicked(glib::clone!(#[weak(rename_to = win)] self, move |_| {
            let name = name.clone();
            let data = data.clone();
            glib::spawn_future_local(async move { win.save_one(&name, data).await });
        }));
        b
    }

    async fn save_one(&self, name: &str, data: Vec<u8>) {
        let dialog = gtk::FileDialog::builder().title("Save Program").initial_name(name).modal(true).build();
        if let Some(dir) = self.imp().settings.borrow().last_folder.clone() {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let Ok(file) = dialog.save_future(Some(self)).await else { return };
        match write_file(&file, data).await {
            Ok(()) => self.toast(&format!("Saved {}", file.basename().unwrap_or_default().display())),
            Err(e) => self.toast(&format!("Could not save: {}", e.message())),
        }
    }

    async fn save_all(&self) {
        let dialog = gtk::FileDialog::builder().title("Save All Programs to Folder").modal(true).build();
        if let Some(dir) = self.imp().settings.borrow().last_folder.clone() {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let Ok(folder) = dialog.select_folder_future(Some(self)).await else { return };
        let programs = self.imp().received.borrow().clone();
        let existing = programs.iter().filter(|p| folder.child(format!("{:04}.nc", p.number)).query_exists(gio::Cancellable::NONE)).count();
        if existing > 0 {
            let ask = adw::AlertDialog::new(
                Some(&format!("Replace {existing} Existing File{}?", if existing == 1 { "" } else { "s" })),
                Some("Files with the same program numbers are already in this folder."),
            );
            ask.add_responses(&[("cancel", "_Cancel"), ("replace", "_Replace")]);
            ask.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
            ask.set_close_response("cancel");
            if ask.choose_future(Some(self)).await != "replace" {
                return;
            }
        }
        for p in &programs {
            if let Err(e) = write_file(&folder.child(format!("{:04}.nc", p.number)), p.bytes.clone()).await {
                self.toast(&format!("Could not save %{:04}: {}", p.number, e.message()));
                return;
            }
        }
        self.toast(&format!("Saved {} programs", programs.len()));
    }

    // ---- Cable test ----

    async fn loopback(&self) {
        let mut cfg = self.imp().settings.borrow().port.clone();
        // Only TX and RX are bridged, so CTS stays off: test without handshake.
        cfg.handshake = Handshake::None;
        let dlg = TransferDialog::new("Loopback Test", "Testing…", &cfg.path);
        let rx = self.run_worker(move |_tx| {
            let mut port = link::open(&cfg).map_err(|e| e.to_string())?;
            let (ok, total) = link::loopback(&mut *port).map_err(|e| e.to_string())?;
            Ok(vec![ok as u8, total as u8])
        });
        dlg.present(Some(self));
        while let Ok(msg) = rx.recv().await {
            if let Msg::Done(r) = msg {
                match r {
                    Ok(v) if v[0] == v[1] => dlg.finish(true, "Cable Works", &format!("All {} characters came back.", v[1])),
                    Ok(v) if v[0] == 0 => dlg.finish(false, "Nothing Came Back", "Check that pins 2 and 3 are bridged at the cable end."),
                    Ok(v) => dlg.finish(false, "Characters Garbled", &format!("Only {} of {} came back intact.", v[0], v[1])),
                    Err(e) => dlg.finish(false, "Test Failed", &e),
                }
            }
        }
        self.set_busy(false);
    }

    // ---- Worker plumbing ----

    fn set_busy(&self, busy: bool) {
        let imp = self.imp();
        imp.busy.set(busy);
        for a in ["win.receive", "win.loopback", "win.open"] {
            self.action_set_enabled(a, !busy);
        }
        self.action_set_enabled("win.send", !busy && imp.prepared.borrow().is_some());
        self.update_watch();
    }

    /// Run `work` on a thread with the port to itself. The receiver ends after `Msg::Done`.
    fn run_worker(
        &self,
        work: impl FnOnce(&async_channel::Sender<Msg>) -> Result<Vec<u8>, String> + Send + 'static,
    ) -> async_channel::Receiver<Msg> {
        self.set_busy(true);
        *self.imp().watch_port.borrow_mut() = None;
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let r = work(&tx);
            let _ = tx.send_blocking(Msg::Done(r));
        });
        rx
    }
}

async fn write_file(file: &gio::File, data: Vec<u8>) -> Result<(), glib::Error> {
    file.replace_contents_future(data, None, false, gio::FileCreateFlags::REPLACE_DESTINATION)
        .await
        .map(|_| ())
        .map_err(|(_, e)| e)
}
