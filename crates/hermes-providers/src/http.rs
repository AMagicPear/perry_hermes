use std::error::Error;
use std::time::Duration;

pub(crate) fn streaming_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(300))
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate()
        .build()
        .expect("reqwest client")
}

pub(crate) fn transport_error_message(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(err) = source {
        message.push_str(": ");
        message.push_str(&err.to_string());
        source = err.source();
    }
    message
}
