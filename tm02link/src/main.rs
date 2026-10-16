mod link;
mod settings;
mod transfer_dialog;
mod window;

use adw::prelude::*;
use gtk::{gio, glib};

pub const APP_ID: &str = "io.github._0wn3r.Tm02Link";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> glib::ExitCode {
    gio::resources_register_include!("tm02link.gresource").expect("resources");

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .resource_base_path("/io/github/_0wn3r/Tm02Link")
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|app| {
        let provider = gtk::CssProvider::new();
        provider.load_from_resource("/io/github/_0wn3r/Tm02Link/style.css");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let quit = gio::ActionEntry::builder("quit").activate(|app: &adw::Application, _, _| app.quit()).build();
        let about = gio::ActionEntry::builder("about")
            .activate(|app: &adw::Application, _, _| {
                let about = adw::AboutDialog::builder()
                    .application_name("TM02 Link")
                    .application_icon(APP_ID)
                    .developer_name("Eduard")
                    .version(VERSION)
                    .comments("Send NC programs to an EMCOTRONIC TM02 lathe control and receive them back over RS232")
                    .website("https://github.com/0wn3r/emco")
                    .build();
                about.present(app.active_window().as_ref());
            })
            .build();
        app.add_action_entries([quit, about]);
        app.set_accels_for_action("app.quit", &["<Control>q"]);
        app.set_accels_for_action("win.open", &["<Control>o"]);
        app.set_accels_for_action("win.send", &["<Control>Return"]);
        app.set_accels_for_action("win.receive", &["<Control>r"]);
    });

    app.connect_activate(|app| {
        window::Window::present_for(app);
    });
    app.connect_open(|app, files, _| {
        let win = window::Window::present_for(app);
        if let Some(f) = files.first() {
            win.load_file(f.clone());
        }
    });

    app.run()
}
