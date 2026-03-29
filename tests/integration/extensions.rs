use crate::{fixtures_dir, TestServer};

#[tokio::test]
async fn markdown_rendered_to_html() {
    let server = TestServer::start(&fixtures_dir(), true);
    let resp = server.get("/test.md").await;

    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("text/html"), "content-type was: {}", ct);
    let body = resp.text().await.unwrap();
    // Comrak should render **test** as <strong>test</strong>.
    assert!(body.contains("<strong>test</strong>"), "body: {}", body);
}

#[tokio::test]
async fn directory_listing_without_index() {
    let server = TestServer::start(&fixtures_dir(), true);
    let resp = server.get("/subdir/").await;

    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("text/html"), "content-type was: {}", ct);
    let body = resp.text().await.unwrap();
    // Should contain a link for parent directory navigation.
    assert!(body.contains(".."), "body should contain '..' link: {}", body);
}

#[tokio::test]
async fn directory_listing_sorted() {
    let server = TestServer::start(&fixtures_dir(), true);
    // Use a directory without index.html so the listing is generated.
    let resp = server.get("/multi/").await;

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("aaa.txt"), "body: {}", body);
    assert!(body.contains("zzz.txt"), "body: {}", body);

    // Verify sorted order: "aaa.txt" should appear before "zzz.txt".
    let pos_aaa = body.find("aaa.txt").unwrap();
    let pos_zzz = body.find("zzz.txt").unwrap();
    assert!(
        pos_aaa < pos_zzz,
        "directory listing should be sorted"
    );
}

#[tokio::test]
async fn source_code_as_plain_text() {
    let server = TestServer::start(&fixtures_dir(), true);
    let resp = server.get("/test.rs").await;

    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("text/plain"), "content-type was: {}", ct);
}

#[tokio::test]
async fn static_html_still_works_with_extensions() {
    let server = TestServer::start(&fixtures_dir(), true);
    let resp = server.get("/index.html").await;

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("Hello from fixtures"));
}

#[tokio::test]
async fn method_not_allowed_with_extensions() {
    let server = TestServer::start(&fixtures_dir(), true);
    let resp = server.request(reqwest::Method::POST, "/index.html").await;

    assert_eq!(resp.status(), 405);
    let allow = resp.headers().get("allow").unwrap().to_str().unwrap();
    assert_eq!(allow, "GET");
}
