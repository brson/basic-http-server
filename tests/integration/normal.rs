use crate::{fixtures_dir, TestServer};

#[tokio::test]
async fn static_file_serving() {
    let server = TestServer::start(&fixtures_dir(), false);
    let resp = server.get("/index.html").await;

    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("text/html"), "content-type was: {}", ct);
    let body = resp.text().await.unwrap();
    assert!(body.contains("Hello from fixtures"));
}

#[tokio::test]
async fn content_length_header() {
    let server = TestServer::start(&fixtures_dir(), false);
    let resp = server.get("/index.html").await;

    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("content-length").is_some());
}

#[tokio::test]
async fn not_found_returns_404() {
    let server = TestServer::start(&fixtures_dir(), false);
    let resp = server.get("/nonexistent").await;

    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn method_not_allowed() {
    let server = TestServer::start(&fixtures_dir(), false);
    let resp = server.request(reqwest::Method::POST, "/index.html").await;

    assert_eq!(resp.status(), 405);
    let allow = resp.headers().get("allow").unwrap().to_str().unwrap();
    assert_eq!(allow, "GET");
}

#[tokio::test]
async fn directory_redirect() {
    let server = TestServer::start(&fixtures_dir(), false);
    let resp = server.get("/subdir-with-index").await;

    // ServeDir uses 307 for trailing-slash redirects (old code used 302).
    assert_eq!(resp.status(), 307, "expected redirect, got {}", resp.status());
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.ends_with("/subdir-with-index/"), "location: {}", location);
}

#[tokio::test]
async fn directory_serves_index_html() {
    let server = TestServer::start(&fixtures_dir(), false);
    let resp = server.get("/subdir-with-index/").await;

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("Index in subdir"));
}

#[tokio::test]
async fn percent_encoded_path() {
    let server = TestServer::start(&fixtures_dir(), false);
    let resp = server.get("/file%20with%20spaces.html").await;

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("File with spaces"));
}

#[tokio::test]
async fn binary_file_octet_stream() {
    let server = TestServer::start(&fixtures_dir(), false);
    let resp = server.get("/binary.bin").await;

    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(
        ct.contains("octet-stream"),
        "expected octet-stream, got: {}",
        ct
    );
}

#[tokio::test]
async fn directory_without_index_returns_404() {
    let server = TestServer::start(&fixtures_dir(), false);
    // subdir has no index.html, so without -x it should 404.
    let resp = server.get("/subdir/").await;

    assert_eq!(resp.status(), 404);
}
