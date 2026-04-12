use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use epub::doc::EpubDoc;
use sha2::{Digest, Sha256};

use crate::util::escape_html;

pub const EPUBS_DIR: &str = "./epubs";
pub const HTML_DIR: &str = "./html";
const POLL_INTERVAL_SECS: u64 = 60;

pub async fn run_extractor() -> anyhow::Result<()> {
    loop {
        if let Err(e) = extract_all() {
            eprintln!("Extractor error: {e}");
        }
        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

fn extract_all() -> anyhow::Result<()> {
    let epubs_root = Path::new(EPUBS_DIR);
    let html_root = Path::new(HTML_DIR);

    if !epubs_root.exists() {
        eprintln!("epubs directory does not exist: {EPUBS_DIR}");
        return Ok(());
    }

    fs::create_dir_all(html_root)?;
    walk_and_extract(epubs_root, epubs_root, html_root)
}

fn walk_and_extract(dir: &Path, epubs_root: &Path, html_root: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_and_extract(&path, epubs_root, html_root)?;
        } else if path.extension().map_or(false, |e| e == "epub") {
            let relative = path.strip_prefix(epubs_root)?;
            let stem = path.file_stem().unwrap();
            let out_dir = html_root
                .join(relative.parent().unwrap_or(Path::new("")))
                .join(stem);

            if let Err(e) = maybe_convert(&path, &out_dir) {
                eprintln!("Failed to process {}: {e}", path.display());
            }
        }
    }
    Ok(())
}

fn maybe_convert(epub_path: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let hash = hash_file(epub_path)?;
    let hash_path = out_dir.join(".hash");

    if hash_path.exists() {
        let stored = fs::read_to_string(&hash_path)?;
        if stored.trim() == hash {
            return Ok(());
        }
    }

    println!("Converting: {}", epub_path.display());
    fs::create_dir_all(out_dir)?;
    clean_output_dir(out_dir);
    convert_epub(epub_path, out_dir)?;
    fs::write(hash_path, &hash)?;
    println!("Done: {}", out_dir.display());

    Ok(())
}

/// Remove old section_*.html, index.html, and resource subdirectories before
/// re-conversion so stale files from a prior version don't linger.
fn clean_output_dir(out_dir: &Path) {
    let Ok(entries) = fs::read_dir(out_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Skip dotfiles (.hash)
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            let _ = fs::remove_dir_all(&path);
        } else if name == "index.html"
            || (name.starts_with("section_") && name.ends_with(".html"))
        {
            let _ = fs::remove_file(&path);
        }
    }
}

fn hash_file(path: &Path) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn convert_epub(epub_path: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let mut doc = EpubDoc::new(epub_path)
        .map_err(|e| anyhow::anyhow!("Failed to open epub: {e}"))?;

    let root_base = doc.root_base.clone();

    // Build path -> title map from TOC (strip fragment identifiers like #anchor)
    let toc_titles: HashMap<PathBuf, String> = doc
        .toc
        .iter()
        .map(|nav| {
            let s = nav.content.to_string_lossy();
            let path_str = s.split('#').next().unwrap_or(&s);
            (PathBuf::from(path_str), nav.label.trim().to_string())
        })
        .collect();

    let mut sections: Vec<(String, String)> = Vec::new();
    let mut toc_hits = 0usize;
    let spine = doc.spine.clone();
    let resources = doc.resources.clone();

    for (i, spine_item) in spine.iter().enumerate() {
        let id = &spine_item.idref;

        let Some(resource) = resources.get(id) else {
            continue;
        };
        let Some((content, mime)) = doc.get_resource_str(id) else {
            continue;
        };
        if !mime.contains("html") {
            continue;
        }

        // Title resolution priority: TOC label > HTML <title> tag > fallback
        let toc_title = toc_titles.get(&resource.path).cloned();
        if toc_title.is_some() {
            toc_hits += 1;
        }
        let title = toc_title
            .or_else(|| extract_title_from_html(&content))
            .unwrap_or_else(|| format!("Chapter {}", i + 1));

        // Rewrite resource paths (img src, link href) so they resolve
        // correctly from the flat section file at the book root.
        let content = rewrite_resource_paths(&content, &resource.path, &root_base);

        let filename = format!("section_{:03}.html", i + 1);
        fs::write(out_dir.join(&filename), &content)?;
        sections.push((title, filename));
    }

    if toc_hits == 0 && !sections.is_empty() {
        eprintln!(
            "Warning: 0 TOC matches for {} — chapter titles are from <title> tags or fallbacks",
            epub_path.display()
        );
    }

    // Extract images, CSS, and fonts
    extract_resources(&mut doc, out_dir, &root_base)?;

    let book_name = out_dir.file_name().unwrap().to_string_lossy();
    fs::write(
        out_dir.join("index.html"),
        generate_index(&book_name, &sections),
    )?;

    Ok(())
}

/// Extract non-HTML resources (images, CSS, fonts) from the EPUB into out_dir,
/// preserving directory structure relative to root_base.
fn extract_resources(
    doc: &mut EpubDoc<std::io::BufReader<std::fs::File>>,
    out_dir: &Path,
    root_base: &Path,
) -> anyhow::Result<()> {
    let resources: Vec<(String, PathBuf, String)> = doc
        .resources
        .iter()
        .map(|(id, r)| (id.clone(), r.path.clone(), r.mime.clone()))
        .collect();

    for (id, res_path, mime) in &resources {
        let dominated_by = mime.starts_with("image/")
            || mime == "text/css"
            || mime.starts_with("font/")
            || mime == "application/vnd.ms-opentype";

        if !dominated_by {
            continue;
        }

        let Some((bytes, _)) = doc.get_resource(id) else {
            continue;
        };

        // Strip root_base prefix to get the output-relative path
        // e.g. OEBPS/images/foo.jpg -> images/foo.jpg
        let rel_path = res_path
            .strip_prefix(root_base)
            .unwrap_or(res_path.as_path());

        let dest = out_dir.join(rel_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, bytes)?;
    }
    Ok(())
}

/// Rewrite src="..." and href="..." attributes in section HTML so that
/// relative resource paths resolve correctly from the flat section file
/// at the book root directory.
///
/// For a section originally at `OEBPS/xhtml/ch01.xhtml` referencing
/// `../images/foo.jpg`, this resolves to `OEBPS/images/foo.jpg`, strips
/// root_base (`OEBPS/`) to get `images/foo.jpg`, which is the correct
/// path relative to the book root where the section file now lives.
fn rewrite_resource_paths(html: &str, section_epub_path: &Path, root_base: &Path) -> String {
    let section_dir = section_epub_path.parent().unwrap_or(Path::new(""));

    let mut result = html.to_string();

    for attr in &["src=\"", "src='", "href=\"", "href='"] {
        let close_quote = if attr.ends_with('"') { '"' } else { '\'' };
        let mut output = String::with_capacity(result.len());
        let mut remainder = result.as_str();

        while let Some(attr_pos) = remainder.find(attr) {
            output.push_str(&remainder[..attr_pos + attr.len()]);
            remainder = &remainder[attr_pos + attr.len()..];

            let Some(end_pos) = remainder.find(close_quote) else {
                break;
            };

            let original_ref = &remainder[..end_pos];

            if original_ref.starts_with("http://")
                || original_ref.starts_with("https://")
                || original_ref.starts_with("data:")
                || original_ref.starts_with('#')
                || original_ref.is_empty()
            {
                output.push_str(original_ref);
            } else {
                // Resolve relative path against the section's original directory
                let resolved = normalize_path(&section_dir.join(original_ref));
                // Strip root_base to get path relative to book root
                let rewritten = resolved.strip_prefix(root_base).unwrap_or(&resolved);
                output.push_str(&rewritten.to_string_lossy());
            }

            remainder = &remainder[end_pos..];
        }
        output.push_str(remainder);
        result = output;
    }

    result
}

/// Normalize a path by resolving `.` and `..` components without touching
/// the filesystem (unlike canonicalize which requires the path to exist).
fn normalize_path(path: &Path) -> PathBuf {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(p) => parts.push(p),
            other => parts.push(other.as_os_str()),
        }
    }
    parts.iter().collect()
}

fn extract_title_from_html(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let start = lower.find("<title>")? + "<title>".len();
    let end = lower[start..].find("</title>").map(|i| start + i)?;
    let title = html[start..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

fn generate_index(book_name: &str, sections: &[(String, String)]) -> String {
    let mut html = format!(
        "<!DOCTYPE html>\n<html>\n<head><meta charset=\"utf-8\"><title>{}</title></head>\n<body>\n<h1>{}</h1>\n<ul>\n",
        escape_html(book_name),
        escape_html(book_name)
    );
    for (title, filename) in sections {
        html.push_str(&format!(
            "  <li><a href=\"{}\">{}</a></li>\n",
            filename,
            escape_html(title)
        ));
    }
    html.push_str("</ul>\n</body>\n</html>\n");
    html
}
