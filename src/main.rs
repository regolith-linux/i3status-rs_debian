use clap::Parser;
use itertools::Itertools as _;

use i3status_rs::blocks::BlockError;
use i3status_rs::config::Config;
use i3status_rs::config::fragments::{FragmentDirs, SkippedFragment};
use i3status_rs::errors::*;
use i3status_rs::escape::Escaped;
use i3status_rs::widget::{State, Widget};
use i3status_rs::{BarState, protocol, util};

#[derive(Debug, thiserror::Error)]
enum ErrorMaybeInBlock {
    #[error(transparent)]
    InBlock(#[from] BlockError),
    #[error(transparent)]
    NotInBlock(#[from] Error),
}

fn main() {
    env_logger::init();

    let args = i3status_rs::CliArgs::parse();
    let blocking_threads = args.blocking_threads;
    let fragment_dirs = args.fragment_dirs();
    let reload_signals = if fragment_dirs.is_some() {
        vec![signal_hook::consts::SIGUSR2, signal_hook::consts::SIGHUP]
    } else {
        vec![signal_hook::consts::SIGUSR2]
    };

    if let Some(index) = args.doctor_worker {
        i3status_rs::doctor::run_worker(&args.config, index);
        std::process::exit(0);
    }

    if args.doctor {
        let display = if args.doctor_noicons {
            i3status_rs::doctor::IconDisplay::Names
        } else {
            i3status_rs::doctor::IconDisplay::Glyphs
        };
        let problems = i3status_rs::doctor::run(
            &args.config,
            args.font.as_deref(),
            args.doctor_skip_live,
            display,
        );
        std::process::exit(if problems > 0 { 1 } else { 0 });
    }

    if !args.no_init {
        protocol::init(args.never_pause);
    }

    let result: Result<(), ErrorMaybeInBlock> = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(blocking_threads)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let bar = match &fragment_dirs {
                Some(dirs) => fragments_bar(dirs).await?,
                None => file_bar(&args.config).await?,
            };
            bar.run_event_loop(restart).await?;
            Ok(())
        });

    if let Err(error) = result {
        let error_widget = Widget::new()
            .with_text(error.to_string().pango_escaped())
            .with_state(State::Critical);

        println!(
            "{},",
            serde_json::to_string(&error_widget.get_data(&Default::default(), 0).unwrap()).unwrap()
        );
        eprintln!("\n\n{error}\n\n");
        dbg!(error);

        // Wait for USR2 (or, with fragment directories, HUP) signal to restart
        signal_hook::iterator::Signals::new(reload_signals)
            .unwrap()
            .forever()
            .next()
            .unwrap();
        restart();
    }
}

/// A bar with the blocks of a config file
async fn file_bar(config: &str) -> Result<BarState> {
    let config_path = util::find_file(config, None, Some("toml"))?
        .or_error(|| format!("Configuration file '{config}' not found"))?;
    let config: Config = util::deserialize_toml_file(&config_path)?;
    spawn_bar(config).await
}

/// A bar with the blocks of the fragment directories, which restarts when
/// the fragments change
async fn fragments_bar(dirs: &FragmentDirs) -> Result<BarState> {
    let watcher = dirs.watch();
    let loaded = dirs.load()?;
    for skipped in &loaded.skipped {
        eprintln!("Skipped configuration fragment {skipped}");
    }
    let mut bar = spawn_bar(loaded.config).await?;
    if !loaded.skipped.is_empty() {
        bar.add_static_block(
            "config_fragments",
            skipped_fragments_widget(&loaded.skipped),
        );
    }
    bar.restart_on(watcher.reloads(loaded.snapshot));
    Ok(bar)
}

async fn spawn_bar(mut config: Config) -> Result<BarState> {
    let blocks = std::mem::take(&mut config.blocks);
    let mut bar = BarState::new(config);
    for block_config in blocks {
        bar.spawn_block(block_config).await?;
    }
    Ok(bar)
}

/// A critical widget naming the fragments that were skipped
fn skipped_fragments_widget(skipped: &[SkippedFragment]) -> Widget {
    let names = skipped.iter().map(SkippedFragment::file_name).join(", ");
    Widget::new()
        .with_text(format!(" Skipped configuration fragments: {names} ").pango_escaped())
        .with_state(State::Critical)
}

/// Restart in-place
fn restart() -> ! {
    use std::env;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStringExt;

    // On linux this line should be OK
    let exe = CString::new(env::current_exe().unwrap().into_os_string().into_vec()).unwrap();

    // Get current arguments
    let mut arg: Vec<CString> = env::args_os()
        .map(|a| CString::new(a.into_vec()).unwrap())
        .collect();

    // Add "--no-init" argument if not already added
    let no_init_arg = CString::new("--no-init").unwrap();
    if !arg.contains(&no_init_arg) {
        arg.push(no_init_arg);
    }

    // Restart
    let err = nix::unistd::execvp(&exe, &arg).unwrap_err();
    panic!("execvp failed: {err}");
}
