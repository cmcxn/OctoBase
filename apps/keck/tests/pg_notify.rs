use std::{io::{BufRead, BufReader}, process::{Child, Command, Stdio}, thread::sleep, time::Duration};

use rand::{thread_rng, Rng};

fn start_server(port: u16, db: &str) -> Child {
    let mut child = Command::new("cargo")
        .args(["run", "-p", "keck"])
        .env("KECK_PORT", port.to_string())
        .env("DATABASE_URL", db)
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to run command");

    if let Some(ref mut stdout) = child.stdout {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = line.expect("Failed to read line");
            if line.contains("listening on 0.0.0.0:") {
                break;
            }
        }
    }

    child
}

#[tokio::test]
#[ignore = "requires external postgres"]
async fn blocks_consistent_between_nodes() {
    let port1 = thread_rng().gen_range(20000..30000);
    let port2 = port1 + 1;
    let db = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/postgres".into());
    let c1 = start_server(port1, &db);
    let c2 = start_server(port2, &db);

    let client = reqwest::Client::new();
    let ws = "ws1";
    let block = "b1";
    let url1 = format!("http://localhost:{port1}/api/block/{ws}/{block}?flavour=text");
    client
        .post(url1)
        .json(&serde_json::json!({"prop:text": "hi"}))
        .send()
        .await
        .unwrap();
    sleep(Duration::from_secs(1));
    let url2 = format!("http://localhost:{port2}/api/block/{ws}/{block}");
    let resp = client.get(url2).send().await.unwrap();
    assert!(resp.status().is_success());
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["prop:text"], "hi");

    unsafe { libc::kill(c1.id() as i32, libc::SIGTERM) };
    unsafe { libc::kill(c2.id() as i32, libc::SIGTERM) };
}
