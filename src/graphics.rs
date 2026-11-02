//! An offline graphics comparison; deliberately never starts a client or sound.

use crate::{draw, gfx::{Fb, Out}};
use crossterm::{cursor, event::{self, Event, KeyCode, KeyEventKind, KeyModifiers}, execute, style, terminal};
use std::{io, time::{Duration, Instant}};

pub fn run(args: &[String]) -> io::Result<()> {
    let mut polished = !args.iter().any(|a| a == "--classic");
    let mut stdout = io::stdout();
    if args.iter().any(|a| a == "--snapshot") {
        let mut fb = Fb::new(140, 48);
        draw::graphics_preview(&mut fb, polished, 12.0);
        Out::preview(fb.cols * fb.rows, polished).present(&fb, &mut stdout)?;
        return execute!(stdout, style::ResetColor, cursor::MoveTo(0, fb.rows as u16));
    }
    terminal::enable_raw_mode()?;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = execute!(io::stdout(), style::ResetColor, cursor::Show, terminal::LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
        }
    }
    let _restore = Restore;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
    let (cols, rows) = terminal::size()?;
    let mut fb = Fb::new(cols as usize, rows as usize);
    let mut out = Out::preview(fb.cols * fb.rows, polished);
    let (mut t, mut paused, mut last) = (12.0, false, Instant::now());
    loop {
        let start = Instant::now();
        if !paused { t += (start - last).as_secs_f32().min(0.1); }
        last = start;
        draw::graphics_preview(&mut fb, polished, t);
        if paused { fb.text(fb.cols as i32 - 9, 2, "PAUSED", crate::gfx::s3(0.7)); }
        out.present(&fb, &mut stdout)?;
        if event::poll(Duration::from_millis(50).saturating_sub(start.elapsed()))? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char(' ') => { polished = !polished; out = Out::preview(fb.cols * fb.rows, polished); }
                    KeyCode::Char('p') => paused = !paused,
                    _ => {}
                },
                Event::Resize(cols, rows) => {
                    fb = Fb::new(cols as usize, rows as usize);
                    out = Out::preview(fb.cols * fb.rows, polished);
                    execute!(stdout, terminal::Clear(terminal::ClearType::All))?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
