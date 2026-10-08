//! Command line parsing. Small enough that a hand-written parser is clearer
//! than pulling in a CLI framework.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

pub const HELP: &str = "\
DeskWatch panel simulator

USAGE:
    deskwatch-sim [OPTIONS]                   connect to MQTT and show the panel
    deskwatch-sim preview <FILE|DIR>...       flip through example payloads
    deskwatch-sim render <SCREEN.json> -o <OUT.png>
                                              draw one payload to a PNG, no window

OPTIONS:
    --host <HOST>          MQTT broker host [default: localhost]
    --port <PORT>          MQTT broker port [default: 1883]
    --prefix <PREFIX>      Topic prefix [default: deskpanel]
    --user <USER>          MQTT username; password from DESKWATCH_MQTT_PASSWORD
    --badges <FILE>        Badges payload for preview and render
    --scale <N>            Pixel scale of the window or PNG [default: 2, render: 1]
    --grid                 Leave a 1 px gap between pixels, to judge small text
    -h, --help             Show this help

BUTTON (run mode):
    click                  short press (held for 600 ms or more: long press)
    space / enter          short press
    L                      long press

PREVIEW KEYS:
    click, space, right    next payload
    left                   previous payload
";

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Run,
    Preview { paths: Vec<PathBuf> },
    Render { screen: PathBuf, out: PathBuf },
    Help,
}

#[derive(Debug, Clone)]
pub struct Args {
    pub command: Command,
    pub host: String,
    pub port: u16,
    pub prefix: String,
    pub user: Option<String>,
    pub badges: Option<PathBuf>,
    pub scale: u32,
    pub pixel_spacing: u32,
}

pub fn parse(mut it: impl Iterator<Item = String>) -> Result<Args> {
    let mut args = Args {
        command: Command::Run,
        host: "localhost".into(),
        port: 1883,
        prefix: "deskpanel".into(),
        user: None,
        badges: None,
        scale: 0,
        pixel_spacing: 0,
    };
    let mut positional = Vec::new();
    let mut out = None;

    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().with_context(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => args.command = Command::Help,
            "--host" => args.host = value("--host")?,
            "--port" => args.port = value("--port")?.parse().context("--port")?,
            "--prefix" => args.prefix = value("--prefix")?.trim_end_matches('/').into(),
            "--user" => args.user = Some(value("--user")?),
            "--badges" => args.badges = Some(value("--badges")?.into()),
            "--scale" => args.scale = value("--scale")?.parse().context("--scale")?,
            "--grid" => args.pixel_spacing = 1,
            "-o" | "--out" => out = Some(PathBuf::from(value("-o")?)),
            s if s.starts_with('-') => bail!("unknown option {s}, see --help"),
            _ => positional.push(arg),
        }
    }

    if args.command == Command::Help {
        return Ok(args);
    }

    let mut positional = positional.into_iter();
    args.command = match positional.next().as_deref() {
        None => Command::Run,
        Some("preview") => {
            let paths: Vec<PathBuf> = positional.map(PathBuf::from).collect();
            if paths.is_empty() {
                bail!("preview needs at least one file or directory");
            }
            Command::Preview { paths }
        }
        Some("render") => {
            let screen = positional.next().context("render needs a screen payload")?;
            let out = out.context("render needs -o <OUT.png>")?;
            Command::Render {
                screen: screen.into(),
                out,
            }
        }
        Some(other) => bail!("unknown command {other}, see --help"),
    };

    // A window is easier to read at 2x; a PNG defaults to the real size.
    if args.scale == 0 {
        args.scale = if matches!(args.command, Command::Render { .. }) {
            1
        } else {
            2
        };
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Args> {
        parse(s.split_whitespace().map(String::from))
    }

    #[test]
    fn defaults_to_run() {
        let a = p("").unwrap();
        assert_eq!(a.command, Command::Run);
        assert_eq!(
            (a.host.as_str(), a.port, a.prefix.as_str(), a.scale),
            ("localhost", 1883, "deskpanel", 2)
        );
    }

    #[test]
    fn render_and_preview() {
        let a = p("render s.json -o out.png --badges b.json").unwrap();
        assert_eq!(
            a.command,
            Command::Render {
                screen: "s.json".into(),
                out: "out.png".into()
            }
        );
        assert_eq!(a.scale, 1);
        let a = p("--prefix deskpanel-test/ preview a b").unwrap();
        assert_eq!(a.prefix, "deskpanel-test");
        assert_eq!(
            a.command,
            Command::Preview {
                paths: vec!["a".into(), "b".into()]
            }
        );
    }

    #[test]
    fn errors() {
        assert!(p("render s.json").is_err());
        assert!(p("--port nope").is_err());
        assert!(p("--bogus").is_err());
        assert!(p("preview").is_err());
    }
}
