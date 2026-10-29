//! Opt-in headless process measurement. No devices, windows or external model.
//! With --pause each phase waits for one bounded stdin line so an owning test
//! controller can sample this process's RSS/threads before allowing progress.
use skylive_person_matte::{INPUT_ELEMENTS, evaluate_wait, retain_background_owner};
use std::io::{self, BufRead, Read, Write};

fn phase(name: &str, cycle: usize, initialized: Option<bool>, total_ns: u128, pause: bool) {
    let initialized = initialized.map_or("null", |value| if value { "true" } else { "false" });
    println!(
        "{{\"phase\":\"{name}\",\"cycle\":{cycle},\"pid\":{},\"initialized\":{initialized},\"total_ns\":{total_ns}}}",
        std::process::id()
    );
    io::stdout().flush().unwrap();
    if pause {
        let mut command = Vec::new();
        let read = io::stdin()
            .lock()
            .take(129)
            .read_until(b'\n', &mut command)
            .unwrap();
        assert!(
            read > 0 && read <= 128 && command.last() == Some(&b'\n'),
            "expected bounded controller line"
        );
    }
}
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let pause = match args.as_slice() {
        [] => false,
        [mode] if mode == "--pause" => true,
        _ => panic!("only --pause is supported"),
    };
    let input: Vec<f32> = include_bytes!("../tests/data/portrait-input.f32")
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    assert_eq!(input.len(), INPUT_ELEMENTS);
    for cycle in 0..2 {
        let owner = retain_background_owner().unwrap();
        phase("before_load", cycle, None, 0, pause);
        let first = evaluate_wait(&input).unwrap();
        assert!(first.timing.initialized);
        phase(
            "after_load",
            cycle,
            Some(first.timing.initialized),
            first.timing.total.as_nanos(),
            pause,
        );
        drop(first);
        let warm = evaluate_wait(&input).unwrap();
        assert!(!warm.timing.initialized);
        phase(
            "after_warm",
            cycle,
            Some(warm.timing.initialized),
            warm.timing.total.as_nanos(),
            pause,
        );
        drop(warm);
        drop(owner);
        phase("after_retire", cycle, None, 0, pause);
    }
}
