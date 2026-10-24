//! Test helper: send a sequence of keys, for driving Mythicshot in tests.
//!
//! `keys F13 wait:800 Space Enter` presses F13, waits 800 ms, then Space and Enter.
//! `click:983,380` moves the mouse there and left-clicks; `rclick:` right-clicks;
//! `drag:x1,y1,x2,y2` drags with the left button.
//! Supported keys: F13–F16, Space, Enter, Escape, `key:<letter>` and `ctrl+<letter>` such as `ctrl+c`.

use std::thread;
use std::time::Duration;

use rdev::{EventType, Key, simulate};

fn main() {
    for arg in std::env::args().skip(1) {
        if let Some(ms) = arg.strip_prefix("wait:") {
            thread::sleep(Duration::from_millis(ms.parse().expect("wait:<ms>")));
            continue;
        }
        if let Some(pts) = arg.strip_prefix("drag:") {
            let v: Vec<f64> = pts.split(',').map(|n| n.parse().expect("drag:<x1>,<y1>,<x2>,<y2>")).collect();
            simulate(&EventType::MouseMove { x: v[0], y: v[1] }).expect("move");
            thread::sleep(Duration::from_millis(80));
            simulate(&EventType::ButtonPress(rdev::Button::Left)).expect("press");
            for i in 1..=10 {
                let f = i as f64 / 10.0;
                thread::sleep(Duration::from_millis(30));
                simulate(&EventType::MouseMove { x: v[0] + (v[2] - v[0]) * f, y: v[1] + (v[3] - v[1]) * f }).expect("move");
            }
            thread::sleep(Duration::from_millis(60));
            simulate(&EventType::ButtonRelease(rdev::Button::Left)).expect("release");
            thread::sleep(Duration::from_millis(150));
            continue;
        }
        let click = arg.strip_prefix("click:").map(|xy| (xy, rdev::Button::Left)).or_else(|| arg.strip_prefix("rclick:").map(|xy| (xy, rdev::Button::Right)));
        if let Some((xy, button)) = click {
            let (x, y) = xy.split_once(',').expect("click:<x>,<y>");
            let (x, y): (f64, f64) = (x.parse().expect("x"), y.parse().expect("y"));
            simulate(&EventType::MouseMove { x, y }).expect("move");
            thread::sleep(Duration::from_millis(80));
            simulate(&EventType::ButtonPress(button)).expect("press");
            thread::sleep(Duration::from_millis(40));
            simulate(&EventType::ButtonRelease(button)).expect("release");
            thread::sleep(Duration::from_millis(150));
            continue;
        }
        if let Some(letter) = arg.strip_prefix("ctrl+") {
            let key = match letter {
                "c" => Key::KeyC,
                "v" => Key::KeyV,
                "x" => Key::KeyX,
                "d" => Key::KeyD,
                "z" => Key::KeyZ,
                other => panic!("unsupported ctrl key {other}"),
            };
            for ev in [EventType::KeyPress(Key::ControlLeft), EventType::KeyPress(key), EventType::KeyRelease(key), EventType::KeyRelease(Key::ControlLeft)] {
                simulate(&ev).expect("key");
                thread::sleep(Duration::from_millis(40));
            }
            thread::sleep(Duration::from_millis(120));
            continue;
        }
        if let Some(letter) = arg.strip_prefix("key:") {
            let key = match letter {
                "a" => Key::KeyA,
                "c" => Key::KeyC,
                "d" => Key::KeyD,
                "e" => Key::KeyE,
                "f" => Key::KeyF,
                "g" => Key::KeyG,
                "l" => Key::KeyL,
                "o" => Key::KeyO,
                "r" => Key::KeyR,
                "u" => Key::KeyU,
                "v" => Key::KeyV,
                "w" => Key::KeyW,
                "z" => Key::KeyZ,
                other => panic!("unsupported key {other}"),
            };
            simulate(&EventType::KeyPress(key)).expect("press");
            thread::sleep(Duration::from_millis(30));
            simulate(&EventType::KeyRelease(key)).expect("release");
            thread::sleep(Duration::from_millis(120));
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
