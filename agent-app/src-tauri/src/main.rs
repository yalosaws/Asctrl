// 发布版不弹出黑色控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    asctrl_agent_lib::run()
}
