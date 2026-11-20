use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use mdictlib::MdxFile;

const MAX_DEFINITIONS: usize = 5;
const MAX_DEFINITION_CHARS: usize = 12_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryDefinition {
    pub headword: String,
    pub dictionary: String,
    pub text: String,
}

pub trait DictionaryProvider: Send + Sync {
    fn lookup(&self, query: &str) -> Result<Vec<DictionaryDefinition>>;
}

#[derive(Clone)]
pub struct DictionaryService {
    providers: Vec<Arc<dyn DictionaryProvider>>,
}

impl DictionaryService {
    pub fn mdx(
        directory: PathBuf,
        source_language: Option<String>,
        target_language: String,
    ) -> Result<Self> {
        let provider = MdxDirectoryProvider::new(directory, source_language, target_language)?;
        Ok(Self::with_providers(vec![Arc::new(provider)]))
    }

    pub fn with_providers(providers: Vec<Arc<dyn DictionaryProvider>>) -> Self {
        Self { providers }
    }

    pub fn lookup(&self, query: &str) -> Result<Vec<DictionaryDefinition>> {
        let mut last_error = None;
        for provider in &self.providers {
            match provider.lookup(query) {
                Ok(entries) if !entries.is_empty() => return Ok(entries),
                Ok(_) => {}
                Err(error) => last_error = Some(error),
            }
        }
        if let Some(error) = last_error {
            return Err(error);
        }
        Ok(Vec::new())
    }
}

type ParsedDictionary = MdxFile;

struct MdxDirectoryProvider {
    directory: PathBuf,
    files: Vec<PathBuf>,
    source_language: Option<String>,
    target_language: String,
    cached: Mutex<Option<(PathBuf, ParsedDictionary)>>,
}

impl MdxDirectoryProvider {
    fn new(
        directory: PathBuf,
        source_language: Option<String>,
        target_language: String,
    ) -> Result<Self> {
        let source_language = source_language
            .map(|language| normalize_language(&language))
            .transpose()?;
        let target_language = normalize_language(&target_language)?;
        let mut files = Vec::new();
        match fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.with_context(|| {
                        format!(
                            "failed to read dictionary directory {}",
                            directory.display()
                        )
                    })?;
                    let path = entry.path();
                    if path.is_file()
                        && path
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("mdx"))
                    {
                        files.push(path);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", directory.display()));
            }
        }
        files.sort();
        Ok(Self {
            directory,
            files,
            source_language,
            target_language,
            cached: Mutex::new(None),
        })
    }

    fn select_file(&self, query: &str) -> Result<PathBuf> {
        if self.files.is_empty() {
            bail!(
                "No .mdx dictionaries found in {}; add a licensed dictionary file there",
                self.directory.display()
            );
        }
        let source = self
            .source_language
            .as_deref()
            .unwrap_or_else(|| detect_language(query));
        let target = if source == "en" {
            "en"
        } else {
            &self.target_language
        };
        if let Some(path) = self.files.iter().find(|path| {
            language_pair(path).is_some_and(|pair| pair.0 == source && pair.1 == target)
        }) {
            return Ok(path.clone());
        }
        if self.files.len() == 1 {
            return Ok(self.files[0].clone());
        }
        bail!(
            "No local {source}-{target} dictionary in {}; name files like {source}-{target}.mdx",
            self.directory.display()
        );
    }

    fn dictionary(&self, path: &Path, query: &str) -> Result<Vec<DictionaryDefinition>> {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cached
            .as_ref()
            .is_none_or(|(cached_path, _)| cached_path != path)
        {
            let dictionary = MdxFile::open(path)
                .with_context(|| format!("failed to parse dictionary {}", path.display()))?;
            *cached = Some((path.to_owned(), dictionary));
        }
        let (_, dictionary) = cached.as_ref().context("dictionary cache is empty")?;
        let title = dictionary.header().title().unwrap_or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("MDX dictionary")
        });
        let Some(matches) = dictionary
            .locate(query)
            .context("failed to look up dictionary entry")?
        else {
            return Ok(Vec::new());
        };
        let mut definitions = Vec::new();
        for ordinal in matches.iter().take(MAX_DEFINITIONS) {
            let Some(entry) = dictionary
                .entry_at(ordinal)
                .context("failed to read dictionary definition")?
            else {
                continue;
            };
            let text = html2text::from_read(entry.text().as_bytes(), 72)
                .context("failed to render dictionary entry")?;
            let text = text.chars().take(MAX_DEFINITION_CHARS).collect();
            definitions.push(DictionaryDefinition {
                headword: entry.key().to_owned(),
                dictionary: title.to_owned(),
                text,
            });
        }
        Ok(definitions)
    }
}

impl DictionaryProvider for MdxDirectoryProvider {
    fn lookup(&self, query: &str) -> Result<Vec<DictionaryDefinition>> {
        let path = self.select_file(query)?;
        self.dictionary(&path, query)
    }
}

pub fn default_dictionary_dir(override_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = override_path {
        return Ok(path);
    }
    let directories = BaseDirs::new().context("could not determine the user's home directory")?;
    Ok(directories
        .home_dir()
        .join(".kiwix-cli")
        .join("dictionaries"))
}

fn normalize_language(language: &str) -> Result<String> {
    let language = language.trim().to_ascii_lowercase();
    if !(2..=3).contains(&language.len()) || !language.bytes().all(|byte| byte.is_ascii_lowercase())
    {
        bail!("language codes must contain two or three ASCII letters");
    }
    Ok(language)
}

fn language_pair(path: &Path) -> Option<(String, String)> {
    let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
    let parts = stem.split(|character: char| !character.is_ascii_alphanumeric());
    for pair in parts.collect::<Vec<_>>().windows(2) {
        if (2..=3).contains(&pair[0].len())
            && (2..=3).contains(&pair[1].len())
            && pair
                .iter()
                .all(|language| language.bytes().all(|byte| byte.is_ascii_lowercase()))
        {
            return Some((pair[0].to_owned(), pair[1].to_owned()));
        }
    }
    None
}

fn detect_language(query: &str) -> &'static str {
    for character in query.chars() {
        let code = u32::from(character);
        if (0x3040..=0x30ff).contains(&code) {
            return "ja";
        }
        if (0xac00..=0xd7af).contains(&code) {
            return "ko";
        }
        if (0x3400..=0x9fff).contains(&code)
            || (0x20000..=0x2fa1f).contains(&code)
            || (0x30000..=0x323af).contains(&code)
        {
            return "zh";
        }
        if (0x0600..=0x06ff).contains(&code) {
            return "ar";
        }
        if (0x0400..=0x052f).contains(&code) {
            return "ru";
        }
    }
    "en"
}

#[cfg(test)]
mod tests {
    use std::fs;

    use directories::BaseDirs;
    use mdictlib::{MdxBuilder, WriteEncoding, WriteOptions};

    use super::{
        DictionaryProvider, MdxDirectoryProvider, default_dictionary_dir, detect_language,
        normalize_language,
    };

    #[test]
    fn default_dictionary_directory_is_in_the_user_home() {
        let home = BaseDirs::new().unwrap().home_dir().to_path_buf();
        assert_eq!(
            default_dictionary_dir(None).unwrap(),
            home.join(".kiwix-cli").join("dictionaries")
        );
    }

    #[test]
    fn detects_common_writing_systems_for_dictionary_selection() {
        assert_eq!(detect_language("apple"), "en");
        assert_eq!(detect_language("苹果"), "zh");
        assert_eq!(detect_language("かな"), "ja");
        assert_eq!(detect_language("한글"), "ko");
        assert_eq!(detect_language("слово"), "ru");
    }

    #[test]
    fn validates_language_codes() {
        assert_eq!(normalize_language("EN").unwrap(), "en");
        assert!(normalize_language("english").is_err());
    }

    #[test]
    fn picks_english_english_for_latin_queries_and_source_pairs_for_other_scripts() {
        let directory = tempfile::tempdir().unwrap();
        let english = directory.path().join("en-en.mdx");
        let chinese = directory.path().join("zh-en.mdx");
        fs::write(&english, b"").unwrap();
        fs::write(&chinese, b"").unwrap();
        let provider =
            MdxDirectoryProvider::new(directory.path().to_path_buf(), None, "en".to_owned())
                .unwrap();

        assert_eq!(provider.select_file("apple").unwrap(), english);
        assert_eq!(provider.select_file("苹果").unwrap(), chinese);
    }

    #[test]
    fn reads_and_renders_a_local_mdx_entry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("en-en.mdx");
        let mut builder =
            MdxBuilder::with_options(WriteOptions::new().with_encoding(WriteEncoding::Utf8));
        builder
            .header_attribute("Title", "Test English Dictionary")
            .unwrap();
        builder.add_entry("apple", "<p>A fruit.</p>").unwrap();
        let mut bytes = Vec::new();
        builder.finish(&mut bytes).unwrap();
        fs::write(&path, bytes).unwrap();

        let provider =
            MdxDirectoryProvider::new(directory.path().to_path_buf(), None, "en".to_owned())
                .unwrap();
        let result = provider.lookup("apple").unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].headword, "apple");
        assert_eq!(result[0].dictionary, "Test English Dictionary");
        assert!(result[0].text.contains("A fruit."));
    }
}
