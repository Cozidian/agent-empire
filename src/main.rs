mod client;
mod daemon;
mod draw;
mod game;
mod gfx;
mod octant;
mod proto;
mod sound;
mod survey;

use proto::{Req, call};
use std::io;

const HELP: &str = "empire — a galaxy of coding agents

  empire           open the galaxy (starts the daemon if needed)
  empire daemon    run the agent host in the foreground
  empire stop      shut down the daemon and every agent in it
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
        Some("stop") => call(&Req::Shutdown).map(|_| println!("daemon stopped; all agents recalled")),
        Some("sounds") => {
            let Some(s) = sound::Sound::start() else {
                println!("no audio device");
                return Ok(());
            };
            for c in sound::ALL {
                println!("{c:?}");
                s.play(c);
                std::thread::sleep(std::time::Duration::from_millis(1600));
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
