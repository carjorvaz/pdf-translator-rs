//! PDF Translator CLI - Command line tool for translating PDF documents.

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use pdf_translator_core::{AppConfig, Lang, PdfDocument, PdfTranslator, TextColor};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{Level, info};
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

    /// Output PDF file (default: `input-<target>.pdf`)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Source language code
    #[arg(short = 's', long)]
    source: Option<String>,

    /// Target language code
    #[arg(short = 't', long)]
    target: Option<String>,

    /// OpenAI API base URL
    #[arg(long, env = "OPENAI_API_BASE")]
    api_base: Option<String>,

    /// OpenAI API key
    #[arg(long, env = "OPENAI_API_KEY")]
    api_key: Option<String>,

    /// Model name for OpenAI-compatible API
    #[arg(long, env = "OPENAI_MODEL")]
    model: Option<String>,

    /// Translation text color
    #[arg(long, value_enum)]
    color: Option<ColorOption>,

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
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_cache: Option<bool>,
}

fn parse_page_range(pages: &str, total: usize) -> Result<Vec<usize>> {
    if pages.trim().is_empty() {
        bail!("Page range cannot be empty");
    }

    // Validate the entire specification before expanding any range. This makes malformed
    // or enormous endpoints fail without partially iterating an earlier component.
    for component in pages.split(',') {
        let component = component.trim();
        if component.is_empty() {
            bail!("Page range contains an empty component");
        }

        if let Some((start, end)) = component.split_once('-') {
            if start.is_empty() || end.is_empty() || end.contains('-') {
                bail!("Invalid page range: {component}");
            }
            let start = start.parse::<usize>().context("Invalid page range start")?;
            let end = end.parse::<usize>().context("Invalid page range end")?;
            validate_page(start, total)?;
            validate_page(end, total)?;
            if start > end {
                bail!("Page range is reversed: {component}");
            }
        } else {
            let page = component.parse::<usize>().context("Invalid page number")?;
            validate_page(page, total)?;
        }
    }

    let mut result = Vec::new();
    for component in pages.split(',').map(str::trim) {
        if let Some((start, end)) = component.split_once('-') {
            let start = start.parse::<usize>().context("Invalid page range start")?;
            let end = end.parse::<usize>().context("Invalid page range end")?;
            result.extend((start..=end).map(|page| page - 1));
        } else {
            let page = component.parse::<usize>().context("Invalid page number")?;
            result.push(page - 1);
        }
    }

    result.sort_unstable();
    result.dedup();
    Ok(result)
}

fn validate_page(page: usize, total: usize) -> Result<()> {
    if page == 0 {
        bail!("Page numbers start at 1");
    }
    if page > total {
        bail!("Page {page} is out of range for a {total}-page document");
    }
    Ok(())
}

fn output_parent(output: &Path) -> &Path {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn resolved_output_path(input: &Path, output: Option<PathBuf>, target: &Lang) -> PathBuf {
    output.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("output");
        input.with_file_name(format!("{stem}-{target}.pdf"))
    })
}

fn reject_input_alias(input: &Path, output: &Path) -> Result<()> {
    let input_canonical = input
        .canonicalize()
        .with_context(|| format!("Failed to resolve input path: {}", input.display()))?;

    if output.exists() {
        let output_canonical = output
            .canonicalize()
            .with_context(|| format!("Failed to resolve output path: {}", output.display()))?;
        if input_canonical == output_canonical {
            bail!("Output path aliases the input PDF");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let input_metadata = std::fs::metadata(&input_canonical)?;
            let output_metadata = std::fs::metadata(&output_canonical)?;
            if input_metadata.dev() == output_metadata.dev()
                && input_metadata.ino() == output_metadata.ino()
            {
                bail!("Output path aliases the input PDF");
            }
        }
    } else {
        let parent = output_parent(output);
        let file_name = output.file_name().context("Output path must name a file")?;
        let resolved_output = parent
            .canonicalize()
            .with_context(|| format!("Failed to resolve output directory: {}", parent.display()))?
            .join(file_name);
        if input_canonical == resolved_output {
            bail!("Output path aliases the input PDF");
        }
    }

    Ok(())
}

fn create_temp_output(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_atomic(output: &Path, bytes: &[u8]) -> Result<()> {
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    let parent = output_parent(output);
    let file_name = output.file_name().context("Output path must name a file")?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before the Unix epoch")?
        .as_nanos();
    let process_id = std::process::id();

    let (temp_path, mut temp_file) = (0..128)
        .find_map(|attempt| {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let mut temp_name = OsString::from(".");
            temp_name.push(file_name);
            temp_name.push(format!(".tmp-{process_id}-{timestamp}-{id}-{attempt}"));
            let path = parent.join(temp_name);
            match create_temp_output(&path) {
                Ok(file) => Some(Ok((path, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .context("Could not allocate a unique temporary output file")?
        .with_context(|| format!("Failed to create temporary output in {}", parent.display()))?;

    let staged = (|| -> Result<()> {
        temp_file.write_all(bytes)?;
        temp_file.sync_all()?;
        Ok(())
    })();
    drop(temp_file);

    if let Err(error) = staged {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error).context("Failed to stage translated PDF");
    }

    if let Err(error) = std::fs::rename(&temp_path, output) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error)
            .with_context(|| format!("Failed to persist output: {}", output.display()));
    }

    Ok(())
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

    // Apply only values explicitly supplied by the CLI or its declared environment sources.
    if let Some(source) = args.source.as_deref() {
        config.source_lang = Lang::new(source);
    }
    if let Some(target) = args.target.as_deref() {
        config.target_lang = Lang::new(target);
    }
    if let Some(color) = args.color {
        config.text_color = color.into();
    }
    if let Some(api_base) = args.api_base {
        config.translator.api_base = api_base;
    }
    if let Some(api_key) = args.api_key {
        config.translator.api_key = Some(api_key);
    }
    if let Some(model) = args.model {
        config.translator.model = model;
    }
    if args.no_cache == Some(true) {
        config.cache.memory_enabled = false;
        config.cache.disk_enabled = false;
    }

    let output_path = resolved_output_path(&args.input, args.output, &config.target_lang);
    reject_input_alias(&args.input, &output_path)?;
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
    let translator =
        PdfTranslator::new(config.clone()).context("Failed to initialize translator")?;

    // Setup progress bar
    #[allow(clippy::cast_possible_truncation)]
    let pb = ProgressBar::new(pages.len() as u64);
    // Template is hardcoded and valid, unwrap is safe
    #[allow(clippy::unwrap_used)]
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
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

    write_atomic(&output_path, &output_bytes)?;

    // CLI output is intentional
    #[allow(clippy::print_stdout)]
    {
        println!("Translated PDF saved to: {}", output_path.display());
    }

    Ok(())
}
