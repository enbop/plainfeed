use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let address = arguments
        .next()
        .unwrap_or_else(|| "127.0.0.1:18090".to_owned());
    let data_root = PathBuf::from(arguments.next().unwrap_or_else(|| "/data".to_owned()));

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run(address, data_root))?;
    Ok(())
}

async fn run(address: String, data_root: PathBuf) -> io::Result<()> {
    let listener = TcpListener::bind(&address).await?;
    let ticks = Arc::new(AtomicU64::new(0));
    let heartbeat_ticks = Arc::clone(&ticks);
    let heartbeat_root = data_root.clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        loop {
            interval.tick().await;
            let tick = heartbeat_ticks.fetch_add(1, Ordering::Relaxed) + 1;
            if let Err(error) = write_heartbeat(&heartbeat_root, tick) {
                eprintln!("heartbeat write failed: {error}");
            }
        }
    });

    println!("listening={address}");
    loop {
        let (stream, _) = listener.accept().await?;
        handle_connection(stream, Arc::clone(&ticks)).await?;
    }
}

async fn handle_connection(mut stream: TcpStream, ticks: Arc<AtomicU64>) -> io::Result<()> {
    let mut request = [0_u8; 4096];
    let length = stream.read(&mut request).await?;
    let request = String::from_utf8_lossy(&request[..length]);
    let path = request.split_whitespace().nth(1).unwrap_or("/");

    let (status, content_type, body) = match path {
        "/health" => ("200 OK", "text/plain", "ok\n".to_owned()),
        "/experiment/status" => (
            "200 OK",
            "text/plain",
            format!(
                "format=plainfeed.service-experiment/v1\nticks={}\n",
                ticks.load(Ordering::Relaxed)
            ),
        ),
        _ => ("404 Not Found", "text/plain", "not found\n".to_owned()),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

fn write_heartbeat(data_root: &Path, tick: u64) -> io::Result<()> {
    let metadata = data_root.join(".plainfeed");
    fs::create_dir_all(&metadata)?;
    let temporary = metadata.join("wasmtime-run-experiment.toml.tmp");
    let target = metadata.join("wasmtime-run-experiment.toml");
    fs::write(
        &temporary,
        format!("format = \"plainfeed.service-experiment/v1\"\nticks = {tick}\n"),
    )?;
    fs::rename(temporary, target)
}
