fn main() {
    eprintln!(
        "The legacy GTK desktop client has been removed.\nRun the Tauri desktop client instead:\n  cd spikes/tauri-desktop && npm run tauri:dev"
    );
    std::process::exit(1);
}
