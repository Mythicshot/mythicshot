//! Test helper: send a sequence of keys, for driving Mythicshot in tests.
//!
//! `keys F13 wait:800 Space Enter` presses F13, waits 800 ms, then Space and Enter.
//! `click:983,380` moves the mouse there and left-clicks.
//! Supported keys: F13–F16, Space, Enter, Escape.

use std::thread;
use std::time::Duration;

use rdev::{EventType, Key, simulate};

fn main() {
    for arg in std::env::args().skip(1) {
        if let Some(ms) = arg.strip_prefix("wait:") {
            thread::sleep(Duration::from_millis(ms.parse().expect("wait:<ms>")));
            continue;
        }
        if let Some(xy) = arg.strip_prefix("click:") {
            let (x, y) = xy.split_once(',').expect("click:<x>,<y>");
            let (x, y): (f64, f64) = (x.parse().expect("x"), y.parse().expect("y"));
            simulate(&EventType::MouseMove { x, y }).expect("move");
            thread::sleep(Duration::from_millis(80));
            simulate(&EventType::ButtonPress(rdev::Button::Left)).expect("press");
            thread::sleep(Duration::from_millis(40));
            simulate(&EventType::ButtonRelease(rdev::Button::Left)).expect("release");
            thread::sleep(Duration::from_millis(150));
            continue;
        }
        let key = match arg.as_str() {
            "F13" => Key::Unknown(124),
            "F14" => Key::Unknown(125),
            "F15" => Key::Unknown(126),
            "F16" => Key::Unknown(127),
            "Space" => Key::Space,
            "Enter" => Key::Return,
            "Escape" => Key::Escape,
            other => panic!("unsupported key {other}"),
        };
        simulate(&EventType::KeyPress(key)).expect("press");
        thread::sleep(Duration::from_millis(30));
        simulate(&EventType::KeyRelease(key)).expect("release");
        thread::sleep(Duration::from_millis(120));
    }
}
