use std::env;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::terminal;
use kiwix_cli::client::KiwixClient;
use kiwix_cli::tui::run_tui;

const DEFAULT_SERVER: &str = "https://browse.library.kiwix.org/";

#[derive(Debug, Parser)]
#[command(version, about = "Browse a self-hosted Kiwix server from the terminal")]
struct Cli {
    /// Kiwix server URL (or `KIWIX_URL`); defaults to the public Browse service
    #[arg(long, env = "KIWIX_URL", global = true)]
    server: Option<String>,

    /// Basic Auth username (or `KIWIX_USERNAME`)
    #[arg(long, env = "KIWIX_USERNAME", global = true)]
    username: Option<String>,

    /// Request timeout in seconds
    #[arg(long, default_value_t = 30, global = true, value_parser = clap::value_parser!(u64).range(1..=300))]
    timeout: u64,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the document libraries on the server
    #[command(alias = "libraries")]
    Books,

    /// Search one document library
    Search {
        /// Catalog UUID shown by `books`
        #[arg(long)]
        book: String,

        /// Search expression
        query: String,

        /// Zero-based result offset
        #[arg(long, default_value_t = 0)]
        start: usize,

        /// Number of results (1-50)
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },

    /// Read a `/content/...` locator printed by `search`
    Read {
        locator: String,

        /// Render width; defaults to the terminal width
        #[arg(long)]
        width: Option<usize>,
    },

    /// Open the home page of one content library
    Home {
        /// Content library name shown by `books`
        #[arg(long)]
        content: String,

        /// Render width; defaults to the terminal width
        #[arg(long)]
        width: Option<usize>,
    },

    /// Open a random article from one content library
    Random {
        /// Content library name shown by `books`
        #[arg(long)]
        content: String,

        /// Render width; defaults to the terminal width
        #[arg(long)]
        width: Option<usize>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let server = configured_server(cli.server);
    let password = env::var("KIWIX_PASSWORD").ok();
    let client = KiwixClient::new(
        &server,
        cli.username,
        password,
        Duration::from_secs(cli.timeout),
    )?;

    match cli.command {
        None => run_tui(client)?,
        Some(Command::Books) => {
            let books = client.list_books()?;
            println!("UUID\tCONTENT\tTITLE");
            for book in books {
                println!(
                    "{}\t{}\t{}",
                    book.id,
                    book.content_id,
                    one_line(&book.title)
                );
            }
        }
        Some(Command::Search {
            book,
            query,
            start,
            limit,
        }) => {
            let page = client.search(&book, &query, start, limit)?;
            println!(
                "Results {}-{} of {}",
                page.start + usize::from(!page.results.is_empty()),
                page.start + page.results.len(),
                page.total
            );
            for (offset, result) in page.results.iter().enumerate() {
                println!("\n{}. {}", page.start + offset + 1, one_line(&result.title));
                println!("   {}", result.locator);
                if let Some(excerpt) = &result.excerpt {
                    println!("   {}", one_line(excerpt));
                }
            }
        }
        Some(Command::Read { locator, width }) => {
            let html = client.read_article(&locator)?;
            print_article(&html, width, "could not render article")?;
        }
        Some(Command::Home { content, width }) => {
            let locator = client.home_locator(&content)?;
            let html = client.read_article(&locator)?;
            print_article(&html, width, "could not render library home")?;
        }
        Some(Command::Random { content, width }) => {
            let locator = client.random_locator(&content)?;
            let html = client.read_article(&locator)?;
            print_article(&html, width, "could not render random article")?;
        }
    }
    Ok(())
}

fn default_width() -> usize {
    terminal::size().map_or(80, |(width, _)| usize::from(width))
}

fn print_article(html: &str, width: Option<usize>, error_context: &'static str) -> Result<()> {
    let width = width.unwrap_or_else(default_width).clamp(20, 240);
    let rendered = html2text::from_read(html.as_bytes(), width).context(error_context)?;
    print!("{rendered}");
    if !rendered.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn configured_server(server: Option<String>) -> String {
    server.unwrap_or_else(|| DEFAULT_SERVER.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_SERVER, configured_server};

    #[test]
    fn uses_public_browse_server_when_no_override_is_set() {
        assert_eq!(configured_server(None), DEFAULT_SERVER);
    }

    #[test]
    fn preserves_an_explicit_server_override() {
        assert_eq!(
            configured_server(Some("https://local.test/".to_owned())),
            "https://local.test/"
        );
    }
}
