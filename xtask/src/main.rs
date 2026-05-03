use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use freedom_ipfs_core::{cid_from_data, encode_car_v1, CarBlock, CODEC_RAW};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

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
    let llvm_nm = rust_llvm_nm()?;
    let output = Command::new(&llvm_nm)
        .args(["--extern-only", "--defined-only"])
        .arg(library)
        .output()
        .with_context(|| format!("{} {}", llvm_nm.display(), library.display()))?;
    if !output.status.success() {
        bail!(
            "{} {} failed: {}",
            llvm_nm.display(),
            library.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for symbol in [
        "freedom_ipfs_version",
        "freedom_ipfs_node_new_with_data_dir",
        "freedom_ipfs_node_start_gateway_online_with_config_v2",
        "freedom_ipfs_node_restart_gateway_online_with_config_v2",
        "freedom_ipfs_node_enter_background",
        "freedom_ipfs_node_handle_low_memory",
        "freedom_ipfs_node_retrieval_stats",
        "freedom_ipfs_node_routing_stats",
        "freedom_ipfs_node_active_preload_count",
        "freedom_ipfs_node_diagnostics",
    ] {
        if !stdout.contains(symbol) {
            bail!("{} does not export {symbol}", library.display());
        }
    }
    Ok(())
}

fn rust_llvm_nm() -> Result<PathBuf> {
    let sysroot = command_stdout(
        Command::new("rustc").args(["--print", "sysroot"]),
        "rustc --print sysroot",
    )?;
    let host = rust_host_triple()?;
    let llvm_nm = Path::new(sysroot.trim())
        .join("lib")
        .join("rustlib")
        .join(host)
        .join("bin")
        .join("llvm-nm");
    if !llvm_nm.exists() {
        run(
            Command::new("rustup").args(["component", "add", "llvm-tools-preview"]),
            "rustup component add llvm-tools-preview",
        )?;
    }
    if !llvm_nm.exists() {
        bail!(
            "{} is missing after installing llvm-tools-preview",
            llvm_nm.display()
        );
    }
    Ok(llvm_nm)
}

fn rust_host_triple() -> Result<String> {
    let version = command_stdout(Command::new("rustc").arg("-vV"), "rustc -vV")?;
    for line in version.lines() {
        if let Some(host) = line.strip_prefix("host: ") {
            return Ok(host.trim().to_string());
        }
    }
    bail!("rustc -vV did not report a host triple")
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
        let rejectedReader = try FreedomIpfsReader()
        do {{
            try rejectedReader.startGateway(address: "0.0.0.0:0")
            fatalError("non-loopback gateway bind unexpectedly succeeded")
        }} catch FreedomIpfsReaderError.startGatewayFailed {{
        }} catch {{
            fatalError("unexpected non-loopback gateway bind error: \(error)")
        }}
        guard rejectedReader.gatewayURL == nil else {{
            fatalError("rejected gateway unexpectedly reported a URL")
        }}

        let reader = try FreedomIpfsReader()
        try reader.importCar(Data([{fixture_car}]))
        try reader.startOnlineGateway(
            delegatedRouter: "http://127.0.0.1:9/routing/v1",
            routingMode: .delegated,
            maxConcurrentRequests: 1
        )
        guard reader.gatewayURL != nil else {{
            fatalError("gateway URL missing")
        }}
        guard reader.activePreloadCount == 0 else {{
            fatalError("unexpected active preloads before request")
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
        let retrievalStats = reader.retrievalStats
        guard retrievalStats.cacheHits > 0,
              retrievalStats.httpProviderBlocks == 0,
              retrievalStats.bitswapBlocks == 0 else {{
            fatalError("unexpected retrieval stats: \(retrievalStats)")
        }}
        let routingStats = reader.routingStats
        guard routingStats.delegatedProviderLookups == 0,
              routingStats.dhtProviderLookups == 0 else {{
            fatalError("cached fixture unexpectedly routed: \(routingStats)")
        }}
        let diagnostics = reader.diagnostics
        guard diagnostics.stats.blockCount == 1,
              diagnostics.retrievalStats.cacheHits > 0,
              diagnostics.retrievalStats.httpProviderBlocks == 0,
              diagnostics.retrievalStats.bitswapBlocks == 0,
              diagnostics.routingStats.delegatedProviderLookups == 0,
              diagnostics.routingStats.dhtProviderLookups == 0,
              diagnostics.activePreloadCount == 0,
              diagnostics.isGatewayRunning,
              !diagnostics.isBackgrounded else {{
            fatalError("unexpected diagnostics snapshot: \(diagnostics)")
        }}
        guard reader.enterBackground(),
              reader.diagnostics.isBackgrounded else {{
            fatalError("background lifecycle hook did not update diagnostics")
        }}
        guard reader.enterForeground(),
              !reader.diagnostics.isBackgrounded else {{
            fatalError("foreground lifecycle hook did not update diagnostics")
        }}
        guard reader.handleLowMemory(maxCacheBytes: 1024 * 1024) else {{
            fatalError("low-memory hook failed")
        }}
        guard reader.handleNetworkChange() else {{
            fatalError("network-change hook failed")
        }}
        try reader.setRoutingMode(
            .delegated,
            delegatedRouters: ["http://127.0.0.1:9/routing/v1"],
            maxConcurrentRequests: 1
        )
        guard reader.gatewayURL != nil,
              reader.diagnostics.isGatewayRunning,
              reader.activePreloadCount == 0 else {{
            fatalError("routing restart did not leave the gateway running")
        }}
        guard let restartedURL = reader.localGatewayURL(for: "/ipfs/{fixture_cid}") else {{
            fatalError("restarted fixture gateway URL missing")
        }}
        let (restartedData, restartedResponse) = try await URLSession.shared.data(from: restartedURL)
        guard (restartedResponse as? HTTPURLResponse)?.statusCode == 200,
              restartedData == Data([{fixture_body}]) else {{
            fatalError("fixture request after routing restart failed")
        }}
        guard reader.diagnostics.activePreloadCount == 0,
              !reader.diagnostics.isBackgrounded else {{
            fatalError("unexpected diagnostics after routing restart: \(reader.diagnostics)")
        }}
        try reader.setRoutingMode(.offline, maxConcurrentRequests: 1)
        guard reader.gatewayURL != nil,
              reader.diagnostics.isGatewayRunning,
              reader.activePreloadCount == 0 else {{
            fatalError("offline routing mode did not leave the gateway running")
        }}
        guard let offlineURL = reader.localGatewayURL(for: "/ipfs/{fixture_cid}") else {{
            fatalError("offline fixture gateway URL missing")
        }}
        let (offlineData, offlineResponse) = try await URLSession.shared.data(from: offlineURL)
        guard (offlineResponse as? HTTPURLResponse)?.statusCode == 200,
              offlineData == Data([{fixture_body}]) else {{
            fatalError("fixture request after offline routing restart failed")
        }}
        guard reader.routingStats == FreedomIpfsRoutingCounters(
            delegatedProviderLookups: 0,
            delegatedProviderResults: 0,
            delegatedProviderErrors: 0,
            dhtProviderLookups: 0,
            dhtProviderResults: 0,
            dhtProviderErrors: 0
        ) else {{
            fatalError("offline routing mode unexpectedly performed routing: \(reader.routingStats)")
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
            .arg("-framework")
            .arg("SystemConfiguration")
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
    )?;

    verify_swift_simulator_app_smoke(
        &verify_dir,
        &headers_dir,
        slice_dir,
        sdk_path.trim(),
        target,
    )
}

fn verify_swift_simulator_app_smoke(
    verify_dir: &Path,
    headers_dir: &Path,
    slice_dir: &Path,
    sdk_path: &str,
    target: &str,
) -> Result<()> {
    let bundle_id = "xyz.floto.freedom-ipfs.AppSmoke";
    let app_dir = verify_dir.join("FreedomIpfsAppSmoke.app");
    if app_dir.exists() {
        fs::remove_dir_all(&app_dir).context("remove previous simulator app smoke bundle")?;
    }
    fs::create_dir_all(&app_dir).context("create simulator app smoke bundle")?;

    let html = br#"<!doctype html><html><head><meta charset="utf-8"><title>Freedom IPFS Smoke</title></head><body><main id="freedom-ipfs-smoke">Freedom IPFS App Smoke</main></body></html>"#;
    let cid = cid_from_data(CODEC_RAW, html);
    let car = encode_car_v1(&[CarBlock {
        cid,
        data: html.to_vec(),
    }]);
    let marker_name = "freedom-ipfs-app-smoke.ok";
    let app_source = format!(
        r#"import Darwin
import Foundation
import UIKit
import WebKit
import FreedomIpfs

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {{
    var window: UIWindow?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {{
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = SmokeViewController()
        window.makeKeyAndVisible()
        self.window = window
        return true
    }}
}}

final class SmokeViewController: UIViewController, WKNavigationDelegate {{
    private let webView = WKWebView(frame: .zero)
    private var reader: FreedomIpfsReader?

    override func viewDidLoad() {{
        super.viewDidLoad()
        webView.navigationDelegate = self
        webView.frame = view.bounds
        webView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.addSubview(webView)

        Task {{
            do {{
                let reader = try FreedomIpfsReader()
                self.reader = reader
                try reader.importCar(Data([{car}]))
                try reader.startGateway()
                guard reader.diagnostics.isGatewayRunning,
                      !reader.diagnostics.isBackgrounded else {{
                    throw SmokeError("unexpected initial app diagnostics")
                }}
                guard reader.enterBackground(),
                      reader.diagnostics.isBackgrounded else {{
                    throw SmokeError("background lifecycle hook failed")
                }}
                guard reader.enterForeground(),
                      !reader.diagnostics.isBackgrounded else {{
                    throw SmokeError("foreground lifecycle hook failed")
                }}
                guard reader.handleLowMemory(maxCacheBytes: 1024 * 1024) else {{
                    throw SmokeError("low-memory hook failed")
                }}
                guard reader.handleNetworkChange() else {{
                    throw SmokeError("network-change hook failed")
                }}
                guard let url = reader.localGatewayURL(for: "/ipfs/{cid}") else {{
                    throw SmokeError("fixture gateway URL missing")
                }}
                let (data, response) = try await URLSession.shared.data(from: url)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else {{
                    throw SmokeError("fixture request failed")
                }}
                guard let html = String(data: data, encoding: .utf8),
                      html.contains("Freedom IPFS App Smoke") else {{
                    throw SmokeError("fixture body mismatch")
                }}
                webView.loadHTMLString(html, baseURL: reader.gatewayURL)
            }} catch {{
                finish("failed: \(error)")
            }}
        }}
    }}

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {{
        webView.evaluateJavaScript("document.getElementById('freedom-ipfs-smoke')?.textContent") {{ result, error in
            if let error {{
                self.finish("failed: \(error)")
                return
            }}
            guard (result as? String) == "Freedom IPFS App Smoke" else {{
                self.finish("failed: rendered marker missing")
                return
            }}
            self.finish("ok")
        }}
    }}

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {{
        finish("failed: \(error)")
    }}

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {{
        finish("failed: \(error)")
    }}

    private func finish(_ message: String) {{
        if let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first {{
            let marker = documents.appendingPathComponent("{marker_name}")
            try? Data(message.utf8).write(to: marker)
        }}
        _ = reader?.stopGateway()
        exit(message == "ok" ? 0 : 1)
    }}
}}

struct SmokeError: Error, CustomStringConvertible {{
    let description: String

    init(_ description: String) {{
        self.description = description
    }}
}}
"#,
        car = format_swift_byte_array(&car),
        cid = cid,
        marker_name = marker_name
    );
    let app_source_path = verify_dir.join("FreedomIpfsAppSmoke.swift");
    fs::write(&app_source_path, app_source).context("write Swift simulator app smoke source")?;

    let info_plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>FreedomIpfsAppSmoke</string>
    <key>CFBundleIdentifier</key>
    <string>{bundle_id}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>FreedomIpfsAppSmoke</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSRequiresIPhoneOS</key>
    <true/>
    <key>NSAppTransportSecurity</key>
    <dict>
        <key>NSAllowsLocalNetworking</key>
        <true/>
    </dict>
    <key>UIDeviceFamily</key>
    <array>
        <integer>1</integer>
    </array>
</dict>
</plist>
"#
    );
    fs::write(app_dir.join("Info.plist"), info_plist).context("write app smoke Info.plist")?;

    run(
        Command::new("xcrun")
            .args(["--sdk", "iphonesimulator", "swiftc"])
            .arg("-target")
            .arg(target)
            .arg("-sdk")
            .arg(sdk_path)
            .arg("-I")
            .arg(headers_dir)
            .arg("-L")
            .arg(slice_dir)
            .arg("-l")
            .arg("freedom_ipfs_mobile")
            .arg("-framework")
            .arg("SystemConfiguration")
            .arg("-framework")
            .arg("UIKit")
            .arg("-framework")
            .arg("WebKit")
            .arg("ffi/swift/FreedomIpfsReader.swift")
            .arg(&app_source_path)
            .arg("-o")
            .arg(app_dir.join("FreedomIpfsAppSmoke")),
        "swiftc simulator app smoke",
    )?;
    run(
        Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&app_dir),
        "codesign simulator app smoke",
    )?;
    let _ = Command::new("xcrun")
        .args(["simctl", "uninstall", "booted", bundle_id])
        .status();
    run(
        Command::new("xcrun")
            .args(["simctl", "install", "booted"])
            .arg(&app_dir),
        "install simulator app smoke",
    )?;
    let app_container = command_stdout(
        Command::new("xcrun").args(["simctl", "get_app_container", "booted", bundle_id, "data"]),
        "get simulator app smoke container",
    )?;
    let marker = Path::new(app_container.trim())
        .join("Documents")
        .join(marker_name);
    if marker.exists() {
        fs::remove_file(&marker).with_context(|| format!("remove {}", marker.display()))?;
    }
    run(
        Command::new("xcrun").args(["simctl", "launch", "--console", "booted", bundle_id]),
        "launch simulator app smoke",
    )?;
    wait_for_app_smoke_marker(&marker)
}

fn wait_for_app_smoke_marker(marker: &Path) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        if marker.exists() {
            let contents =
                fs::read_to_string(marker).with_context(|| format!("read {}", marker.display()))?;
            if contents.trim() == "ok" {
                return Ok(());
            }
            bail!("simulator app smoke failed: {}", contents.trim());
        }
        thread::sleep(Duration::from_millis(500));
    }
    bail!(
        "simulator app smoke did not write {} within 60 seconds",
        marker.display()
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
