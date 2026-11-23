use crate::context::{file_loader, pdf_extractor};
use std::path::Path;

/// Plain-text and source-code extensions, read as text.
pub const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "rst", "adoc", "tex", "log", "csv", "tsv", "json", "jsonl", "yaml", "yml",
    "toml", "ini", "cfg", "conf", "env", "xml", "html", "htm", "css", "scss", "less", "js", "jsx", "mjs",
    "cjs", "ts", "tsx", "vue", "svelte", "py", "ipynb", "rs", "go", "java", "kt", "kts", "scala", "swift",
    "c", "h", "cpp", "cc", "hpp", "cs", "fs", "rb", "php", "pl", "lua", "r", "dart", "sh", "bash", "zsh",
    "ps1", "bat", "cmd", "sql", "graphql", "proto", "gradle", "tf",
];

/// The type NexQ indexes `path` as (its lowercased extension), or None if unsupported.
pub fn file_type_for(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    (matches!(ext.as_str(), "pdf" | "docx" | "pptx" | "xlsx") || TEXT_EXTENSIONS.contains(&ext.as_str()))
        .then_some(ext)
}

/// Extract text from a file based on its type (see `file_type_for`).
pub fn extract_text(file_path: &str, file_type: &str) -> Result<String, String> {
    match file_type.to_lowercase().as_str() {
        "pdf" => pdf_extractor::extract_text_from_pdf(file_path),
        "docx" => extract_docx_text(file_path),
        "pptx" => extract_pptx_text(file_path),
        "xlsx" => extract_xlsx_text(file_path),
        t if TEXT_EXTENSIONS.contains(&t) => file_loader::load_text_file(file_path),
        other => Err(format!(
            "Unsupported file type for text extraction: {}",
            other
        )),
    }
}

/// Largest archive entry read (uncompressed), so a hostile file can't exhaust memory.
const MAX_ZIP_ENTRY_BYTES: u64 = 50 * 1024 * 1024;

/// The (name, text) of every entry in the ZIP at `file_path` whose name passes `wanted`.
fn zip_entries(file_path: &str, wanted: impl Fn(&str) -> bool) -> Result<Vec<(String, String)>, String> {
    let file = std::fs::File::open(file_path).map_err(|e| format!("Failed to open '{}': {}", file_path, e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Failed to read '{}' as ZIP: {}", file_path, e))?;
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("Bad ZIP entry in '{}': {}", file_path, e))?;
        let Ok(name) = entry.name().map(|n| n.to_string()) else { continue };
        if !wanted(&name) || entry.size() > MAX_ZIP_ENTRY_BYTES {
            continue;
        }
        let mut text = String::new();
        std::io::Read::read_to_string(&mut entry, &mut text).map_err(|e| format!("Failed to read {}: {}", name, e))?;
        out.push((name, text));
    }
    Ok(out)
}

/// The number in an entry name ("ppt/slides/slide12.xml" -> 12), for ordering.
fn entry_number(name: &str) -> u32 {
    name.chars().filter(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)
}

/// Decode the XML entities `&amp; &lt; &gt; &quot; &apos; &#N; &#xN;`.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| match &rest[1..e] {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            n if n.starts_with("#x") => u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32),
            n if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        });
        match (decoded, end) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The decoded text of every `<tag>` element in `xml`, in order.
fn element_texts(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        rest = &rest[i + open.len()..];
        // `<a:t>` or `<a:t xml:space=..>`, not `<a:tbl>`
        if !rest.starts_with('>') && !rest.starts_with(' ') {
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        if rest[..gt].ends_with('/') {
            rest = &rest[gt + 1..];
            continue;
        }
        let body = &rest[gt + 1..];
        let Some(end) = body.find(&close) else { break };
        out.push(decode_entities(&body[..end]));
        rest = &body[end + close.len()..];
    }
    out
}

/// Slide text from a PowerPoint file, one line per paragraph, slides in order.
pub fn extract_pptx_text(file_path: &str) -> Result<String, String> {
    let mut slides = zip_entries(file_path, |n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))?;
    slides.sort_by_key(|(name, _)| entry_number(name));
    let parts: Vec<String> = slides
        .iter()
        .enumerate()
        .filter_map(|(i, (_, xml))| {
            let lines: Vec<String> = xml
                .split("</a:p>")
                .map(|p| element_texts(p, "a:t").concat())
                .filter(|l| !l.trim().is_empty())
                .collect();
            (!lines.is_empty()).then(|| format!("Slide {}:\n{}", i + 1, lines.join("\n")))
        })
        .collect();
    Ok(parts.join("\n\n"))
}

/// Cell text from an Excel file: one line per row, cells separated by tabs.
pub fn extract_xlsx_text(file_path: &str) -> Result<String, String> {
    let entries = zip_entries(file_path, |n| {
        n == "xl/sharedStrings.xml" || (n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
    })?;
    let shared: Vec<String> = entries
        .iter()
        .find(|(n, _)| n == "xl/sharedStrings.xml")
        .map(|(_, xml)| xml.split("</si>").map(|si| element_texts(si, "t").concat()).collect())
        .unwrap_or_default();
    let mut sheets: Vec<&(String, String)> = entries.iter().filter(|(n, _)| n.starts_with("xl/worksheets/")).collect();
    sheets.sort_by_key(|(name, _)| entry_number(name));
    let parts: Vec<String> = sheets
        .iter()
        .enumerate()
        .filter_map(|(i, (_, xml))| {
            let rows = sheet_rows(xml, &shared);
            (!rows.is_empty()).then(|| format!("Sheet {}:\n{}", i + 1, rows.join("\n")))
        })
        .collect();
    Ok(parts.join("\n\n"))
}

/// The value of attribute `name` in a tag's attribute text (`r="A1" t="s"`).
fn attribute<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{}=\"", name);
    let mut from = 0;
    while let Some(i) = attrs[from..].find(&key) {
        let at = from + i;
        if at == 0 || attrs.as_bytes()[at - 1] == b' ' {
            return attrs[at + key.len()..].split('"').next();
        }
        from = at + key.len();
    }
    None
}

fn sheet_rows(xml: &str, shared: &[String]) -> Vec<String> {
    xml.split("</row>")
        .filter_map(|row| {
            let cells: Vec<String> = row
                .split("<c ")
                .skip(1)
                .filter_map(|cell| {
                    let (attrs, body) = cell.split_once('>')?;
                    if attrs.ends_with('/') {
                        return None;
                    }
                    let body = body.split("</c>").next().unwrap_or("");
                    let value = match attribute(attrs, "t") {
                        Some("s") => element_texts(body, "v")
                            .first()
                            .and_then(|i| i.trim().parse::<usize>().ok())
                            .and_then(|i| shared.get(i).cloned()),
                        Some("inlineStr") => Some(element_texts(body, "t").concat()),
                        _ => element_texts(body, "v").into_iter().next(),
                    }?;
                    (!value.trim().is_empty()).then_some(value)
                })
                .collect();
            (!cells.is_empty()).then(|| cells.join("\t"))
        })
        .collect()
}

/// Extract text from a DOCX (Office Open XML) file.
///
/// Opens the file as a ZIP archive, reads `word/document.xml`,
/// and extracts text content from `<w:t>` elements using simple
/// string parsing (no full XML parser dependency).
pub fn extract_docx_text(file_path: &str) -> Result<String, String> {
    let file = std::fs::File::open(file_path)
        .map_err(|e| format!("Failed to open DOCX file '{}': {}", file_path, e))?;

    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("Failed to read DOCX as ZIP archive: {}", e))?;

    let mut xml_content = String::new();
    {
        let mut document_xml = archive
            .by_name("word/document.xml")
            .map_err(|e| format!("DOCX missing word/document.xml: {}", e))?;

        std::io::Read::read_to_string(&mut document_xml, &mut xml_content)
            .map_err(|e| format!("Failed to read document.xml: {}", e))?;
    }

    Ok(parse_docx_xml(&xml_content))
}

/// Parse text content from DOCX XML by extracting `<w:t>` element text.
///
/// Uses simple string splitting rather than a full XML parser:
/// 1. Split on '<' to get tag fragments
/// 2. Look for fragments starting with "w:t>" or "w:t " (the w:t element)
/// 3. Extract the text content after the '>'
/// 4. Track `<w:p>` (paragraph) boundaries to insert newlines
fn parse_docx_xml(xml: &str) -> String {
    let mut result = String::new();
    let mut in_paragraph = false;

    // Split on '<' to get tag-like fragments
    for fragment in xml.split('<') {
        if fragment.is_empty() {
            continue;
        }

        // Check for paragraph start: "w:p " or "w:p>"
        if fragment.starts_with("w:p>") || fragment.starts_with("w:p ") {
            if in_paragraph && !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            in_paragraph = true;
            continue;
        }

        // Check for paragraph end: "/w:p>"
        if fragment.starts_with("/w:p>") {
            if in_paragraph && !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            in_paragraph = false;
            continue;
        }

        // Check for text element: "w:t>" or "w:t " (with attributes like xml:space)
        if fragment.starts_with("w:t>") || fragment.starts_with("w:t ") {
            // Extract text after the closing '>'
            if let Some(pos) = fragment.find('>') {
                let text = &fragment[pos + 1..];
                result.push_str(text);
            }
        }
    }

    let trimmed = result.trim().to_string();

    // Clean up excessive blank lines (collapse multiple newlines to double)
    let mut cleaned = String::with_capacity(trimmed.len());
    let mut prev_newline_count = 0;
    for ch in trimmed.chars() {
        if ch == '\n' {
            prev_newline_count += 1;
            if prev_newline_count <= 2 {
                cleaned.push(ch);
            }
        } else {
            prev_newline_count = 0;
            cleaned.push(ch);
        }
    }

    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_docx_xml_simple() {
        let xml = r#"<w:body><w:p><w:r><w:t>Hello World</w:t></w:r></w:p></w:body>"#;
        let text = parse_docx_xml(xml);
        assert!(text.contains("Hello World"));
    }

    #[test]
    fn test_parse_docx_xml_multiple_paragraphs() {
        let xml = r#"<w:body><w:p><w:r><w:t>First</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p></w:body>"#;
        let text = parse_docx_xml(xml);
        assert!(text.contains("First"));
        assert!(text.contains("Second"));
        // Paragraphs should be separated by newlines
        assert!(text.contains('\n'));
    }

    #[test]
    fn test_parse_docx_xml_with_attributes() {
        let xml = r#"<w:p><w:r><w:t xml:space="preserve"> spaced text </w:t></w:r></w:p>"#;
        let text = parse_docx_xml(xml);
        assert!(text.contains("spaced text"));
    }

    #[test]
    fn test_parse_docx_xml_empty() {
        let text = parse_docx_xml("");
        assert!(text.is_empty());
    }

    #[test]
    fn test_extract_docx_text_from_a_deflated_archive() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("nexq-docx-{}.docx", std::process::id()));
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("word/document.xml", options).unwrap();
            zip.write_all(br#"<w:body><w:p><w:r><w:t>Quarterly plan</w:t></w:r></w:p></w:body>"#).unwrap();
            zip.finish().unwrap();
        }
        let text = extract_docx_text(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
        assert!(text.unwrap().contains("Quarterly plan"));
    }

    fn write_zip(name: &str, entries: &[(&str, &str)]) -> std::path::PathBuf {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("nexq-{}-{}", std::process::id(), name));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (entry, body) in entries {
            zip.start_file(*entry, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn test_extract_pptx_text_in_slide_order() {
        let path = write_zip("deck.pptx", &[
            ("ppt/slides/slide10.xml", r#"<p:sld><a:p><a:r><a:t>Tenth</a:t></a:r></a:p></p:sld>"#),
            ("ppt/slides/slide2.xml", r#"<p:sld><a:p><a:r><a:t>Q3 </a:t></a:r><a:r><a:t xml:space="preserve">churn &amp; pricing</a:t></a:r></a:p><a:p><a:r><a:t>Next steps</a:t></a:r></a:p><a:tbl/></p:sld>"#),
            ("ppt/slides/_rels/slide2.xml.rels", "<x/>"),
        ]);
        let text = extract_pptx_text(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(text, "Slide 1:\nQ3 churn & pricing\nNext steps\n\nSlide 2:\nTenth");
    }

    #[test]
    fn test_extract_xlsx_text_resolves_shared_strings() {
        let path = write_zip("book.xlsx", &[
            ("xl/sharedStrings.xml", r#"<sst><si><t>Account</t></si><si><r><t>Acme </t></r><r><t>Corp</t></r></si></sst>"#),
            ("xl/worksheets/sheet1.xml", r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="inlineStr"><is><t>Seats</t></is></c></row><row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2"><v>50</v></c><c r="C2" s="1"/></row></sheetData></worksheet>"#),
        ]);
        let text = extract_xlsx_text(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(text, "Sheet 1:\nAccount\tSeats\nAcme Corp\t50");
    }

    #[test]
    fn test_decode_entities() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#65;&#x42; &bogus; &"), "a & b <c> AB &bogus; &");
    }

    #[test]
    fn test_attribute_needs_a_whole_name() {
        assert_eq!(attribute(r#"r="A1" s="2" t="s""#, "t"), Some("s"));
        assert_eq!(attribute(r#"r="A1" st="x""#, "t"), None);
    }

    #[test]
    fn test_file_type_for() {
        assert_eq!(file_type_for(Path::new("C:/x/Deck.PPTX")).as_deref(), Some("pptx"));
        assert_eq!(file_type_for(Path::new("main.rs")).as_deref(), Some("rs"));
        assert_eq!(file_type_for(Path::new("photo.png")), None);
        assert_eq!(file_type_for(Path::new("Makefile")), None);
    }

    #[test]
    fn test_extract_text_unsupported() {
        let result = extract_text("test.xyz", "xyz");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported"));
    }
}
