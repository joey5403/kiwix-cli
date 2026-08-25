use anyhow::{Context, Result, bail};
use quick_xml::escape::unescape;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use uuid::Uuid;

use crate::model::{Book, SearchPage, SearchResult};

const MAX_DEPTH: usize = 128;
const MAX_BOOKS: usize = 10_000;
const MAX_RESULTS: usize = 50;

#[derive(Default)]
struct BookDraft {
    id: String,
    title: String,
    hrefs: Vec<String>,
}

#[derive(Default)]
struct SearchDraft {
    title: String,
    link: String,
    description: String,
}

pub fn parse_catalog(xml: &str) -> Result<Vec<Book>> {
    let mut reader = Reader::from_str(xml);
    let mut stack = Vec::<String>::new();
    let mut entry_depth = None;
    let mut current = None::<BookDraft>;
    let mut books = Vec::new();
    let mut root_seen = false;

    loop {
        match reader.read_event().context("invalid OPDS XML")? {
            Event::Start(event) => {
                let name = local_name(&event);
                if stack.is_empty() {
                    if name != "feed" || root_seen {
                        bail!("OPDS response is not an Atom feed");
                    }
                    root_seen = true;
                }
                stack.push(name.clone());
                if stack.len() > MAX_DEPTH {
                    bail!("OPDS document is nested too deeply");
                }
                if name == "entry" && current.is_none() {
                    entry_depth = Some(stack.len());
                    current = Some(BookDraft::default());
                } else if name == "link"
                    && direct_entry_child(&stack, entry_depth)
                    && let Some(href) = attribute(&event, "href")?
                {
                    current
                        .as_mut()
                        .context("OPDS link appeared outside an entry")?
                        .hrefs
                        .push(href);
                }
            }
            Event::Empty(event) => {
                let name = local_name(&event);
                if name == "link"
                    && entry_depth == Some(stack.len())
                    && let Some(href) = attribute(&event, "href")?
                {
                    current
                        .as_mut()
                        .context("OPDS link appeared outside an entry")?
                        .hrefs
                        .push(href);
                }
            }
            Event::Text(text) => {
                if direct_entry_child(&stack, entry_depth) {
                    let value = decode_text(text.xml10_content().as_ref())?;
                    let draft = current
                        .as_mut()
                        .context("OPDS text appeared outside an entry")?;
                    match stack.last().map(String::as_str) {
                        Some("id") => draft.id.push_str(&value),
                        Some("title") => draft.title.push_str(&value),
                        _ => {}
                    }
                }
            }
            Event::CData(text) => {
                if direct_entry_child(&stack, entry_depth) {
                    let value = text.xml10_content();
                    let draft = current
                        .as_mut()
                        .context("OPDS CDATA appeared outside an entry")?;
                    match stack.last().map(String::as_str) {
                        Some("id") => draft.id.push_str(&value),
                        Some("title") => draft.title.push_str(&value),
                        _ => {}
                    }
                }
            }
            Event::GeneralRef(reference) => {
                if direct_entry_child(&stack, entry_depth) {
                    let value = decode_reference(&reference)?;
                    let draft = current
                        .as_mut()
                        .context("OPDS entity appeared outside an entry")?;
                    match stack.last().map(String::as_str) {
                        Some("id") => draft.id.push(value),
                        Some("title") => draft.title.push(value),
                        _ => {}
                    }
                } else {
                    decode_reference(&reference)?;
                }
            }
            Event::End(event) => {
                let name = event.local_name().as_ref().to_owned();
                if stack.last() != Some(&name) {
                    bail!("invalid OPDS element nesting");
                }
                if name == "entry" && entry_depth == Some(stack.len()) {
                    let draft = current.take().context("OPDS entry state was lost")?;
                    if let Some(book) = finish_book(&draft) {
                        books.push(book);
                        if books.len() > MAX_BOOKS {
                            bail!("OPDS catalog contains too many books");
                        }
                    }
                    entry_depth = None;
                }
                stack.pop();
            }
            Event::DocType(_) => bail!("DTD is not allowed in OPDS XML"),
            Event::Eof => break,
            _ => {}
        }
    }

    if !root_seen || !stack.is_empty() {
        bail!("incomplete OPDS response");
    }
    Ok(books)
}

pub fn parse_search(xml: &str) -> Result<SearchPage> {
    let mut reader = Reader::from_str(xml);
    let mut stack = Vec::<String>::new();
    let mut item_depth = None;
    let mut current = None::<SearchDraft>;
    let mut total = String::new();
    let mut start = String::new();
    let mut page_length = String::new();
    let mut results = Vec::new();
    let mut root_seen = false;

    loop {
        match reader.read_event().context("invalid search XML")? {
            Event::Start(event) => {
                let name = local_name(&event);
                if stack.is_empty() {
                    if name != "rss" || root_seen {
                        bail!("search response is not RSS");
                    }
                    root_seen = true;
                }
                stack.push(name.clone());
                if stack.len() > MAX_DEPTH {
                    bail!("search document is nested too deeply");
                }
                if name == "item" {
                    if current.is_some() {
                        bail!("nested search result");
                    }
                    item_depth = Some(stack.len());
                    current = Some(SearchDraft::default());
                }
            }
            Event::Text(text) => {
                let value = decode_text(text.xml10_content().as_ref())?;
                append_search_text(
                    &stack,
                    item_depth,
                    current.as_mut(),
                    &mut total,
                    &mut start,
                    &mut page_length,
                    &value,
                );
            }
            Event::CData(text) => append_search_text(
                &stack,
                item_depth,
                current.as_mut(),
                &mut total,
                &mut start,
                &mut page_length,
                text.xml10_content().as_ref(),
            ),
            Event::GeneralRef(reference) => {
                let value = decode_reference(&reference)?.to_string();
                append_search_text(
                    &stack,
                    item_depth,
                    current.as_mut(),
                    &mut total,
                    &mut start,
                    &mut page_length,
                    &value,
                );
            }
            Event::End(event) => {
                let name = event.local_name().as_ref().to_owned();
                if stack.last() != Some(&name) {
                    bail!("invalid search XML nesting");
                }
                if name == "item" && item_depth == Some(stack.len()) {
                    let draft = current.take().context("search item state was lost")?;
                    if draft.title.trim().is_empty() || draft.link.trim().is_empty() {
                        bail!("search result is missing a title or link");
                    }
                    if results.len() >= MAX_RESULTS {
                        bail!("search response contains too many results");
                    }
                    let excerpt = clean_excerpt(&draft.description)?;
                    results.push(SearchResult {
                        title: draft.title.trim().to_owned(),
                        locator: draft.link.trim().to_owned(),
                        excerpt,
                    });
                    item_depth = None;
                }
                stack.pop();
            }
            Event::DocType(_) => bail!("DTD is not allowed in search XML"),
            Event::Eof => break,
            _ => {}
        }
    }

    if !root_seen || !stack.is_empty() {
        bail!("incomplete search response");
    }
    let total = parse_number("totalResults", &total)?;
    let start = parse_number("startIndex", &start)?;
    let page_length = parse_number("itemsPerPage", &page_length)?;
    if page_length == 0 || page_length > MAX_RESULTS || results.len() > page_length {
        bail!("invalid search page length");
    }
    if start > total || start.saturating_add(results.len()) > total {
        bail!("search result range exceeds totalResults");
    }
    Ok(SearchPage {
        total,
        start,
        page_length,
        results,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_search_text(
    stack: &[String],
    item_depth: Option<usize>,
    current: Option<&mut SearchDraft>,
    total: &mut String,
    start: &mut String,
    page_length: &mut String,
    value: &str,
) {
    if item_depth.is_some_and(|depth| stack.len() == depth + 1) {
        if let Some(draft) = current {
            match stack.last().map(String::as_str) {
                Some("title") => draft.title.push_str(value),
                Some("link") => draft.link.push_str(value),
                Some("description") => draft.description.push_str(value),
                _ => {}
            }
        }
    } else if stack.len() >= 2 && stack[stack.len() - 2] == "channel" {
        match stack.last().map(String::as_str) {
            Some("totalResults") => total.push_str(value),
            Some("startIndex") => start.push_str(value),
            Some("itemsPerPage") => page_length.push_str(value),
            _ => {}
        }
    }
}

fn finish_book(draft: &BookDraft) -> Option<Book> {
    let raw_id = draft
        .id
        .trim()
        .strip_prefix("urn:uuid:")
        .unwrap_or(draft.id.trim());
    let id = Uuid::parse_str(raw_id).ok()?.to_string();
    let title = draft.title.trim();
    let content_id = draft.hrefs.iter().find_map(|href| content_id(href))?;
    (!title.is_empty()).then(|| Book {
        id,
        content_id,
        title: title.to_owned(),
    })
}

fn content_id(href: &str) -> Option<String> {
    let path = url::Url::parse(href).map_or_else(
        |_| href.split(['?', '#']).next().unwrap_or_default().to_owned(),
        |url| url.path().to_owned(),
    );
    let (_, tail) = path.split_once("/content/")?;
    let value = tail.trim_end_matches('/').split('/').next()?;
    (!value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
    .then(|| value.to_owned())
}

fn direct_entry_child(stack: &[String], entry_depth: Option<usize>) -> bool {
    entry_depth.is_some_and(|depth| stack.len() == depth + 1)
}

fn local_name(event: &BytesStart<'_>) -> String {
    event.local_name().as_ref().to_owned()
}

fn attribute(event: &BytesStart<'_>, name: &str) -> Result<Option<String>> {
    for attribute in event.attributes() {
        let attribute = attribute.context("invalid OPDS attribute")?;
        if attribute.key.local_name().as_ref() == name {
            return Ok(Some(
                attribute
                    .normalized_value(XmlVersion::Implicit1_0)
                    .context("invalid OPDS attribute value")?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}

fn decode_text(value: &str) -> Result<String> {
    Ok(unescape(value).context("invalid XML entity")?.into_owned())
}

fn decode_reference(reference: &quick_xml::events::BytesRef<'_>) -> Result<char> {
    let value = match reference.as_ref() {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "apos" => '\'',
        "quot" => '"',
        _ => reference
            .resolve_char_ref()
            .context("invalid XML character reference")?
            .context("custom XML entities are not allowed")?,
    };
    if value.is_control() && !matches!(value, '\t' | '\n' | '\r') {
        bail!("invalid control character in XML reference");
    }
    Ok(value)
}

fn parse_number(name: &str, value: &str) -> Result<usize> {
    let value = value.trim();
    if value.is_empty() {
        bail!("search response has invalid {name}");
    }
    let normalized = if value.contains(',') {
        let mut groups = value.split(',');
        let first = groups.next().unwrap_or_default();
        if first.is_empty() || first.len() > 3 || !first.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("search response has invalid {name}");
        }
        let mut normalized = first.to_owned();
        for group in groups {
            if group.len() != 3 || !group.bytes().all(|byte| byte.is_ascii_digit()) {
                bail!("search response has invalid {name}");
            }
            normalized.push_str(group);
        }
        normalized
    } else {
        if !value.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("search response has invalid {name}");
        }
        value.to_owned()
    };
    normalized
        .parse()
        .with_context(|| format!("search {name} is too large"))
}

fn clean_excerpt(value: &str) -> Result<Option<String>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    let text =
        html2text::from_read(value.as_bytes(), 120).context("invalid HTML in search excerpt")?;
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok((!text.is_empty()).then_some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_atom_catalog_and_skips_bad_entries() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom">
          <entry><id>urn:uuid:12345678-1234-5678-1234-567812345678</id>
            <title>Wikipedia 中文</title><link href="/content/wikipedia_zh_all" /></entry>
          <entry><id>bad</id><title>Bad</title><link href="/content/bad" /></entry>
        </feed>"#;
        let books = parse_catalog(xml).unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].content_id, "wikipedia_zh_all");
    }

    #[test]
    fn parses_search_rss_and_projects_excerpt() {
        let xml = r#"<rss xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/"><channel>
          <opensearch:totalResults>258,827</opensearch:totalResults><opensearch:startIndex>0</opensearch:startIndex>
          <opensearch:itemsPerPage>20</opensearch:itemsPerPage>
          <item><title>Rust</title><link>/content/zim/A/Rust</link>
          <description>Fast &lt;b&gt;systems&lt;/b&gt; language</description></item>
        </channel></rss>"#;
        let page = parse_search(xml).unwrap();
        assert_eq!(page.total, 258_827);
        assert_eq!(
            page.results[0].excerpt.as_deref(),
            Some("Fast **systems** language")
        );
    }

    #[test]
    fn rejects_doctype() {
        let error = parse_catalog("<!DOCTYPE feed><feed/>").unwrap_err();
        assert!(error.to_string().contains("DTD"));
    }

    #[test]
    fn rejects_malformed_grouped_search_numbers() {
        for value in ["1,2", "12,34", "1,,000", ",100", "1000,000"] {
            assert!(parse_number("totalResults", value).is_err(), "{value}");
        }
    }
}
