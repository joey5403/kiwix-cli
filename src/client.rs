use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use percent_encoding::percent_decode_str;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, USER_AGENT};
use reqwest::redirect::Policy;
use url::Url;
use uuid::Uuid;

use crate::model::{Book, SearchPage};
use crate::xml::{parse_catalog, parse_search};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PAGE_LENGTH: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArticleReference {
    Internal {
        locator: String,
        fragment: Option<String>,
    },
    External(String),
}

#[derive(Debug)]
pub(crate) enum ImageResource {
    Downloaded { bytes: Vec<u8>, extension: String },
    External(String),
}

#[derive(Clone)]
pub struct KiwixClient {
    base: Url,
    http: Client,
    username: Option<String>,
    password: Option<String>,
}

impl KiwixClient {
    /// Creates a client for one Kiwix server and optional Basic Auth credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the URL or credential pair is invalid, or when the HTTP client
    /// cannot be initialized.
    pub fn new(
        server: &str,
        username: Option<String>,
        password: Option<String>,
        timeout: Duration,
    ) -> Result<Self> {
        let mut base = Url::parse(server).context("invalid Kiwix server URL")?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            bail!("Kiwix server URL must use http or https and include a host");
        }
        if !base.username().is_empty() || base.password().is_some() {
            bail!("put credentials in KIWIX_USERNAME and KIWIX_PASSWORD, not in the URL");
        }
        if base.query().is_some() || base.fragment().is_some() {
            bail!("Kiwix server URL must not contain a query or fragment");
        }
        if username.is_some() != password.is_some() {
            bail!("KIWIX_USERNAME and KIWIX_PASSWORD must be set together");
        }
        let normalized_path = format!("{}/", base.path().trim_end_matches('/'));
        base.set_path(&normalized_path);

        let http = Client::builder()
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .timeout(timeout)
            .redirect(Policy::none())
            .build()
            .context("failed to create HTTP client")?;
        Ok(Self {
            base,
            http,
            username,
            password,
        })
    }

    /// Loads and parses the complete OPDS library catalog.
    ///
    /// # Errors
    ///
    /// Returns an error for transport failures, rejected authentication, oversized responses,
    /// or an invalid OPDS document.
    pub fn list_books(&self) -> Result<Vec<Book>> {
        let mut url = self.base.clone();
        url.set_path("/catalog/v2/entries");
        url.query_pairs_mut().append_pair("count", "-1");
        let body = self.get_text(&url, "application/atom+xml;profile=opds-catalog")?;
        parse_catalog(&body).context("failed to parse Kiwix catalog")
    }

    pub(crate) fn server_key(&self) -> &str {
        self.base.as_str()
    }

    /// Searches one library by its catalog UUID.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid arguments, transport failures, or an invalid RSS response.
    pub fn search(
        &self,
        book_id: &str,
        query: &str,
        start: usize,
        limit: usize,
    ) -> Result<SearchPage> {
        let book_id = Uuid::parse_str(book_id).context("--book must be a catalog UUID")?;
        if query.trim().is_empty() {
            bail!("search query must not be empty");
        }
        if limit == 0 || limit > MAX_PAGE_LENGTH {
            bail!("--limit must be between 1 and {MAX_PAGE_LENGTH}");
        }
        let mut url = self.base.clone();
        let path = format!("{}/search", self.base.path().trim_end_matches('/'));
        url.set_path(&path);
        url.query_pairs_mut()
            .append_pair("books.id", &book_id.to_string())
            .append_pair("pattern", query.trim())
            .append_pair("start", &start.to_string())
            .append_pair("pageLength", &limit.to_string())
            .append_pair("format", "xml");
        let body = self.get_text(
            &url,
            "application/rss+xml, application/xml;q=0.9, text/xml;q=0.8",
        )?;
        let mut page = parse_search(&body).context("failed to parse Kiwix search response")?;
        if page.start != start {
            bail!("Kiwix search response returned an unexpected start index");
        }
        for result in &mut page.results {
            result.locator = self.normalize_locator(&result.locator)?;
        }
        Ok(page)
    }

    /// Retrieves article HTML from a validated Kiwix content locator.
    ///
    /// # Errors
    ///
    /// Returns an error when the locator is unsafe or the article request fails.
    pub fn read_article(&self, locator: &str) -> Result<String> {
        let url = self.raw_url(locator)?;
        self.get_text(&url, "text/html, application/xhtml+xml;q=0.9")
    }

    /// Resolves the random-article redirect for a content library.
    ///
    /// # Errors
    ///
    /// Returns an error when the content ID is invalid or the redirect leaves the requested
    /// library.
    pub fn random_locator(&self, content_id: &str) -> Result<String> {
        validate_content_id(content_id)?;
        let mut url = self.base.clone();
        let path = format!("{}/random", self.base.path().trim_end_matches('/'));
        url.set_path(&path);
        url.query_pairs_mut().append_pair("content", content_id);
        self.redirected_content_locator(&url, content_id, "random")
    }

    /// Resolves the home-page redirect for a content library.
    ///
    /// # Errors
    ///
    /// Returns an error when the content ID is invalid or the redirect leaves the requested
    /// library.
    pub fn home_locator(&self, content_id: &str) -> Result<String> {
        validate_content_id(content_id)?;
        let mut url = self.base.clone();
        let base_path = self.base.path().trim_end_matches('/');
        url.set_path(&format!("{base_path}/content/{content_id}"));
        self.redirected_content_locator(&url, content_id, "home")
    }

    fn redirected_content_locator(
        &self,
        url: &Url,
        content_id: &str,
        endpoint: &str,
    ) -> Result<String> {
        let request = self
            .http
            .get(url.clone())
            .header(USER_AGENT, concat!("kiwix-cli/", env!("CARGO_PKG_VERSION")))
            .header(ACCEPT, "text/html");
        let response = self
            .authenticate(request)
            .send()
            .with_context(|| format!("request to {} failed", display_url(url)))?;
        let status = response.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            bail!("Kiwix server rejected the configured credentials ({status})");
        }
        if !status.is_redirection() {
            bail!("Kiwix {endpoint} endpoint returned {status} instead of a redirect");
        }
        let location = response
            .headers()
            .get(LOCATION)
            .with_context(|| format!("Kiwix {endpoint} response has no Location header"))?
            .to_str()
            .with_context(|| format!("Kiwix {endpoint} response has an invalid Location header"))?;
        let locator = self.normalize_locator(location)?;
        let base_path = self.base.path().trim_end_matches('/');
        let expected_prefix = format!("{base_path}/content/{content_id}/");
        if !locator.starts_with(&expected_prefix) || locator.len() <= expected_prefix.len() {
            bail!("Kiwix {endpoint} response points outside the requested content library");
        }
        Ok(locator)
    }

    pub(crate) fn resolve_article_reference(
        &self,
        current_locator: &str,
        reference: &str,
    ) -> Result<ArticleReference> {
        if reference.is_empty() || reference.chars().any(char::is_control) {
            bail!("article reference is empty or contains control characters");
        }
        let current = self.resolve_same_origin(current_locator)?;
        self.normalize_locator(current.as_str())?;
        let resolved = match Url::parse(reference) {
            Ok(url) => url,
            Err(_) => current
                .join(reference)
                .context("invalid article reference")?,
        };
        if !matches!(resolved.scheme(), "http" | "https") {
            if matches!(resolved.scheme(), "mailto" | "tel" | "geo") {
                return Ok(ArticleReference::External(resolved.to_string()));
            }
            bail!(
                "article reference uses unsupported scheme {}",
                resolved.scheme()
            );
        }
        if resolved.origin() != self.base.origin() {
            return Ok(ArticleReference::External(resolved.to_string()));
        }
        let fragment = resolved.fragment().map(ToOwned::to_owned);
        let mut without_fragment = resolved;
        without_fragment.set_fragment(None);
        let locator = self.normalize_locator(without_fragment.as_str())?;
        Ok(ArticleReference::Internal { locator, fragment })
    }

    pub(crate) fn fetch_image(&self, current_locator: &str, source: &str) -> Result<ImageResource> {
        match self.resolve_article_reference(current_locator, source)? {
            ArticleReference::External(url) => Ok(ImageResource::External(url)),
            ArticleReference::Internal { locator, .. } => {
                let url = self.raw_url(&locator)?;
                let (bytes, content_type) = self.get_bytes(&url, "image/*", MAX_IMAGE_BYTES)?;
                if !content_type.starts_with("image/") {
                    bail!("Kiwix image response has unsupported media type {content_type}");
                }
                Ok(ImageResource::Downloaded {
                    bytes,
                    extension: image_extension(&content_type),
                })
            }
        }
    }

    fn normalize_locator(&self, locator: &str) -> Result<String> {
        let url = self.resolve_same_origin(locator)?;
        if url.query().is_some() || url.fragment().is_some() {
            bail!("article locator contains a query or fragment");
        }
        let base_path = self.base.path().trim_end_matches('/');
        let prefix = format!("{base_path}/content/");
        if !url.path().starts_with(&prefix) || url.path().len() <= prefix.len() {
            bail!("article locator is outside the configured Kiwix content path");
        }
        validate_path(url.path())?;
        Ok(url.path().to_owned())
    }

    fn raw_url(&self, locator: &str) -> Result<Url> {
        let normalized = self.normalize_locator(locator)?;
        let base_path = self.base.path().trim_end_matches('/');
        let content_prefix = format!("{base_path}/content/");
        let tail = normalized
            .strip_prefix(&content_prefix)
            .context("article locator lost its validated content prefix")?;
        let (content_id, article) = tail
            .split_once('/')
            .filter(|(content_id, article)| !content_id.is_empty() && !article.is_empty())
            .context("article locator must include a content ID and article path")?;
        let mut url = self.base.clone();
        url.set_path(&format!("{base_path}/raw/{content_id}/content/{article}"));
        Ok(url)
    }

    fn resolve_same_origin(&self, locator: &str) -> Result<Url> {
        let url = match Url::parse(locator) {
            Ok(url) => url,
            Err(_) => self.base.join(locator).context("invalid article locator")?,
        };
        if url.origin() != self.base.origin()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            bail!("article locator must stay on the configured Kiwix server");
        }
        Ok(url)
    }

    fn get_text(&self, url: &Url, accept: &'static str) -> Result<String> {
        let (bytes, _) = self.get_bytes(url, accept, MAX_RESPONSE_BYTES)?;
        String::from_utf8(bytes).context("Kiwix response is not UTF-8")
    }

    fn get_bytes(
        &self,
        url: &Url,
        accept: &'static str,
        limit: usize,
    ) -> Result<(Vec<u8>, String)> {
        let mut request = self
            .http
            .get(url.clone())
            .header(USER_AGENT, concat!("kiwix-cli/", env!("CARGO_PKG_VERSION")))
            .header(ACCEPT, accept);
        request = self.authenticate(request);
        let mut response = request
            .send()
            .with_context(|| format!("request to {} failed", display_url(url)))?;
        let status = response.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            bail!("Kiwix server rejected the configured credentials ({status})");
        }
        if status.is_redirection() {
            bail!("Kiwix server returned a redirect; configure the final service URL ({status})");
        }
        if !status.is_success() {
            bail!("Kiwix server returned {status} for {}", display_url(url));
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > limit)
        {
            bail!("Kiwix response exceeds the configured size limit");
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .context("failed while reading Kiwix response")?;
        if bytes.len() > limit {
            bail!("Kiwix response exceeds the configured size limit");
        }
        Ok((bytes, content_type))
    }

    fn authenticate(&self, request: RequestBuilder) -> RequestBuilder {
        match (&self.username, &self.password) {
            (Some(username), Some(password)) => request.basic_auth(username, Some(password)),
            _ => request,
        }
    }
}

fn validate_path(path: &str) -> Result<()> {
    if path.contains('\\') || path.chars().any(char::is_control) || has_bad_percent_escape(path) {
        bail!("article locator contains unsafe characters");
    }
    for encoded in path.split('/') {
        let mut segment = encoded.to_owned();
        for _ in 0..4 {
            let decoded = percent_decode_str(&segment)
                .decode_utf8()
                .context("article locator contains invalid UTF-8 escaping")?
                .into_owned();
            if decoded == segment {
                break;
            }
            segment = decoded;
        }
        if segment == "."
            || segment == ".."
            || segment.contains(['/', '\\', '?', '#'])
            || segment
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            bail!("article locator contains an unsafe path segment");
        }
    }
    Ok(())
}

fn validate_content_id(content_id: &str) -> Result<()> {
    if content_id.is_empty()
        || !content_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        bail!("content ID contains unsupported characters");
    }
    Ok(())
}

fn has_bad_percent_escape(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        *byte == b'%'
            && (index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit())
    })
}

fn display_url(url: &Url) -> String {
    let mut safe = url.clone();
    safe.set_query(None);
    safe.to_string()
}

fn image_extension(content_type: &str) -> String {
    match content_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        _ => "img",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use httpmock::Method::GET;
    use httpmock::MockServer;

    use super::*;

    fn client() -> KiwixClient {
        KiwixClient::new(
            "https://example.test/kiwix",
            None,
            None,
            Duration::from_secs(5),
        )
        .unwrap()
    }

    #[test]
    fn maps_content_locator_to_raw_endpoint() {
        let url = client().raw_url("/kiwix/content/wiki/A/Rust").unwrap();
        assert_eq!(
            url.as_str(),
            "https://example.test/kiwix/raw/wiki/content/A/Rust"
        );
    }

    #[test]
    fn rejects_cross_origin_and_encoded_traversal() {
        assert!(
            client()
                .raw_url("https://evil.test/kiwix/content/wiki/A")
                .is_err()
        );
        assert!(
            client()
                .raw_url("/kiwix/content/wiki/%252e%252e/secret")
                .is_err()
        );
        assert!(client().raw_url("/kiwix/content/wiki/%GG/secret").is_err());
    }

    #[test]
    fn resolves_wiki_links_and_fragments_against_the_current_article() {
        let client = client();
        assert_eq!(
            client
                .resolve_article_reference("/kiwix/content/wiki/Current", "Next_Page#History")
                .unwrap(),
            ArticleReference::Internal {
                locator: "/kiwix/content/wiki/Next_Page".to_owned(),
                fragment: Some("History".to_owned()),
            }
        );
        assert_eq!(
            client
                .resolve_article_reference(
                    "/kiwix/content/wiki/Current",
                    "https://outside.example/page"
                )
                .unwrap(),
            ArticleReference::External("https://outside.example/page".to_owned())
        );
        assert!(
            client
                .resolve_article_reference("/kiwix/content/wiki/Current", "javascript:alert(1)")
                .is_err()
        );
    }

    #[test]
    fn authenticated_article_image_is_downloaded_from_the_raw_endpoint() {
        let server = MockServer::start();
        let image = server.mock(|when, then| {
            when.method(GET)
                .path("/raw/wiki/content/_assets_/map.jpg")
                .header("authorization", "Basic d2lraTpzZWNyZXQ=");
            then.status(200)
                .header("content-type", "image/jpeg")
                .body([0xff, 0xd8, 0xff, 0xd9]);
        });
        let client = KiwixClient::new(
            &server.base_url(),
            Some("wiki".to_owned()),
            Some("secret".to_owned()),
            Duration::from_secs(2),
        )
        .unwrap();

        let resource = client
            .fetch_image("/content/wiki/Current", "./_assets_/map.jpg")
            .unwrap();

        match resource {
            ImageResource::Downloaded { bytes, extension } => {
                assert_eq!(bytes, [0xff, 0xd8, 0xff, 0xd9]);
                assert_eq!(extension, "jpg");
            }
            ImageResource::External(_) => panic!("expected an authenticated image download"),
        }
        image.assert();
    }
}
