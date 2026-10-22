//! Test 3: log keys and clicks system-wide with timestamps.
//!
//! `input` sends harmless synthetic input (F13, Shift, a mouse move) and checks
//! the hook saw it. `input manual` just listens for 15 seconds so a person can
//! type and click.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rdev::{EventType, Key, listen, simulate};

fn main() {
    let manual = std::env::args().nth(1).as_deref() == Some("manual");
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    thread::spawn(move || {
        listen(move |event| {
            let _ = tx.send((started.elapsed(), event.event_type));
        })
        .expect("install input hook");
    });
    thread::sleep(Duration::from_millis(300));

    if manual {
        println!("listening for 15 s: type and click anywhere");
    } else {
        let send = |e: EventType| {
            simulate(&e).expect("simulate input");
            thread::sleep(Duration::from_millis(40));
        };
        send(EventType::KeyPress(Key::ShiftLeft));
        send(EventType::KeyPress(Key::Unknown(0x7C)));
        send(EventType::KeyRelease(Key::Unknown(0x7C)));
        send(EventType::KeyRelease(Key::ShiftLeft));
        send(EventType::MouseMove { x: 400.0, y: 400.0 });
        send(EventType::MouseMove { x: 420.0, y: 410.0 });
    }

    let deadline = Duration::from_secs(if manual { 15 } else { 2 });
    let (mut keys, mut buttons, mut moves) = (0, 0, 0);
    while started.elapsed() < deadline + Duration::from_millis(300) {
        let Ok((at, ev)) = rx.recv_timeout(Duration::from_millis(100)) else { continue };
        match ev {
            EventType::KeyPress(_) | EventType::KeyRelease(_) => keys += 1,
            EventType::ButtonPress(_) | EventType::ButtonRelease(_) => buttons += 1,
            EventType::MouseMove { .. } => {
                moves += 1;
                continue;
            }
            _ => {}
        }
        println!("{:>8.3}s {:?}", at.as_secs_f64(), ev);
    }
    println!("key events={keys} button events={buttons} mouse moves={moves}");
    let ok = if manual { keys + buttons > 0 } else { keys >= 4 && moves >= 2 };
    println!("{}", if ok { "RESULT: PASS" } else { "RESULT: FAIL" });
    std::process::exit(0);
}
