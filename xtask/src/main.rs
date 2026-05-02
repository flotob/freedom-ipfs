use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
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

    println!(
        "iOS static libraries built. On macOS, package them with xcodebuild -create-xcframework."
    );
    Ok(())
}
