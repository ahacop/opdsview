//! opdsview — a terminal UI for browsing OPDS catalogs.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui_image::picker::Picker;

use opdsview::app::{App, is_ctrl_c};
use opdsview::cache::Cache;
use opdsview::storage::{self, Config, UserConfig, cache_dir};
use opdsview::ui;
use opdsview::worker::{Response, Worker};

/// Everything that can wake the event loop: a terminal input event or a
/// completed worker response. Folding both into one channel lets the UI thread
/// block until *something* happens instead of polling — so a freshly fetched
/// feed, cover, or download is drawn the instant it arrives rather than up to a
/// poll interval later.
enum AppEvent {
    Input(Event),
    // Boxed: `Response` is far larger than `Input`, and unboxed it would bloat
    // every queued input event to the response's size.
    Worker(Box<Response>),
}

fn main() -> Result<()> {
    let config = Config::load()?;
    let user_config = UserConfig::load()?;
    // Install the user's path overrides before anything resolves a directory
    // (the cache below, and the worker thread once spawned).
    storage::install_settings(user_config.settings.clone());
    let cache = Cache::new(cache_dir()?)?;

    // Detect the terminal's image protocol before entering the alternate screen.
    // Falls back to Unicode half-blocks, which render in any terminal. The worker
    // builds cover protocols (fetch + decode) off the UI thread, so it needs the
    // picker.
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let (worker, responses) = Worker::spawn(cache, picker)?;

    let mut app = App::new(config, user_config);

    let mut terminal = ratatui::init();
    // Raw mode is on now, so keys read cleanly; start the event pump.
    let events = spawn_event_pump(responses);
    let result = run(&mut terminal, &mut app, &worker, &events);
    ratatui::restore();
    result
}

/// Spawn the two source threads that feed the unified event channel: one blocks
/// on terminal input, the other drains the worker's responses. Both forward into
/// the returned receiver, which the event loop blocks on. The threads exit when
/// the receiver is dropped (their `send` fails) or their source closes.
fn spawn_event_pump(responses: Receiver<Response>) -> Receiver<AppEvent> {
    let (tx, rx) = mpsc::channel::<AppEvent>();

    let input_tx = tx.clone();
    thread::spawn(move || {
        while let Ok(event) = event::read() {
            if input_tx.send(AppEvent::Input(event)).is_err() {
                break;
            }
        }
    });

    thread::spawn(move || {
        while let Ok(resp) = responses.recv() {
            if tx.send(AppEvent::Worker(Box::new(resp))).is_err() {
                break;
            }
        }
    });

    rx
}

/// How long the selection must hold still before a held-back cover is drawn.
/// Longer than a key's auto-repeat interval, so holding ↑/↓ never draws a cover
/// mid-scroll (image work then can't throttle the scroll); short enough that the
/// cover appears promptly once you stop.
const COVER_SETTLE: Duration = Duration::from_millis(80);

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    worker: &Worker,
    events: &Receiver<AppEvent>,
) -> Result<()> {
    let mut had_overlay = false;
    // While `cover_pending`, the selection is still moving fast: the cover is
    // held back (drawn blank) and `settle_at` is when to draw it if no further
    // move arrives. `last_move` dates the previous selection change, so an
    // isolated move draws its cover at once while a fast scroll defers it.
    let mut cover_pending = false;
    let mut settle_at = Instant::now();
    let mut last_move = Instant::now();

    loop {
        // A popup painted over a graphics-protocol image leaves its cells behind
        // when it closes; a full clear forces the image to be re-emitted.
        let overlay = app.has_overlay();
        if had_overlay && !overlay {
            terminal.clear()?;
        }
        had_overlay = overlay;

        // Covers resize+encode inline during render now; drawing with covers
        // hidden while scrolling keeps this draw cheap (text only), so the scroll
        // runs at full speed and that one encode happens only once the selection
        // settles.
        terminal.draw(|frame| ui::render(frame, app, !cover_pending))?;
        // The reader queues inline-image loads while drawing; flush those (and
        // anything response handling queued) now.
        dispatch(app, worker);

        // Block until the next event. While a cover is held back, wake at
        // `settle_at` instead so it's drawn once the scroll stops. Worker
        // responses are applied but never push `settle_at` out — only input
        // does — so a burst of arriving covers can't keep the view blank.
        let event = if cover_pending {
            let now = Instant::now();
            if now >= settle_at {
                cover_pending = false;
                continue;
            }
            match events.recv_timeout(settle_at - now) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => {
                    cover_pending = false;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match events.recv() {
                Ok(event) => event,
                Err(_) => break,
            }
        };

        // Drain everything already queued so a burst of key-repeat or arriving
        // responses collapses into a single redraw instead of one per event.
        let cover_before = app.selected_cover_url();
        let mut quit = process_event(app, event);
        while let Ok(event) = events.try_recv() {
            quit |= process_event(app, event);
        }
        if quit || app.should_quit {
            break;
        }

        if app.selected_cover_url() != cover_before {
            let now = Instant::now();
            // Moves closer together than COVER_SETTLE are a fast scroll: hold the
            // cover back. An isolated move (the gap is larger) draws it at once.
            cover_pending = now.duration_since(last_move) < COVER_SETTLE;
            last_move = now;
            if cover_pending {
                settle_at = now + COVER_SETTLE;
            }
        }
    }
    Ok(())
}

/// Apply one event to the app. Returns `true` if the app should quit (Ctrl-C).
fn process_event(app: &mut App, event: AppEvent) -> bool {
    match event {
        AppEvent::Worker(resp) => app.handle_response(*resp),
        AppEvent::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
            app.status.clear();
            if is_ctrl_c(&key) {
                return true;
            }
            app.handle_key(key);
        }
        // Resize, focus, paste, mouse, key-release: nothing to apply; the redraw
        // after draining picks up any new terminal size.
        AppEvent::Input(_) => {}
    }
    false
}

/// Send any queued network requests to the worker thread.
fn dispatch(app: &mut App, worker: &Worker) {
    for req in app.outbox.drain(..) {
        worker.request(req);
    }
}
