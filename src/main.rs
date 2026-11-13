// The Windows desktop build is a GUI program: no console window when it runs
// in the background. Command-line use attaches to the caller's console.
#![cfg_attr(all(windows, feature = "gui"), windows_subsystem = "windows")]

mod app;

fn main() -> std::process::ExitCode {
    app::main()
}
