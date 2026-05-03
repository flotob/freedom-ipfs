use axum::http::StatusCode;
use freedom_ipfs_gateway::router;
use freedom_ipfs_store::SqliteBlockStore;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;
use tokio::net::TcpListener;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires KUBO_BIN=/path/to/ipfs; generates a local Kubo CAR fixture"]
async fn kubo_generated_unixfs_site_matches_local_gateway_bytes() {
    let kubo = env::var("KUBO_BIN").expect("set KUBO_BIN=/path/to/ipfs");
    let tempdir = tempfile::tempdir().unwrap();
    let repo = tempdir.path().join("kubo-repo");
    let site = tempdir.path().join("site");
    fs::create_dir_all(site.join("assets")).unwrap();
    fs::write(
        site.join("index.html"),
        b"<html><body>freedom</body></html>",
    )
    .unwrap();
    fs::write(site.join("assets/style.css"), b"body { color: #111; }\n").unwrap();
    let large = (0..600_000)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    fs::write(site.join("assets/blob.bin"), &large).unwrap();

    kubo_ok(&kubo, &repo, ["init", "--empty-repo"]);
    let root = kubo_stdout(
        &kubo,
        &repo,
        [
            OsStr::new("add"),
            OsStr::new("-Qr"),
            OsStr::new("--cid-version=1"),
            OsStr::new("--raw-leaves=true"),
            site.as_os_str(),
        ],
    );
    let root = String::from_utf8(root).unwrap().trim().to_string();
    let car = kubo_stdout(
        &kubo,
        &repo,
        [OsStr::new("dag"), OsStr::new("export"), OsStr::new(&root)],
    );

    let store = SqliteBlockStore::in_memory(16 * 1024 * 1024).unwrap();
    store.import_car(&car).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(store);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    for path in ["index.html", "assets/style.css", "assets/blob.bin"] {
        let kubo_path = format!("/ipfs/{root}/{path}");
        let expected = kubo_stdout(&kubo, &repo, [OsStr::new("cat"), OsStr::new(&kubo_path)]);
        let url = format!("http://{addr}/ipfs/{root}/{path}");
        let response = reqwest::get(url).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            expected.as_slice()
        );
    }
}

fn kubo_ok<I, S>(kubo: &str, repo: &Path, args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = kubo_command(kubo, repo, args).output().unwrap();
    assert!(
        output.status.success(),
        "kubo command failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn kubo_stdout<I, S>(kubo: &str, repo: &Path, args: I) -> Vec<u8>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = kubo_command(kubo, repo, args).output().unwrap();
    assert!(
        output.status.success(),
        "kubo command failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn kubo_command<I, S>(kubo: &str, repo: &Path, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(kubo);
    command.env("IPFS_PATH", repo).args(args);
    command
}
