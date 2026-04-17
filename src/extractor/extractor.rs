use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use epub::doc::EpubDoc;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::AsyncReadExt;
use walkdir::WalkDir;

use crate::extractor::read_position::{ReadPosition, ReadPositionFileData};
use crate::extractor::util::DirHelper;

pub const EPUBS_DIR: &str = "./epubs";
pub const HTML_DIR: &str = "./html";
pub const DATA_DIR: &str = "./data";
const POLL_INTERVAL_SECS: u64 = 60;

pub async fn run_extractor() -> anyhow::Result<()> {
    loop {
        if let Err(e) = extract_all().await {
            eprintln!("Extractor error: {e}");
        }
        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

async fn extract_all() -> anyhow::Result<()> {
    let epubs_root = Path::new(EPUBS_DIR);
    let html_root = Path::new(HTML_DIR);
    fs::create_dir_all(html_root).await?;
    fs::create_dir_all(epubs_root).await?;
    let mut epub_paths: HashSet<DirHelper> = HashSet::new();
    for entry in WalkDir::new(epubs_root) {
        let entry = entry?;
        let entry = entry.path().strip_prefix(EPUBS_DIR)?;
        if entry.starts_with("api") {
            continue;
        }
        if entry.extension().map_or(false, |e| e == "epub") {
            let entry = entry.with_extension("");
            epub_paths.insert(DirHelper::new(entry.to_path_buf()));
        }
    }

    let mut html_paths: HashSet<DirHelper> = HashSet::new();
    for entry in WalkDir::new(html_root) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name() == ".hash" {
            let entry = entry.path().strip_prefix(HTML_DIR)?;
            let entry = entry.parent().unwrap();
            html_paths.insert(DirHelper::new(entry.to_path_buf()));
        }
    }

    for epub_file in epub_paths.iter() {
        match maybe_convert(&epub_file.epub_file_path(), &epub_file.html_dir()).await {
            Ok(_) => {}
            Err(e) => {
                eprintln!("ERROR CONVERTING EPUB: {}", e.to_string());
            }
        }
    }

    for html_path in html_paths.iter() {
        if !epub_paths.contains(&html_path) {
            fs::remove_dir_all(&html_path.html_dir()).await?;
        }
    }

    return anyhow::Ok(());
}

async fn maybe_convert(epub_path: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let hash = hash_file(epub_path).await?;
    let hash_path = out_dir.join(".hash");

    if hash_path.exists() {
        let stored = fs::read_to_string(&hash_path).await?;
        if stored.trim() == hash {
            return Ok(());
        }
    }

    fs::create_dir_all(out_dir).await?;
    clean_output_dir(out_dir).await;
    convert_epub(epub_path, out_dir).await?;
    fs::write(hash_path, &hash).await?;
    Ok(())
}

/// Remove old section_*.html, index.html, and resource subdirectories before
/// re-conversion so stale files from a prior version don't linger.
async fn clean_output_dir(out_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(out_dir) else {
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
        } else if name == "index.json"
            || name == "index.html"
            || (name.starts_with("section_") && name.ends_with(".html"))
        {
            let _ = fs::remove_file(&path);
        }
    }
}

async fn hash_file(path: &Path) -> anyhow::Result<String> {
    let mut file = fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

async fn convert_epub(epub_path: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let mut doc =
        EpubDoc::new(epub_path).map_err(|e| anyhow::anyhow!("Failed to open epub: {e}"))?;
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

    let mut info = ReadPositionFileData::default();

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
        let content = inject_scroll_script(&content);
        let filename = format!("section_{:03}", i + 1);
        info.read_position.insert(
            filename.clone(),
            ReadPosition::new_default(filename.clone()),
        );
        let filename = format!("{}.html", filename);
        fs::write(out_dir.join(&filename), &content).await?;
        sections.push((title, filename));
    }

    fs::write(
        out_dir.join(".info.json"),
        serde_json::to_string_pretty(&info)?,
    )
    .await?;

    if toc_hits == 0 && !sections.is_empty() {
        eprintln!(
            "Warning: 0 TOC matches for {} — chapter titles are from <title> tags or fallbacks",
            epub_path.display()
        );
    }

    // Extract images, CSS, and fonts
    extract_resources(&mut doc, out_dir, &root_base).await?;

    let book_name = out_dir.file_name().unwrap().to_string_lossy();
    fs::write(
        out_dir.join("index.json"),
        generate_index(&book_name, &sections),
    )
    .await?;

    Ok(())
}

/// Extract non-HTML resources (images, CSS, fonts) from the EPUB into out_dir,
/// preserving directory structure relative to root_base.
async fn extract_resources(
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
            fs::create_dir_all(parent).await?;
        }
        fs::write(&dest, bytes).await?;
    }
    Ok(())
}

/// Inject the scroll-position tracking script before </body>.
/// Uses rfind on a lowercased copy for byte-safe, case-insensitive matching.
/// Falls back to appending if </body> is absent (some EPUB sections are fragments).
fn inject_scroll_script(html: &str) -> String {
    const SCRIPT: &str = r#"<script>
(function() {
  function getNodePath(el) {
    var path = [];
    var node = el;
    while (node && node !== document.body) {
      var parent = node.parentElement;
      if (!parent) break;
      path.unshift(Array.prototype.indexOf.call(parent.children, node));
      node = parent;
    }
    return path;
  }

  function findTopmostVisible() {
    var x = Math.max(1, Math.floor(window.innerWidth / 2));
    var el = document.elementFromPoint(x, 1);
    return el || document.body;
  }

  function resolveNodePath(nodePath) {
    var el = document.body;
    for (var i = 0; i < nodePath.length; i++) {
      if (!el.children || nodePath[i] >= el.children.length) return null;
      el = el.children[nodePath[i]];
    }
    return el;
  }

  var debounceTimer = null;
  window.addEventListener('scroll', function() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(function() {
      var el = findTopmostVisible();
      var rect = el.getBoundingClientRect();
      fetch('/api/updateReadPosition', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          path: window.location.pathname,
          node_path: getNodePath(el),
          offset: Math.max(0, -Math.round(rect.top))
        })
      }).catch(function() {});
    }, 500);
  }, { passive: true });

  window.addEventListener('load', function() {
    fetch('/api/readPosition?path=' + encodeURIComponent(window.location.pathname))
      .then(function(r) { return r.json(); })
      .then(function(data) {
        if (!data.node_path || !data.node_path.length) return;
        var el = resolveNodePath(data.node_path);
        if (!el) return;
        el.scrollIntoView({ block: 'start' });
        if (data.offset > 0) window.scrollBy(0, data.offset);
      })
      .catch(function() {});
  });
})();
</script>"#;

    let lower = html.to_ascii_lowercase();
    if let Some(pos) = lower.rfind("</body>") {
        let mut result = String::with_capacity(html.len() + SCRIPT.len() + 2);
        result.push_str(&html[..pos]);
        result.push('\n');
        result.push_str(SCRIPT);
        result.push('\n');
        result.push_str(&html[pos..]);
        result
    } else {
        let mut result = html.to_string();
        result.push('\n');
        result.push_str(SCRIPT);
        result
    }
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
    let index = serde_json::json!({
        "book_name": book_name,
        "sections": sections.iter().map(|(title, filename)| {
            serde_json::json!({ "title": title, "filename": filename })
        }).collect::<Vec<_>>()
    });
    serde_json::to_string_pretty(&index).unwrap()
}
