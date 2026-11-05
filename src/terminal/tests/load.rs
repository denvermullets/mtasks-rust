//! Load acceptance (JRST-375): 16 open Terminals, 4 visible, 8 flooding, and
//! an interactive Terminal that must still answer. Real shells, through the
//! public API, headless: the figures exclude drawing, rasterising and GPU cost.
//!
//! Debug builds parse about 250 times slower, so this only means something in
//! release. Run it with:
//!   mise exec -- cargo test --release load_acceptance -- --ignored --nocapture
use super::*;

const OPEN: usize = 16;
const VISIBLE: usize = 4;
const FLOODING: usize = 8;
const SAMPLES: usize = 40;

fn command(cmd: &str, args: &[&str]) -> String {
    std::process::Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn rss_mb(pid: u32) -> f64 {
    command("ps", &["-o", "rss=", "-p", &pid.to_string()])
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1024.0
}

/// Own RSS plus every shell's, in MB.
fn total_rss_mb(t: &Terminals) -> f64 {
    rss_mb(std::process::id()) + t.ids().iter().filter_map(|id| t.pid(*id)).map(rss_mb).sum::<f64>()
}

/// Poll for `check`, taking the UI's wake as a real app does, until `secs` pass.
fn respond_time(t: &Terminals, id: TerminalId, marker: &str, wakes: &AtomicUsize) -> Duration {
    let start = Instant::now();
    let mut last_ack = Instant::now();
    loop {
        if let Some(f) = t.frame(id) {
            if f.contains_line(marker) {
                return start.elapsed();
            }
        }
        if last_ack.elapsed() >= Duration::from_millis(16) {
            t.ack_wake();
            last_ack = Instant::now();
        }
        assert!(start.elapsed() < Duration::from_secs(10), "no answer for {marker}; wakes {}", wakes.load(Ordering::SeqCst));
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "load figures need a release build: cargo test --release load_acceptance -- --ignored --nocapture"]
fn load_acceptance_16_open_4_visible_8_flooding() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let mut t = Terminals::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    let ids: Vec<TerminalId> = (0..OPEN)
        .map(|i| t.spawn(sh_config(if i % 2 == 0 { "team-a" } else { "team-b" })).unwrap())
        .collect();
    assert_eq!(ids.len(), OPEN, "there is no hard limit to hit at this count");
    for id in &ids {
        wait_for(&t, *id, "a prompt", |f| f.cursor.is_some() && f.lines().iter().any(|l| l.contains('$')));
    }
    // Index 0 is the interactive one. Visible: 0 to 3. Flooding: 3 to 10, so
    // one flooder is visible and seven are hidden; 11 to 15 sit idle, hidden.
    let flooding: Vec<usize> = vec![3, 4, 5, 6, 7, 8, 9, 10];
    assert_eq!(flooding.len(), FLOODING);
    for (i, id) in ids.iter().enumerate() {
        t.set_visible(*id, i < VISIBLE);
    }
    let idle_rss = total_rss_mb(&t);
    for &i in &flooding {
        type_line(&t, ids[i], "yes flood-output");
    }
    std::thread::sleep(Duration::from_secs(2));
    let wakes_before = wakes.load(Ordering::SeqCst);
    let window = Instant::now();

    let mut times: Vec<Duration> = Vec::new();
    for n in 0..SAMPLES {
        let marker = format!("ok{n}x{}", 1000 + n);
        type_line(&t, ids[0], &format!("echo ok{n}x$((1000+{n}))"));
        times.push(respond_time(&t, ids[0], &marker, &wakes));
        std::thread::sleep(Duration::from_millis(25));
    }
    let secs = window.elapsed().as_secs_f64();
    let frames = wakes.load(Ordering::SeqCst) - wakes_before;
    let loaded_rss = total_rss_mb(&t);

    for &i in &flooding {
        assert!(!t.frame(ids[i]).unwrap().dead, "Terminal {i} died under load");
    }
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let p50 = ms(times[SAMPLES / 2]);
    let p95 = ms(times[SAMPLES * 95 / 100]);
    let max = ms(*times.last().unwrap());
    eprintln!("LOAD machine: {}, {} cores, {} GB, macOS {}",
        command("sysctl", &["-n", "machdep.cpu.brand_string"]),
        command("sysctl", &["-n", "hw.ncpu"]),
        command("sysctl", &["-n", "hw.memsize"]).parse::<u64>().unwrap_or(0) / (1 << 30),
        command("sw_vers", &["-productVersion"]));
    eprintln!("LOAD build: {}", if cfg!(debug_assertions) { "debug" } else { "release" });
    eprintln!("LOAD open {OPEN}, visible {VISIBLE}, flooding {FLOODING}, {SAMPLES} samples over {secs:.1}s (headless: no drawing)");
    eprintln!("LOAD interactive response: p50 {p50:.1} ms, p95 {p95:.1} ms, max {max:.1} ms (key sent to marker in the frame)");
    eprintln!("LOAD frame wakes to the UI: {frames} ({:.0}/s)", frames as f64 / secs);
    eprintln!("LOAD memory (app plus shells, RSS): idle {idle_rss:.0} MB, loaded {loaded_rss:.0} MB");

    // The target is a board that stays usable: a keystroke answers well inside
    // a frame budget of a few frames. Generous so it only fails on a real stall.
    assert!(p95 < 250.0, "interactive Terminal too slow under load: p95 {p95:.1} ms");
    close_all(t);
}
