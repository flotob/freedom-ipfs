use freedom_ipfs_core::{cid_from_data, encode_car_v1, CarBlock, CODEC_RAW};
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[tokio::test]
async fn explicit_offline_routing_runs_cache_only_gateway() {
    let body = b"cli offline fixture";
    let cid = cid_from_data(CODEC_RAW, body);
    let car = encode_car_v1(&[CarBlock {
        cid,
        data: body.to_vec(),
    }]);

    let tempdir = tempfile::tempdir().expect("create tempdir");
    let car_path = tempdir.path().join("fixture.car");
    fs::write(&car_path, car).expect("write fixture CAR");

    let mut child = ChildGuard::spawn(
        Command::new(env!("CARGO_BIN_EXE_freedom-ipfs-gateway"))
            .arg("--online")
            .arg("--routing-mode")
            .arg("offline")
            .arg("--import-car")
            .arg(&car_path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let stderr = child
        .0
        .stderr
        .take()
        .expect("gateway stderr should be piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if tx.send(line.unwrap_or_default()).is_err() {
                break;
            }
        }
    });

    let addr = wait_for_gateway_addr(&mut child, &rx);
    let response = reqwest::get(format!("http://{addr}/ipfs/{cid}"))
        .await
        .expect("fetch fixture from CLI gateway");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.bytes().await.expect("read fixture body").as_ref(),
        body
    );
}

struct ChildGuard(Child);

impl ChildGuard {
    fn spawn(command: &mut Command) -> Self {
        Self(command.spawn().expect("spawn gateway"))
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().expect("poll gateway").is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn wait_for_gateway_addr(child: &mut ChildGuard, rx: &mpsc::Receiver<String>) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                if let Some(addr) = line.strip_prefix("gateway listening on http://") {
                    return addr.to_string();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.0.try_wait().expect("poll gateway") {
                    panic!("gateway exited before listening: {status}");
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("gateway stderr closed before listening");
            }
        }
    }

    panic!("timed out waiting for gateway listen address");
}
