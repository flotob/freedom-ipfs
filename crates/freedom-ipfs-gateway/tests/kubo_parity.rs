use axum::http::header::{CONTENT_RANGE, CONTENT_TYPE, RANGE};
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

    let expected_index = kubo_stdout(
        &kubo,
        &repo,
        [
            OsStr::new("cat"),
            OsStr::new(&format!("/ipfs/{root}/index.html")),
        ],
    );
    let response = reqwest::get(format!("http://{addr}/ipfs/{root}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "text/html"
    );
    assert_eq!(response.bytes().await.unwrap().as_ref(), expected_index);

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

    let range_start = 123_456usize;
    let range_end = 124_567usize;
    let url = format!("http://{addr}/ipfs/{root}/assets/blob.bin");
    let response = reqwest::Client::new()
        .get(url)
        .header(RANGE, format!("bytes={range_start}-{range_end}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_RANGE)
            .unwrap()
            .to_str()
            .unwrap(),
        format!("bytes {range_start}-{range_end}/{}", large.len())
    );
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        &large[range_start..=range_end]
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires KUBO_BIN=/path/to/ipfs; generates a local Kubo HAMT fixture"]
async fn kubo_generated_hamt_directory_matches_local_gateway_bytes() {
    let kubo = env::var("KUBO_BIN").expect("set KUBO_BIN=/path/to/ipfs");
    let tempdir = tempfile::tempdir().unwrap();
    let repo = tempdir.path().join("kubo-repo");
    let site = tempdir.path().join("hamt");
    fs::create_dir_all(&site).unwrap();
    for index in 0..64 {
        fs::write(
            site.join(format!("file-{index:02}.txt")),
            format!("hamt-file-{index:02}\n"),
        )
        .unwrap();
    }

    kubo_ok(&kubo, &repo, ["init", "--empty-repo"]);
    let root = kubo_stdout(
        &kubo,
        &repo,
        [
            OsStr::new("add"),
            OsStr::new("-Qr"),
            OsStr::new("--cid-version=1"),
            OsStr::new("--raw-leaves=true"),
            OsStr::new("--max-directory-links=1"),
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

    for path in ["file-00.txt", "file-17.txt", "file-63.txt"] {
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
