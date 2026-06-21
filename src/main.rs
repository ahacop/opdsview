//! opdsview — a terminal UI for browsing OPDS catalogs.

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui_image::picker::Picker;

use opdsview::app::{App, is_ctrl_c};
use opdsview::cache::Cache;
use opdsview::storage::{self, Config, UserConfig, cache_dir};
use opdsview::ui;
use opdsview::worker::Worker;

fn main() -> Result<()> {
    let config = Config::load()?;
    let user_config = UserConfig::load()?;
    // Install the user's path overrides before anything resolves a directory
    // (the cache below, and the worker thread once spawned).
    storage::install_settings(user_config.settings.clone());
    let cache = Cache::new(cache_dir()?)?;
    let worker = Worker::spawn(cache)?;

    // Detect the terminal's image protocol before entering the alternate screen.
    // Falls back to Unicode half-blocks, which render in any terminal.
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

    let mut app = App::new(config, user_config, picker);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &worker);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App, worker: &Worker) -> Result<()> {
    let mut had_overlay = false;
    loop {
        terminal.draw(|frame| ui::render(frame, app))?;

        // Apply any completed network responses.
        while let Ok(resp) = worker.rx.try_recv() {
            app.handle_response(resp);
        }
        dispatch(app, worker);

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.status.clear();
            if is_ctrl_c(&key) {
                break;
            }
            app.handle_key(key);
            dispatch(app, worker);
        }

        // A popup that was painted over a graphics-protocol image leaves its
        // cells behind when it closes; a full clear forces the image to be
        // re-emitted on the next draw.
        let overlay = app.has_overlay();
        if had_overlay && !overlay {
            terminal.clear()?;
        }
        had_overlay = overlay;

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// Send any queued network requests to the worker thread.
fn dispatch(app: &mut App, worker: &Worker) {
    for req in app.outbox.drain(..) {
        worker.request(req);
    }
}
