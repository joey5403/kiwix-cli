#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Book {
    pub id: String,
    pub content_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub locator: String,
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage {
    pub total: usize,
    pub start: usize,
    pub page_length: usize,
    pub results: Vec<SearchResult>,
}
