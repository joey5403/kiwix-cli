use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::model::Book;

const SCHEMA_VERSION: u32 = 1;
const MAX_HISTORY_PER_SERVER: usize = 500;
const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedArticle {
    pub server: String,
    pub book_id: String,
    pub content_id: String,
    pub book_title: String,
    pub title: String,
    pub locator: String,
}

impl SavedArticle {
    pub(crate) fn new(server: &str, book: &Book, title: &str, locator: &str) -> Self {
        Self {
            server: server.to_owned(),
            book_id: book.id.clone(),
            content_id: book.content_id.clone(),
            book_title: book.title.clone(),
            title: title.to_owned(),
            locator: locator.to_owned(),
        }
    }

    pub(crate) fn book(&self) -> Book {
        Book {
            id: self.book_id.clone(),
            content_id: self.content_id.clone(),
            title: self.book_title.clone(),
        }
    }

    fn same_article(&self, other: &Self) -> bool {
        self.server == other.server && self.locator == other.locator
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ReadingData {
    version: u32,
    history: Vec<SavedArticle>,
    favorites: Vec<SavedArticle>,
}

impl Default for ReadingData {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            history: Vec::new(),
            favorites: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ReadingStore {
    path: Option<PathBuf>,
    data: ReadingData,
}

impl ReadingStore {
    pub(crate) fn load_default() -> Result<Self> {
        Self::load(default_path()?)
    }

    fn load(path: PathBuf) -> Result<Self> {
        let data = match fs::metadata(&path) {
            Ok(metadata) => {
                if metadata.len() > MAX_STATE_BYTES {
                    bail!("reading data is larger than {MAX_STATE_BYTES} bytes");
                }
                let bytes = fs::read(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let mut data: ReadingData = serde_json::from_slice(&bytes)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                if data.version > SCHEMA_VERSION {
                    bail!(
                        "reading data uses unsupported schema version {}",
                        data.version
                    );
                }
                data.version = SCHEMA_VERSION;
                trim_history(&mut data.history);
                data
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ReadingData::default(),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
            }
        };
        Ok(Self {
            path: Some(path),
            data,
        })
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            path: None,
            data: ReadingData::default(),
        }
    }

    pub(crate) fn history(&self, server: &str) -> impl Iterator<Item = &SavedArticle> {
        self.data
            .history
            .iter()
            .filter(move |article| article.server == server)
    }

    pub(crate) fn favorites(&self, server: &str) -> impl Iterator<Item = &SavedArticle> {
        self.data
            .favorites
            .iter()
            .filter(move |article| article.server == server)
    }

    pub(crate) fn record_visit(&mut self, article: SavedArticle) -> Result<()> {
        self.update(|data| {
            data.history
                .retain(|existing| !existing.same_article(&article));
            data.history.insert(0, article);
            trim_history(&mut data.history);
        })
    }

    pub(crate) fn toggle_favorite(&mut self, article: SavedArticle) -> Result<bool> {
        self.update(|data| {
            if let Some(index) = data
                .favorites
                .iter()
                .position(|existing| existing.same_article(&article))
            {
                data.favorites.remove(index);
                false
            } else {
                data.favorites.insert(0, article);
                true
            }
        })
    }

    pub(crate) fn is_favorite(&self, server: &str, locator: &str) -> bool {
        self.data
            .favorites
            .iter()
            .any(|article| article.server == server && article.locator == locator)
    }

    pub(crate) fn remove_history(&mut self, server: &str, locator: &str) -> Result<()> {
        self.update(|data| {
            data.history
                .retain(|article| article.server != server || article.locator != locator);
        })
    }

    pub(crate) fn remove_favorite(&mut self, server: &str, locator: &str) -> Result<()> {
        self.update(|data| {
            data.favorites
                .retain(|article| article.server != server || article.locator != locator);
        })
    }

    fn update<T>(&mut self, change: impl FnOnce(&mut ReadingData) -> T) -> Result<T> {
        let previous = self.data.clone();
        let outcome = change(&mut self.data);
        if let Err(error) = self.save() {
            self.data = previous;
            return Err(error);
        }
        Ok(outcome)
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path
            .parent()
            .context("reading data path has no parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
            format!("failed to create a temporary file in {}", parent.display())
        })?;
        serde_json::to_writer_pretty(&mut temporary, &self.data)
            .context("failed to serialize reading data")?;
        temporary
            .write_all(b"\n")
            .context("failed to finish reading data")?;
        temporary
            .as_file()
            .sync_all()
            .context("failed to sync reading data")?;
        temporary
            .persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(())
    }
}

fn trim_history(history: &mut Vec<SavedArticle>) {
    let mut counts = HashMap::<String, usize>::new();
    history.retain(|article| {
        let count = counts.entry(article.server.clone()).or_default();
        *count += 1;
        *count <= MAX_HISTORY_PER_SERVER
    });
}

fn default_path() -> Result<PathBuf> {
    if let Some(directory) = env::var_os("KIWIX_CLI_DATA_DIR") {
        if directory.is_empty() {
            bail!("KIWIX_CLI_DATA_DIR must not be empty");
        }
        return Ok(Path::new(&directory).join("reading.json"));
    }
    let directories = ProjectDirs::from("", "", "kiwix-cli")
        .context("could not determine the application data directory")?;
    Ok(directories.data_local_dir().join("reading.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn article(locator: &str) -> SavedArticle {
        SavedArticle::new(
            "https://example.test/",
            &Book {
                id: "book-id".to_owned(),
                content_id: "wiki".to_owned(),
                title: "Wiki".to_owned(),
            },
            "Article",
            locator,
        )
    }

    #[test]
    fn history_is_global_across_books_and_moves_revisited_articles_to_the_front() {
        let mut store = ReadingStore::in_memory();
        let mut first = article("/content/wiki/First");
        let mut second = article("/content/other/Second");
        second.book_id = "other-book".to_owned();
        second.content_id = "other".to_owned();

        store.record_visit(first.clone()).unwrap();
        store.record_visit(second).unwrap();
        first.title = "Updated title".to_owned();
        store.record_visit(first).unwrap();

        let history = store.history("https://example.test/").collect::<Vec<_>>();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].title, "Updated title");
        assert_eq!(history[1].book_id, "other-book");
    }

    #[test]
    fn favorites_are_scoped_by_server_and_survive_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reading.json");
        let mut store = ReadingStore::load(path.clone()).unwrap();
        let saved = article("/content/wiki/Favorite");

        assert!(store.toggle_favorite(saved.clone()).unwrap());
        let store = ReadingStore::load(path).unwrap();

        assert!(store.is_favorite("https://example.test/", &saved.locator));
        assert_eq!(store.favorites("https://other.test/").count(), 0);
    }

    #[test]
    fn history_limit_is_applied_independently_per_server() {
        let mut history = (0..=MAX_HISTORY_PER_SERVER)
            .map(|index| article(&format!("/content/wiki/{index}")))
            .collect::<Vec<_>>();
        let mut other = article("/content/wiki/Other");
        other.server = "https://other.test/".to_owned();
        history.push(other);

        trim_history(&mut history);

        assert_eq!(
            history
                .iter()
                .filter(|article| article.server == "https://example.test/")
                .count(),
            MAX_HISTORY_PER_SERVER
        );
        assert_eq!(
            history
                .iter()
                .filter(|article| article.server == "https://other.test/")
                .count(),
            1
        );
    }
}
