use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

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

pub(crate) struct ReadingStore {
    path: Option<PathBuf>,
    data: ReadingData,
    persistence: Option<PersistenceWorker>,
}

struct PersistenceWorker {
    pending: Arc<(Mutex<PendingSnapshot>, Condvar)>,
    errors: Receiver<String>,
    thread: thread::JoinHandle<()>,
}

#[derive(Default)]
struct PendingSnapshot {
    data: Option<ReadingData>,
    shutdown: bool,
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
            persistence: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            path: None,
            data: ReadingData::default(),
            persistence: None,
        }
    }

    pub(crate) fn enable_async_persistence(&mut self) -> Result<()> {
        if self.persistence.is_some() {
            return Ok(());
        }
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        let pending = Arc::new((Mutex::new(PendingSnapshot::default()), Condvar::new()));
        let worker_pending = Arc::clone(&pending);
        let (error_tx, errors) = mpsc::channel::<String>();
        let writer = thread::Builder::new()
            .name("kiwix-storage".to_owned())
            .spawn(move || {
                loop {
                    let data = {
                        let (state, wake) = &*worker_pending;
                        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                        while state.data.is_none() && !state.shutdown {
                            state = wake.wait(state).unwrap_or_else(|error| error.into_inner());
                        }
                        match state.data.take() {
                            Some(data) => Some(data),
                            None if state.shutdown => None,
                            None => continue,
                        }
                    };
                    let Some(data) = data else {
                        break;
                    };
                    let store = ReadingStore {
                        path: Some(path.clone()),
                        data,
                        persistence: None,
                    };
                    if let Err(error) = store.save() {
                        let _ = error_tx.send(format_error(error));
                    }
                }
            })
            .context("failed to start reading data writer")?;
        self.persistence = Some(PersistenceWorker {
            pending,
            errors,
            thread: writer,
        });
        Ok(())
    }

    pub(crate) fn take_persistence_errors(&self) -> impl Iterator<Item = String> + '_ {
        self.persistence
            .iter()
            .flat_map(|worker| worker.errors.try_iter())
    }

    pub(crate) fn finish_persistence(&mut self) -> Result<()> {
        let Some(worker) = self.persistence.take() else {
            return Ok(());
        };
        let (state, wake) = &*worker.pending;
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        state.shutdown = true;
        wake.notify_one();
        drop(state);
        worker
            .thread
            .join()
            .map_err(|_| anyhow::anyhow!("reading data writer panicked"))?;
        let errors = worker.errors.try_iter().collect::<Vec<_>>();
        if !errors.is_empty() {
            bail!("failed to persist reading data: {}", errors.join("; "));
        }
        Ok(())
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
        let result = if let Some(worker) = &self.persistence {
            if worker.thread.is_finished() {
                Err(anyhow::anyhow!("reading data writer stopped"))
            } else {
                let (state, wake) = &*worker.pending;
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                state.data = Some(self.data.clone());
                wake.notify_one();
                Ok(())
            }
        } else {
            self.save()
        };
        if let Err(error) = result {
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

impl Drop for ReadingStore {
    fn drop(&mut self) {
        let _ = self.finish_persistence();
    }
}

fn format_error(error: anyhow::Error) -> String {
    format!("{error:#}")
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
    fn async_persistence_flushes_the_latest_snapshot_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reading.json");
        {
            let mut store = ReadingStore::load(path.clone()).unwrap();
            store.enable_async_persistence().unwrap();
            store.record_visit(article("/content/wiki/First")).unwrap();
            store.record_visit(article("/content/wiki/Latest")).unwrap();
        }

        let reloaded = ReadingStore::load(path).unwrap();
        let history = reloaded
            .history("https://example.test/")
            .collect::<Vec<_>>();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].locator, "/content/wiki/Latest");
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
