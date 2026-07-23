// Mirror — thin display client living in each window's reserved pane.
// Shows the daemon's frame file, forwards raw key bytes to the daemon's
// FIFO, and copies the shared rows file to the per-pane path click.sh
// expects. Because every window keeps this pane permanently, switching
// windows never changes any layout — the join-pane "bump" is gone.
use crate::sidebar::{poll_fd, read_byte, term_size, RawMode, E};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

static WINCH: AtomicBool = AtomicBool::new(false);
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, Ordering::Relaxed);
}
extern "C" fn on_term(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

/// Print the frame clipped to this pane's height: the daemon renders for
/// the smallest mirror, but panes can shrink between scans — never emit a
/// line that would scroll us.
fn draw(frame: &str) {
    let (_, rows) = term_size();
    let mut out = String::with_capacity(frame.len());
    for (i, line) in frame.split_inclusive('\n').enumerate() {
        if i + 1 >= rows {
            break;
        }
        out.push_str(line);
    }
    print!("{out}{E}[J");
    let _ = std::io::stdout().flush();
}

pub fn run() -> i32 {
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_term as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as libc::sighandler_t);
    }
    let _raw = RawMode::enable();
    print!("{E}[?25l{E}[2J");
    let _ = std::io::stdout().flush();

    let tmp = std::env::temp_dir();
    let frame_file = tmp.join("agents-mon-frame");
    let keys_path = tmp.join("agents-mon-keys");
    let rows_shared = tmp.join("agents-mon-rows");
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let rows_own = tmp.join(format!("agents-mon-rows-{}", pane.trim_start_matches('%')));

    // FIFO write end: O_NONBLOCK open fails with ENXIO until the daemon's
    // read end exists — retry through the startup race
    let start = Instant::now();
    let mut fifo = -1;
    let c = std::ffi::CString::new(keys_path.as_os_str().as_encoded_bytes()).unwrap();
    while fifo < 0 && start.elapsed() < Duration::from_secs(10) && !QUIT.load(Ordering::Relaxed) {
        fifo = unsafe { libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
        if fifo < 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    if fifo < 0 {
        return 1;
    }

    let mut last_mtime = SystemTime::UNIX_EPOCH;
    let mut last_frame = String::new();
    let mut hot_until = Instant::now(); // tight polling right after a key
    // ponytail: background mirrors redraw animation frames too (4Hz × N
    // panes); gate to the focused window if battery ever matters
    loop {
        if QUIT.load(Ordering::Relaxed) {
            break;
        }
        let wait = if Instant::now() < hot_until { 10 } else { 100 };
        if poll_fd(0, Some(Duration::from_millis(wait))) {
            let Some(b) = read_byte(0) else { break }; // EOF — pane dying
            unsafe { libc::write(fifo, [b].as_ptr().cast(), 1) };
            hot_until = Instant::now() + Duration::from_millis(300);
        }
        match std::fs::metadata(&frame_file).and_then(|m| m.modified()) {
            Err(_) => break, // frame gone — daemon quit
            Ok(mtime) => {
                if mtime != last_mtime {
                    last_mtime = mtime;
                    let frame = std::fs::read_to_string(&frame_file).unwrap_or_default();
                    if frame != last_frame {
                        draw(&frame);
                        last_frame = frame;
                        let _ = std::fs::copy(&rows_shared, &rows_own);
                    }
                } else if mtime.elapsed().map_or(false, |a| a > Duration::from_secs(10)) {
                    break; // stale frame — daemon died without cleanup
                }
            }
        }
        if WINCH.swap(false, Ordering::Relaxed) {
            print!("{E}[2J");
            draw(&last_frame);
        }
    }
    print!("{E}[?25h");
    let _ = std::io::stdout().flush();
    let _ = std::fs::remove_file(&rows_own);
    unsafe { libc::close(fifo) };
    0
}
