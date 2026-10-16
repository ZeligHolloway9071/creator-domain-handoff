mod creator_domain_service;
mod infrai_client;
mod webhook_signature;

use creator_domain_service::{accept_verified_domain, onboard, DeliveryState, MediaOnboarding};
use infrai_client::InfraiClient;
use std::{
    future::Future,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    pin::Pin,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

fn main() {
    if let Err(error) = block_on(run()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let domain = args
        .next()
        .ok_or("usage: creator-domain-handoff <domain> <creator-origin>")?;
    let creator_origin = args
        .next()
        .ok_or("usage: creator-domain-handoff <domain> <creator-origin>")?;
    let webhook_url = std::env::var("DOMAIN_WEBHOOK_URL")?;
    let webhook_secret = std::env::var("DOMAIN_WEBHOOK_SECRET")?;
    let bind_address = std::env::var("WEBHOOK_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let listener = TcpListener::bind(&bind_address)?;
    let client = InfraiClient::from_env()?;
    let media = MediaOnboarding {
        asset_id: "asset-demo-001".into(),
        processing_job_id: "transcode-demo-001".into(),
        domain,
        creator_origin,
        state: DeliveryState::Processing,
    };
    let pending = onboard(&client, media, &webhook_url, &webhook_secret).await?;
    println!(
        "asset={} job={} domain={} state=awaiting_domain",
        pending.asset_id, pending.processing_job_id, pending.domain
    );
    serve_webhooks(listener, pending, &webhook_secret)
}

fn serve_webhooks(
    listener: TcpListener,
    mut media: MediaOnboarding,
    secret: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for connection in listener.incoming() {
        let mut stream = connection?;
        let request = read_request(&mut stream)?;
        let signature = header(&request.headers, "x-infrai-signature").unwrap_or_default();
        match accept_verified_domain(&mut media, request.body.as_bytes(), signature, secret) {
            Ok(body) => write_response(&mut stream, "200 OK", &body)?,
            Err(error) => {
                let status = if matches!(
                    error,
                    creator_domain_service::ServiceError::InvalidSignature
                ) {
                    "401 Unauthorized"
                } else {
                    "400 Bad Request"
                };
                write_response(&mut stream, status, r#"{"accepted":false}"#)?;
            }
        }
    }
    Ok(())
}

struct HttpRequest {
    headers: String,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<HttpRequest> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(split) = find_bytes(&bytes, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..split]).into_owned();
            let length = header(&headers, "content-length")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            if bytes.len() >= split + 4 + length {
                let body =
                    String::from_utf8_lossy(&bytes[split + 4..split + 4 + length]).into_owned();
                return Ok(HttpRequest { headers, body });
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "incomplete HTTP request",
    ))
}

fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_response(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn block_on<F: Future>(mut future: F) -> F::Output {
    fn raw_waker() -> RawWaker {
        fn clone(_: *const ()) -> RawWaker {
            raw_waker()
        }
        fn noop(_: *const ()) {}
        RawWaker::new(
            std::ptr::null(),
            &RawWakerVTable::new(clone, noop, noop, noop),
        )
    }
    let waker = unsafe { Waker::from_raw(raw_waker()) };
    let mut context = Context::from_waker(&waker);
    let mut future = unsafe { Pin::new_unchecked(&mut future) };
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        std::thread::yield_now();
    }
}
