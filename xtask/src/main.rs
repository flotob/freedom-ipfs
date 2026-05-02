use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Parser)]
struct Args {
    #[command(subcommand)]
    command: XtaskCommand,
}

#[derive(Debug, Subcommand)]
enum XtaskCommand {
    BuildXcframework,
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        XtaskCommand::BuildXcframework => build_xcframework(),
    }
}

fn build_xcframework() -> Result<()> {
    if env::consts::OS != "macos" {
        bail!(
            "build-xcframework requires macOS with Xcode command line tools; current host is {}",
            env::consts::OS
        );
    }

    let targets = [
        "aarch64-apple-ios",
        "aarch64-apple-ios-sim",
        "x86_64-apple-ios",
    ];

    for target in targets {
        let status = Command::new("rustup")
            .args(["target", "add", target])
            .status()
            .with_context(|| format!("rustup target add {target}"))?;
        if !status.success() {
            bail!("rustup target add {target} failed");
        }

        let status = Command::new("cargo")
            .args([
                "build",
                "-p",
                "freedom-ipfs-mobile",
                "--release",
                "--target",
                target,
            ])
            .env("IPHONEOS_DEPLOYMENT_TARGET", "16.0")
            .status()
            .with_context(|| format!("cargo build for {target}"))?;
        if !status.success() {
            bail!("cargo build for {target} failed");
        }
    }

    let out_dir = PathBuf::from("target/ios-xcframework");
    let sim_dir = out_dir.join("simulator");
    fs::create_dir_all(&sim_dir).context("create simulator output directory")?;

    let device_lib = staticlib("aarch64-apple-ios");
    let sim_arm64_lib = staticlib("aarch64-apple-ios-sim");
    let sim_x86_64_lib = staticlib("x86_64-apple-ios");
    let sim_universal_lib = sim_dir.join("libfreedom_ipfs_mobile.a");
    let framework = out_dir.join("FreedomIpfs.xcframework");
    if framework.exists() {
        fs::remove_dir_all(&framework).context("remove previous xcframework")?;
    }

    run(
        Command::new("lipo")
            .args(["-create", "-output"])
            .arg(&sim_universal_lib)
            .arg(&sim_arm64_lib)
            .arg(&sim_x86_64_lib),
        "lipo simulator static libraries",
    )?;

    run(
        Command::new("xcodebuild")
            .arg("-create-xcframework")
            .arg("-library")
            .arg(&device_lib)
            .arg("-headers")
            .arg("ffi/include")
            .arg("-library")
            .arg(&sim_universal_lib)
            .arg("-headers")
            .arg("ffi/include")
            .arg("-output")
            .arg(&framework),
        "xcodebuild -create-xcframework",
    )?;

    println!("created {}", framework.display());
    Ok(())
}

fn staticlib(target: &str) -> PathBuf {
    Path::new("target")
        .join(target)
        .join("release")
        .join("libfreedom_ipfs_mobile.a")
}

fn run(command: &mut Command, label: &str) -> Result<()> {
    let status = command.status().with_context(|| label.to_string())?;
    if !status.success() {
        bail!("{label} failed");
    }
    Ok(())
}
