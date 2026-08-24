// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|arg| arg == "--daily-check") {
        if let Err(error) = worktree_gc_lib::run_daily_check() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    worktree_gc_lib::run()
}
