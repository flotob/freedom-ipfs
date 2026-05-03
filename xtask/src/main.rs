use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use freedom_ipfs_core::{cid_from_data, encode_car_v1, CarBlock, CODEC_RAW};
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
    VerifyXcframework,
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        XtaskCommand::BuildXcframework => build_xcframework(),
        XtaskCommand::VerifyXcframework => verify_xcframework_command(),
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
    let headers_dir = out_dir.join("headers");
    fs::create_dir_all(&sim_dir).context("create simulator output directory")?;
    stage_headers(&headers_dir)?;

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
            .arg(&headers_dir)
            .arg("-library")
            .arg(&sim_universal_lib)
            .arg("-headers")
            .arg(&headers_dir)
            .arg("-output")
            .arg(&framework),
        "xcodebuild -create-xcframework",
    )?;

    verify_xcframework(&framework, false)?;
    println!("created {}", framework.display());
    Ok(())
}

fn verify_xcframework_command() -> Result<()> {
    if env::consts::OS != "macos" {
        bail!(
            "verify-xcframework requires macOS with Xcode command line tools; current host is {}",
            env::consts::OS
        );
    }
    verify_xcframework(
        &PathBuf::from("target/ios-xcframework/FreedomIpfs.xcframework"),
        true,
    )
}

fn stage_headers(headers_dir: &Path) -> Result<()> {
    if headers_dir.exists() {
        fs::remove_dir_all(headers_dir).context("remove previous header staging directory")?;
    }
    fs::create_dir_all(headers_dir).context("create header staging directory")?;
    fs::copy(
        "ffi/include/freedom_ipfs.h",
        headers_dir.join("freedom_ipfs.h"),
    )
    .context("stage freedom_ipfs.h")?;
    fs::copy(
        "ffi/modulemap/module.modulemap",
        headers_dir.join("module.modulemap"),
    )
    .context("stage module.modulemap")?;
    Ok(())
}

fn verify_xcframework(framework: &Path, run_simulator_smoke: bool) -> Result<()> {
    if !framework.exists() {
        bail!("{} does not exist", framework.display());
    }
    let info_plist = framework.join("Info.plist");
    if !info_plist.exists() {
        bail!("{} is missing", info_plist.display());
    }

    let libraries = find_named_files(framework, "libfreedom_ipfs_mobile.a")?;
    if libraries.len() < 2 {
        bail!(
            "expected device and simulator static libraries in {}, found {}",
            framework.display(),
            libraries.len()
        );
    }
    let headers = find_named_files(framework, "freedom_ipfs.h")?;
    if headers.len() < 2 {
        bail!(
            "expected headers in each XCFramework slice in {}, found {}",
            framework.display(),
            headers.len()
        );
    }
    let modulemaps = find_named_files(framework, "module.modulemap")?;
    if modulemaps.len() < 2 {
        bail!(
            "expected module maps in each XCFramework slice in {}, found {}",
            framework.display(),
            modulemaps.len()
        );
    }

    for library in &libraries {
        verify_exported_symbols(library)?;
    }

    if run_simulator_smoke {
        verify_swift_simulator_smoke(framework, &libraries)?;
    }

    println!("verified {}", framework.display());
    Ok(())
}

fn find_named_files(root: &Path, name: &str) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_named_files(root, name, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_named_files(path: &Path, name: &str, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_named_files(&path, name, files)?;
        } else if path.file_name().and_then(|file_name| file_name.to_str()) == Some(name) {
            files.push(path);
        }
    }
    Ok(())
}

fn verify_exported_symbols(library: &Path) -> Result<()> {
    let output = Command::new("xcrun")
        .args(["nm", "-gU"])
        .arg(library)
        .output()
        .with_context(|| format!("xcrun nm {}", library.display()))?;
    if !output.status.success() {
        bail!("xcrun nm {} failed", library.display());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for symbol in [
        "freedom_ipfs_version",
        "freedom_ipfs_node_new_with_data_dir",
        "freedom_ipfs_node_start_gateway_online_with_config_v2",
        "freedom_ipfs_node_enter_background",
        "freedom_ipfs_node_handle_low_memory",
    ] {
        if !stdout.contains(symbol) {
            bail!("{} does not export {symbol}", library.display());
        }
    }
    Ok(())
}

fn verify_swift_simulator_smoke(framework: &Path, libraries: &[PathBuf]) -> Result<()> {
    let library = libraries
        .iter()
        .find(|library| library.to_string_lossy().contains("simulator"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} is missing a simulator library slice",
                framework.display()
            )
        })?;
    let slice_dir = library
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", library.display()))?;
    let headers_dir = slice_dir.join("Headers");
    if !headers_dir.join("freedom_ipfs.h").exists() {
        bail!(
            "{} is missing the simulator slice freedom_ipfs.h header",
            headers_dir.display()
        );
    }
    if !headers_dir.join("module.modulemap").exists() {
        bail!(
            "{} is missing the simulator slice module.modulemap",
            headers_dir.display()
        );
    }

    let sdk_path = command_stdout(
        Command::new("xcrun").args(["--sdk", "iphonesimulator", "--show-sdk-path"]),
        "xcrun --sdk iphonesimulator --show-sdk-path",
    )?;
    let target = simulator_swift_target()?;
    let verify_dir = PathBuf::from("target/ios-xcframework/verify");
    if verify_dir.exists() {
        fs::remove_dir_all(&verify_dir).context("remove previous Swift verification directory")?;
    }
    fs::create_dir_all(&verify_dir).context("create Swift verification directory")?;
    let smoke = verify_dir.join("FreedomIpfsSmoke.swift");
    let fixture_bytes = b"simulator fixture";
    let fixture_cid = cid_from_data(CODEC_RAW, fixture_bytes);
    let fixture_car = encode_car_v1(&[CarBlock {
        cid: fixture_cid,
        data: fixture_bytes.to_vec(),
    }]);
    let fixture_car = format_swift_byte_array(&fixture_car);
    let fixture_body = format_swift_byte_array(fixture_bytes);
    let smoke_source = format!(
        r#"import Foundation
import FreedomIpfs

@main
enum FreedomIpfsSmoke {{
    static func main() async throws {{
        _ = FreedomIpfsReader.version
        let reader = try FreedomIpfsReader()
        try reader.importCar(Data([{fixture_car}]))
        try reader.startGateway()
        guard reader.gatewayURL != nil else {{
            fatalError("gateway URL missing")
        }}
        guard let url = reader.localGatewayURL(for: "/ipfs/{fixture_cid}") else {{
            fatalError("fixture gateway URL missing")
        }}
        let (data, response) = try await URLSession.shared.data(from: url)
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {{
            fatalError("fixture request failed")
        }}
        guard data == Data([{fixture_body}]) else {{
            fatalError("fixture body mismatch")
        }}
        _ = reader.stopGateway()
    }}
}}
"#,
    );
    fs::write(&smoke, smoke_source).context("write Swift verification smoke source")?;

    let executable = verify_dir.join("FreedomIpfsSmoke");
    run(
        Command::new("xcrun")
            .args(["--sdk", "iphonesimulator", "swiftc"])
            .arg("-target")
            .arg(target)
            .arg("-sdk")
            .arg(sdk_path.trim())
            .arg("-I")
            .arg(&headers_dir)
            .arg("-L")
            .arg(slice_dir)
            .arg("-l")
            .arg("freedom_ipfs_mobile")
            .arg("ffi/swift/FreedomIpfsReader.swift")
            .arg(&smoke)
            .arg("-o")
            .arg(&executable),
        "swiftc simulator link smoke",
    )?;

    run(
        Command::new("xcrun").args(["simctl", "bootstatus", "booted", "-b"]),
        "wait for booted iOS simulator",
    )?;
    let executable = fs::canonicalize(&executable)
        .with_context(|| format!("canonicalize {}", executable.display()))?;
    run(
        Command::new("xcrun")
            .args(["simctl", "spawn", "booted"])
            .arg(&executable),
        "simctl simulator gateway smoke",
    )
}

fn simulator_swift_target() -> Result<&'static str> {
    match env::consts::ARCH {
        "aarch64" => Ok("arm64-apple-ios16.0-simulator"),
        "x86_64" => Ok("x86_64-apple-ios16.0-simulator"),
        arch => bail!("unsupported macOS host architecture for simulator Swift smoke: {arch}"),
    }
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

fn command_stdout(command: &mut Command, label: &str) -> Result<String> {
    let output = command.output().with_context(|| label.to_string())?;
    if !output.status.success() {
        bail!("{label} failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn format_swift_byte_array(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
