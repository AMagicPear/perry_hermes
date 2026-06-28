use std::error::Error;
use std::time::Duration;

pub(crate) fn streaming_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(300))
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate();

    // On Android (Termux), reqwest 0.13's default rustls feature enables
    // `rustls-platform-verifier`, which calls into Android's JVM to verify
    // certificates. Termux has no JVM, so the verifier panics on the first
    // handshake ("Expect rustls-platform-verifier to be initialized"). We
    // bypass it by preconfiguring rustls with Mozilla's bundled root CAs.
    #[cfg(target_os = "android")]
    {
        builder = builder.tls_backend_preconfigured(build_webpki_client_config());
    }

    builder.build().expect("reqwest client")
}

#[cfg(target_os = "android")]
fn build_webpki_client_config() -> rustls::ClientConfig {
    use rustls::RootCertStore;

    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("safe default TLS versions are supported by aws-lc-rs")
    .with_root_certificates(root_store)
    .with_no_client_auth()
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
