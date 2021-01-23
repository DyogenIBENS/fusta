#![allow(clippy::redundant_field_names)]
#[macro_use]
extern crate lazy_static;
use std::ffi::OsStr;

use anyhow::{bail, Context, Result};
use clap::*;
use daemonize::*;
use log::*;
use simplelog::*;

pub mod fs;
use fs::*;

#[derive(Debug, Clone)]
struct RunEnvironment {
    mountpoint: std::path::PathBuf,
    created_mountpoint: bool,
}
fn main() -> Result<()> {
    let args =
        App::new("fusta")
            .setting(AppSettings::ColoredHelp)
            .setting(AppSettings::ColorAuto)
            .setting(AppSettings::VersionlessSubcommands)
            .setting(AppSettings::UnifiedHelpMessage)
            .version(crate_version!())
            .author(crate_authors!())
            .arg(
                Arg::with_name("FASTA")
                    .help("A (multi)FASTA file containing the sequences to mount")
                    .required(true)
                    .index(1),
            )
            .arg(
                Arg::with_name("mountpoint")
                    .short("o")
                    .long("mountpoint")
                    .help("Specifies the directory to use as mountpoint; it will be created if it does not exist")
                    .default_value("fusta")
                    .takes_value(true)
                    .required(true),
            )
            .arg(
                Arg::with_name("max-cache")
                    .short("C")
                    .long("max-cache")
                    .help("Set the maximum amount of memory to use to cache writes (MB)")
                    .default_value("500")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("v")
                    .short("v")
                    .multiple(true)
                    .help("Sets the level of verbosity"),
            )
            .arg(
                Arg::with_name("daemon")
                    .short("D")
                    .long("daemon")
                    .help("Launch in the background; will automatically quit when unmounted"),
            )
            .arg(Arg::with_name("nommap").short("M").long("nommap").help(
                "Don't use mmap, but rather fseek(2) to extract sequences. Slower, but more memory-efficient.",
            ))
            .arg(
                Arg::with_name("nonempty")
                    .short("E")
                    .long("non-empty")
                    .help("Perform the mount even if the destination folder is not empty"),
            )
            .get_matches();

    let log_level = match args.occurrences_of("v") {
        0 => LevelFilter::Warn,
        1 => LevelFilter::Info,
        2 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };
    let log_config = ConfigBuilder::new().set_time_format_str("").build();

    if args.is_present("daemon") {
        let log_file_path = tempfile::Builder::new()
            .prefix("fusta-")
            .suffix(".log")
            .tempfile()
            .context("Unable to create a temporary file")?;

        println!(
            "Logs ({:?}) available in {:?}",
            log_level,
            log_file_path.path()
        );
        WriteLogger::init(log_level, log_config, log_file_path)
            .context("Unable to initialize logger")?;

        let pid_file = tempfile::Builder::new()
            .prefix("fusta-")
            .suffix(".pid")
            .tempfile()
            .context("Unable to create a temporary PID file")?;

        Daemonize::new()
            .pid_file(pid_file)
            .working_directory(std::env::current_dir().context("Unable to read current directory")?)
            .start()?;
    } else {
        TermLogger::init(log_level, log_config, TerminalMode::Mixed)
            .context("Unable to initialize logger")?;
    }

    let fasta_file = value_t!(args, "FASTA", String)?;
    let mountpoint = value_t!(args, "mountpoint", String)?;
    let mut fuse_options: Vec<&OsStr> = vec![
        &OsStr::new("-o"),
        &OsStr::new("auto_unmount"),
        &OsStr::new("-o"),
        &OsStr::new("default_permissions"),
    ];
    let settings = FustaSettings {
        mmap: !args.is_present("nommap"),
        concretize_threshold: value_t!(args, "max-cache", usize).unwrap() * 1024 * 1024,
    };

    info!("Using MMAP:      {}", settings.mmap);

    let fs = FustaFS::new(settings, &fasta_file);
    let mut env = RunEnvironment {
        mountpoint: std::path::PathBuf::from(mountpoint),
        created_mountpoint: false,
    };

    if !env.mountpoint.exists() {
        std::fs::create_dir(&env.mountpoint)?;
        env.created_mountpoint = true;
    }
    if !env.mountpoint.is_dir() {
        bail!("mount point `{:?}` is not a directory", env.mountpoint);
    }

    if args.is_present("nonempty") {
        fuse_options.push(&OsStr::new("-o"));
        fuse_options.push(&OsStr::new("nonempty"));
    }
    if !args.is_present("nonempty") && std::fs::read_dir(&env.mountpoint)?.take(1).count() != 0 {
        bail!(
            "mount point {:?} is not empty, use the -E flag to mount in a non-empty directory.",
            env.mountpoint
        );
    }

    let err_msg = if cfg!(target_os = "freebsd") || cfg!(target_os = "macos") {
        format!(
            "Please use `umount {:?}` to exit.",
            &env.mountpoint.canonicalize().unwrap()
        )
    } else {
        format!(
            "Please use `fusermount -u {0:?}` or `umount {0:?}` to exit.",
            &env.mountpoint.canonicalize().unwrap()
        )
    };
    ctrlc::set_handler(move || {
        error!("{}", err_msg);
    })?;
    match fuser::mount(fs, &env.mountpoint, &fuse_options) {
        Ok(()) => {
            eprintln!(
                "{} successfully mounted in {:?}",
                &fasta_file,
                &env.mountpoint.canonicalize().unwrap()
            );
        }
        _ => {
            error!("Unable to mount the FUSE filesystem");
            std::process::exit(1);
        }
    }
    cleanup(&env)?;
    println!("Quitting");

    Ok(())
}

fn cleanup(env: &RunEnvironment) -> Result<()> {
    if env.created_mountpoint {
        warn!(
            "You can now safely remove the {:?} directory",
            env.mountpoint
        );
    }
    Ok(())
}
