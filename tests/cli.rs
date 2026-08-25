use assert_cmd::Command;
use httpmock::Method::GET;
use httpmock::MockServer;
use predicates::prelude::*;

const BOOK_ID: &str = "12345678-1234-5678-1234-567812345678";

#[test]
fn books_lists_authenticated_catalog() {
    let server = MockServer::start();
    let catalog = format!(
        r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><id>urn:uuid:{BOOK_ID}</id><title>Rust Docs</title><link href="/content/rust_docs" /></entry></feed>"#
    );
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/catalog/v2/entries")
            .query_param("count", "-1")
            .header("authorization", "Basic d2lraTpzZWNyZXQ=");
        then.status(200).body(catalog);
    });

    Command::cargo_bin("kiwix-cli")
        .unwrap()
        .env("KIWIX_URL", server.base_url())
        .env("KIWIX_USERNAME", "wiki")
        .env("KIWIX_PASSWORD", "secret")
        .arg("books")
        .assert()
        .success()
        .stdout(predicate::str::contains("Rust Docs"))
        .stdout(predicate::str::contains("rust_docs"));
    mock.assert();
}

#[test]
fn search_builds_kiwix_query_and_prints_locator() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/search")
            .query_param("books.id", BOOK_ID)
            .query_param("pattern", "structured concurrency")
            .query_param("start", "0")
            .query_param("pageLength", "20")
            .query_param("format", "xml");
        then.status(200).body(
            r#"<rss xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/"><channel>
            <opensearch:totalResults>258,827</opensearch:totalResults><opensearch:startIndex>0</opensearch:startIndex>
            <opensearch:itemsPerPage>20</opensearch:itemsPerPage><item><title>Rust async</title>
            <link>/content/rust_docs/A/Async</link><description>Useful result</description></item></channel></rss>"#,
        );
    });

    Command::cargo_bin("kiwix-cli")
        .unwrap()
        .env("KIWIX_URL", server.base_url())
        .args(["search", "--book", BOOK_ID, "structured concurrency"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Results 1-1 of 258827"))
        .stdout(predicate::str::contains("/content/rust_docs/A/Async"));
    mock.assert();
}

#[test]
fn read_fetches_raw_article_and_renders_text() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).path("/raw/rust_docs/content/A/Async");
        then.status(200)
            .body("<html><body><h1>Async Rust</h1><p>Works over SSH.</p></body></html>");
    });

    Command::cargo_bin("kiwix-cli")
        .unwrap()
        .env("KIWIX_URL", server.base_url())
        .args(["read", "/content/rust_docs/A/Async", "--width", "60"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Async Rust"))
        .stdout(predicate::str::contains("Works over SSH."));
    mock.assert();
}

#[test]
fn refuses_cross_origin_article_locator() {
    let server = MockServer::start();
    Command::cargo_bin("kiwix-cli")
        .unwrap()
        .env("KIWIX_URL", server.base_url())
        .args(["read", "https://example.invalid/content/wiki/A/Secret"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "must stay on the configured Kiwix server",
        ));
}

#[test]
fn interactive_mode_reports_a_clear_error_without_a_terminal() {
    let server = MockServer::start();
    Command::cargo_bin("kiwix-cli")
        .unwrap()
        .env("KIWIX_URL", server.base_url())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "interactive mode requires a terminal",
        ));
}

#[test]
fn random_follows_a_validated_locator_through_the_raw_endpoint() {
    let server = MockServer::start();
    let random = server.mock(|when, then| {
        when.method(GET)
            .path("/random")
            .query_param("content", "wikivoyage_en_all_maxi_2026-06");
        then.status(302)
            .header("location", "/content/wikivoyage_en_all_maxi_2026-06/Feroke");
    });
    let article = server.mock(|when, then| {
        when.method(GET)
            .path("/raw/wikivoyage_en_all_maxi_2026-06/content/Feroke");
        then.status(200)
            .body("<html><body><h1>Feroke</h1><p>Random destination.</p></body></html>");
    });

    Command::cargo_bin("kiwix-cli")
        .unwrap()
        .env("KIWIX_URL", server.base_url())
        .args([
            "random",
            "--content",
            "wikivoyage_en_all_maxi_2026-06",
            "--width",
            "60",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Feroke"))
        .stdout(predicate::str::contains("Random destination."));
    random.assert();
    article.assert();
}

#[test]
fn random_rejects_a_redirect_to_another_content_library() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/random");
        then.status(302)
            .header("location", "/content/another_library/Article");
    });

    Command::cargo_bin("kiwix-cli")
        .unwrap()
        .env("KIWIX_URL", server.base_url())
        .args(["random", "--content", "wikivoyage_en_all_maxi_2026-06"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "outside the requested content library",
        ));
}

#[test]
fn home_follows_the_library_entrypoint_and_renders_the_article() {
    let server = MockServer::start();
    let home = server.mock(|when, then| {
        when.method(GET).path("/content/wiki");
        then.status(302)
            .header("location", "/content/wiki/Home_Page");
    });
    let article = server.mock(|when, then| {
        when.method(GET).path("/raw/wiki/content/Home_Page");
        then.status(200)
            .body("<html><body><h1>Wiki home</h1><p>Welcome.</p></body></html>");
    });

    Command::cargo_bin("kiwix-cli")
        .unwrap()
        .env("KIWIX_URL", server.base_url())
        .args(["home", "--content", "wiki", "--width", "60"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Wiki home"))
        .stdout(predicate::str::contains("Welcome."));
    home.assert();
    article.assert();
}
