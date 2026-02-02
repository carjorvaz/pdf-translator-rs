//! PDF Translator CLI - Command line tool for translating PDF documents.

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use pdf_translator_core::{AppConfig, Lang, PdfDocument, PdfTranslator, TextColor};
use std::path::PathBuf;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;


#[derive(Debug, Clone, ValueEnum)]
enum ColorOption {
    DarkRed,
    Black,
    Blue,
    DarkGreen,
    Purple,
}

impl From<ColorOption> for TextColor {
    fn from(opt: ColorOption) -> Self {
        match opt {
            ColorOption::DarkRed => Self::dark_red(),
            ColorOption::Black => Self::black(),
            ColorOption::Blue => Self::blue(),
            ColorOption::DarkGreen => Self::dark_green(),
            ColorOption::Purple => Self::purple(),
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "pdf-translate")]
#[command(author, version, about = "Translate PDF documents", long_about = None)]
struct Args {
    /// Input PDF file
    #[arg(required = true)]
    input: PathBuf,

    /// Output PDF file (default: input-<target>.pdf)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Source language code
    #[arg(short = 's', long, default_value = "fr")]
    source: String,

    /// Target language code
    #[arg(short = 't', long, default_value = "en")]
    target: String,

    /// OpenAI API base URL
    #[arg(long, env = "OPENAI_API_BASE", default_value = "http://localhost:8080/v1")]
    api_base: String,

    /// OpenAI API key
    #[arg(long, env = "OPENAI_API_KEY")]
    api_key: Option<String>,

    /// Model name for OpenAI-compatible API
    #[arg(long, env = "OPENAI_MODEL", default_value = "default_model")]
    model: String,

    /// Translation text color
    #[arg(long, value_enum, default_value = "dark-red")]
    color: ColorOption,

    /// Config file path
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Verbose output
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Translate only specific pages (e.g., "1-5" or "1,3,5")
    #[arg(long)]
    pages: Option<String>,

    /// Disable caching
    #[arg(long)]
    no_cache: bool,
}

fn parse_page_range(pages: &str, total: usize) -> Result<Vec<usize>> {
    let mut result = Vec::new();

    for part in pages.split(',') {
        let part = part.trim();
        if part.contains('-') {
            let range: Vec<&str> = part.split('-').collect();
            if range.len() == 2 {
                let start: usize = range[0].parse().context("Invalid page range start")?;
                let end: usize = range[1].parse().context("Invalid page range end")?;
                for p in start..=end {
                    if p > 0 && p <= total {
                        result.push(p - 1); // Convert to 0-indexed
                    }
                }
            }
        } else {
            let page: usize = part.parse().context("Invalid page number")?;
            if page > 0 && page <= total {
                result.push(page - 1); // Convert to 0-indexed
            }
        }
    }

    result.sort_unstable();
    result.dedup();
    Ok(result)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present (before parsing args so env vars are available)
    dotenvy::dotenv().ok();

    let args = Args::parse();

    // Setup logging
    let log_level = match args.verbose {
        0 => Level::WARN,
        1 => Level::INFO,
        2 => Level::DEBUG,
        _ => Level::TRACE,
    };

    FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_target(false)
        .init();

    // Load or create config
    let mut config = if let Some(config_path) = &args.config {
        AppConfig::from_file(config_path).context("Failed to load config file")?
    } else {
        AppConfig::load()
    };

    // Override config with CLI arguments
    config.source_lang = Lang::new(&args.source);
    config.target_lang = Lang::new(&args.target);
    config.text_color = args.color.into();

    if args.no_cache {
        config.cache.memory_enabled = false;
        config.cache.disk_enabled = false;
    }

    // Configure translator
    config.translator =
        pdf_translator_core::TranslatorConfig::new(args.api_base, args.api_key, args.model);

    // Load input PDF
    info!("Loading PDF: {}", args.input.display());
    let doc = PdfDocument::from_file(&args.input)
        .context(format!("Failed to load PDF: {}", args.input.display()))?;

    let total_pages = doc.page_count();
    info!("Document has {} pages", total_pages);

    // Determine which pages to translate
    let pages = if let Some(ref page_spec) = args.pages {
        parse_page_range(page_spec, total_pages)?
    } else {
        (0..total_pages).collect()
    };

    if pages.is_empty() {
        anyhow::bail!("No valid pages to translate");
    }

    info!("Translating {} pages", pages.len());

    // Create translator
    let translator = PdfTranslator::new(config.clone())
        .context("Failed to initialize translator")?;

    // Setup progress bar
    #[allow(clippy::cast_possible_truncation)]
    let pb = ProgressBar::new(pages.len() as u64);
    // Template is hardcoded and valid, unwrap is safe
    #[allow(clippy::unwrap_used)]
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("#>-"),
    );

    // Translate pages
    let mut translated_pages = Vec::with_capacity(pages.len());

    for &page_num in &pages {
        pb.set_message(format!("Page {}", page_num + 1));

        let result = translator
            .translate_page(&doc, page_num)
            .await
            .context(format!("Failed to translate page {}", page_num + 1))?;

        if result.from_cache {
            pb.println(format!("Page {} (cached)", page_num + 1));
        }

        translated_pages.push(result.pdf_bytes);
        pb.inc(1);
    }

    pb.finish_with_message("Translation complete");

    // Combine pages
    let output_bytes = pdf_translator_core::pdf::overlay::combine_pdfs(&translated_pages)
        .context("Failed to combine translated pages")?;

    // Determine output path
    let output_path = args.output.unwrap_or_else(|| {
        let stem = args
            .input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        args.input.with_file_name(format!("{}-{}.pdf", stem, args.target))
    });

    // Save output
    std::fs::write(&output_path, output_bytes)
        .context(format!("Failed to write output: {}", output_path.display()))?;

    // CLI output is intentional
    #[allow(clippy::print_stdout)]
    {
        println!("Translated PDF saved to: {}", output_path.display());
    }

    Ok(())
}
