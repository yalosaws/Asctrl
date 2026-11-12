// No black console window in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    asctrl_agent_lib::run()
}
