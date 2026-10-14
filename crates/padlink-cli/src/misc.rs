//! udev-rule / personas / logs / art

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use padlink_core::{art, discovery, integration, logger, paths, personas};

use crate::style::{bold, dim, status, stdout_is_tty};
use crate::{DevArgs, Result, die, get_ctrl};

pub fn udev_rule(dev: &DevArgs) -> Result {
    let x = get_ctrl(dev)?;
    print!("{}", integration::udev_rule(&x));
    std::io::stdout().flush()?;
    if stdout_is_tty() {
        eprintln!(
            "{}",
            dim(&format!(
                "\n# install: padlink udev-rule {} | sudo tee /etc/udev/rules.d/70-padlink.rules \
                 && sudo udevadm control --reload && sudo udevadm trigger",
                x.id_str()
            ))
        );
    }
    Ok(0)
}

pub fn personas() -> Result {
    for p in &personas::PERSONAS {
        let default = if p.id == personas::DEFAULT { dim(" (default)") } else { String::new() };
        println!("{:24} {} [{}]{default}", bold(p.id), p.label, p.id_str());
        println!("    {}", dim(p.name));
        println!("    {}", p.description);
    }
    println!("{:24} clone the physical controller's own buttons/axes (+ rumble)", bold("mirror"));
    println!("{}", dim("\nuse: padlink bridge <dev> --as <persona>"));
    Ok(0)
}

#[derive(clap::Args)]
pub struct LogsArgs {
    #[arg(value_parser = ["list", "show", "export", "path"])]
    action: String,
    /// show: index or path; export: output .tar.gz
    file: Option<String>,
    /// show: comma-separated kinds to include (e.g. ff_upload,ff_play)
    #[arg(long)]
    kind: Option<String>,
    #[arg(long, default_value_t = 30)]
    limit: usize,
}

pub fn logs(a: &LogsArgs) -> Result {
    let logs = logger::list_logs(None);
    let log_dir = paths::log_dir();
    match a.action.as_str() {
        "path" => println!("{}", log_dir.display()),
        "list" => {
            for (i, p) in logs.iter().take(a.limit).enumerate() {
                let size = fs::metadata(p).map_or(0, |m| m.len());
                println!("{i:3}  {}  {} KiB", file_name(p), size / 1024);
            }
            if logs.is_empty() {
                println!("no logs in {}", log_dir.display());
            }
        }
        "show" => {
            let path = match a.file.as_deref() {
                None => logs.first().cloned(),
                Some(t) if t.bytes().all(|b| b.is_ascii_digit()) && !t.is_empty() => {
                    t.parse::<usize>().ok().and_then(|i| logs.get(i).cloned())
                }
                Some(t) => Some(PathBuf::from(t)),
            };
            let Some(path) = path else { return Ok(die("no such log")) };
            let recs = logger::read_log(&path)?;
            let kinds: Option<Vec<&str>> = a.kind.as_deref().map(|k| k.split(',').collect());
            let t0 = recs.first().and_then(|r| r.get("t")).and_then(serde_json::Value::as_f64);
            println!("{}", dim(&path.display().to_string()));
            for r in &recs {
                let kind = r.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default();
                if kinds.as_ref().is_some_and(|ks| !ks.contains(&kind)) {
                    continue;
                }
                println!("{}", logger::format_record(r, t0));
            }
        }
        "export" => {
            if logs.is_empty() {
                return Ok(die("no logs"));
            }
            let dest = a.file.clone().unwrap_or_else(|| "padlink-logs.tar.gz".into());
            let gz = flate2::write::GzEncoder::new(fs::File::create(&dest)?, flate2::Compression::default());
            let mut tar = tar::Builder::new(gz);
            for p in logs.iter().take(a.limit) {
                tar.append_path_with_name(p, format!("logs/{}", file_name(p)))?;
            }
            let profile_dir = paths::profile_dir();
            if let Ok(entries) = fs::read_dir(&profile_dir) {
                for p in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")) {
                    tar.append_path_with_name(&p, format!("profiles/{}", file_name(&p)))?;
                }
            }
            tar.into_inner()?.finish()?;
            println!("wrote {dest}");
        }
        _ => return Ok(1),
    }
    Ok(0)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[derive(clap::Args)]
pub struct ArtArgs {
    /// template: print/write a template; init: create an editable copy in
    /// ~/.config/padlink/art; check: validate a file; list: what each controller uses
    #[arg(value_parser = ["template", "init", "check", "list"])]
    action: String,
    /// xbox | playstation | nintendo (or a model name / .svg path for check)
    name: Option<String>,
    /// template: write to this file
    #[arg(short, long)]
    output: Option<String>,
    /// init: overwrite an existing file
    #[arg(long)]
    force: bool,
}

/// Art shipped with padlink: `<prefix>/share/padlink/art` next to the binary's `bin/`.
fn builtin_art_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent()?.parent().map(|p| p.join("share/padlink/art")))
        .unwrap_or_else(|| PathBuf::from("/usr/share/padlink/art"))
}

pub fn art(a: &ArtArgs) -> Result {
    let user = art::user_art_dir();
    let builtin = builtin_art_dir();
    match a.action.as_str() {
        "template" => {
            let svg = art::template_svg(a.name.as_deref().unwrap_or("xbox"))?;
            match &a.output {
                Some(out) => {
                    fs::write(out, svg)?;
                    println!("wrote {out}");
                }
                None => print!("{svg}"),
            }
            Ok(0)
        }
        "init" => {
            let name = a.name.as_deref().unwrap_or("xbox");
            let family = if art::FAMILIES.contains(&name) {
                Some(name)
            } else {
                match name {
                    "xbox360" | "xboxone" | "xboxseries" => Some("xbox"),
                    "ds4" | "dualsense" => Some("playstation"),
                    "switchpro" => Some("nintendo"),
                    _ => None,
                }
            };
            let Some(family) = family else {
                return Ok(die(&format!(
                    "unknown art name '{name}'; use {} or a model name",
                    art::FAMILIES.join(", ")
                )));
            };
            let dest = user.join(format!("{name}.svg"));
            if dest.exists() && !a.force {
                return Ok(die(&format!("{} exists (use --force to overwrite)", dest.display())));
            }
            fs::create_dir_all(&user)?;
            fs::write(&dest, art::template_svg(family)?)?;
            println!("wrote {}", dest.display());
            println!("Open it in Illustrator/Inkscape, draw over the shapes, keep the names, save as SVG.");
            println!("padlink's Input tab reloads it automatically on save.");
            Ok(0)
        }
        "check" => {
            let target = match a.name.as_deref().filter(|n| n.ends_with(".svg")) {
                Some(p) => PathBuf::from(p),
                None => {
                    let name = a.name.as_deref().unwrap_or("xbox");
                    let found = [&user, &builtin].iter().map(|d| d.join(format!("{name}.svg"))).find(|p| p.exists());
                    match found {
                        Some(p) => p,
                        None => {
                            return Ok(die(&format!(
                                "no artwork named '{name}' in {} or {}",
                                user.display(),
                                builtin.display()
                            )));
                        }
                    }
                }
            };
            let results = art::check(&target);
            println!("{}", bold(&target.display().to_string()));
            for (st, msg) in &results {
                println!("{}  {msg}", status(st.as_str()));
            }
            Ok(i32::from(results.iter().any(|(s, _)| *s == art::Status::Fail)))
        }
        "list" => {
            println!("user art:    {}", user.display());
            println!("shipped art: {}", builtin.display());
            for d in [&user, &builtin] {
                let mut svgs: Vec<PathBuf> = fs::read_dir(d)
                    .map(|it| {
                        it.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "svg")).collect()
                    })
                    .unwrap_or_default();
                svgs.sort();
                for f in svgs {
                    println!("  {}", f.display());
                }
            }
            for c in discovery::scan(false)? {
                let found = art::find(c.vendor, c.product, &[user.clone(), builtin.clone()]);
                let looks: Vec<String> =
                    art::candidates(c.vendor, c.product).iter().map(|n| format!("{n}.svg")).collect();
                println!(
                    "{} [{}]: looks for {} -> {}",
                    c.name,
                    c.id_str(),
                    looks.join(", "),
                    found.map_or_else(|| "built-in drawing".to_string(), |p| p.display().to_string())
                );
            }
            Ok(0)
        }
        _ => Ok(1),
    }
}
