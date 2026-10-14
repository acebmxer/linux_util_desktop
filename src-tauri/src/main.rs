#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--askpass") {
        linux_util_desktop_lib::askpass()
    }
    linux_util_desktop_lib::run()
}
