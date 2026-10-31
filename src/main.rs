mod client;
mod daemon;
mod draw;
mod game;
mod gfx;
mod advisor;
mod octant;
mod proto;
mod role;
mod sound;
mod structure;
mod screenshots;
mod survey;

use proto::{Req, call};
use std::io;

const HELP: &str = "empire — a galaxy of coding agents

  empire           open the galaxy (starts the daemon if needed)
  empire daemon    run the agent host in the foreground
  empire status    one line for a status bar: ▲ blocked ✔ done ◆ working ○ idle
                   (--json: waybar-style {text, tooltip, class})
  empire upgrade   swap a running daemon for this build, keeping every agent
                   (the galaxy does this by itself when it starts)
  empire stop      shut down the daemon and every agent in it (they won't come back)
  empire sounds    play every sound cue once";

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("daemon") => daemon::run(),
        Some("hook") => {
            daemon::hook(args.get(1).map_or("", String::as_str));
            Ok(())
        }
        Some("statusline") => {
            daemon::statusline();
            Ok(())
        }
        Some("status") => {
            daemon::status(args.iter().any(|a| a == "--json"));
            Ok(())
        }
        Some("upgrade") => {
            match daemon::ensure_current() {
                None => println!("daemon is up to date; every agent kept"),
                Some(e) => println!("{e}"),
            }
            Ok(())
        }
        Some("stop") => call(&Req::Shutdown).map(|_| println!("daemon stopped; all agents recalled")),
        Some("sounds") => {
            let Some(s) = sound::Sound::start(false) else {
                println!("no audio device");
                return Ok(());
            };
            for c in sound::ALL {
                println!("{c:?}");
                s.play(c);
                std::thread::sleep(std::time::Duration::from_millis(3200));
            }
            Ok(())
        }
        Some("-h") | Some("--help") | Some("help") => {
            println!("{HELP}");
            Ok(())
        }
        _ => game::run(),
    }
}
