//! EPUB page discovery. EPUBs are ZIP containers, but only raster images
//! referenced by spine documents are passed to the normal image pipeline.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use anyhow::{ensure, Context, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use zip::ZipArchive;

use crate::archive::ArchiveMetadata;
use crate::resize::is_image;

const MAX_EPUB_METADATA_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) struct EpubPage {
    pub(crate) archive_path: String,
    pub(crate) entry_name: String,
}

struct ManifestItem {
    path: Option<String>,
    media_type: String,
}

struct PackageDocument {
    manifest: HashMap<String, ManifestItem>,
    image_media_types: HashMap<String, String>,
    stylesheets: Vec<String>,
    spine: Vec<String>,
}

enum CssSource {
    Linked(String),
    InlineDeclaration(String),
    InlineStylesheet(String),
}

#[derive(Default)]
struct DocumentReferences {
    images: Vec<String>,
    stylesheets: Vec<CssSource>,
    selectors: CssSelectorContext,
}

#[derive(Default)]
struct CssSelectorContext {
    body_ids: HashSet<String>,
    body_classes: HashSet<String>,
    page_ids: HashSet<String>,
    page_classes: HashSet<String>,
}

pub(crate) fn read_epub_metadata(path: &Path) -> Result<ArchiveMetadata> {
    let file =
        File::open(path).with_context(|| format!("Failed to open EPUB: {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("Failed to open EPUB ZIP container: {}", path.display()))?;
    let pages = resolve_epub_pages(&mut archive)?;
    Ok(ArchiveMetadata {
        image_count: pages.len(),
        entry_count: pages.len(),
    })
}

pub(crate) fn resolve_epub_pages<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<Vec<EpubPage>> {
    let container = read_metadata_member(archive, "META-INF/container.xml")
        .context("EPUB is missing META-INF/container.xml")?;
    let package_path = parse_container(&container)?;
    let package_bytes = read_metadata_member(archive, &package_path)
        .with_context(|| format!("EPUB package document is missing: {package_path}"))?;
    let package = parse_package_document(&package_bytes, &package_path)?;

    let mut pages = Vec::new();
    for idref in &package.spine {
        let item = package
            .manifest
            .get(idref)
            .with_context(|| format!("EPUB spine item has no manifest entry: {idref}"))?;
        let path = item
            .path
            .as_deref()
            .with_context(|| format!("EPUB spine item has an invalid href: {idref}"))?;

        if let Some(extension) = image_extension(path, Some(&item.media_type)) {
            require_member(archive, path)?;
            push_page(&mut pages, path, &extension);
            continue;
        }

        let document = read_metadata_member(archive, path)
            .with_context(|| format!("EPUB spine document is missing: {path}"))?;
        let references = parse_document_references(&document)
            .with_context(|| format!("Failed to parse EPUB spine document: {path}"))?;

        let pages_before_direct_images = pages.len();
        for href in &references.images {
            if let Some(image_path) = resolve_epub_reference(Some(path), href) {
                add_referenced_image(archive, &mut pages, &package.image_media_types, &image_path)?;
            }
        }
        if pages.len() > pages_before_direct_images {
            continue;
        }

        // Image-focused EPUBs sometimes use CSS backgrounds instead of img or
        // SVG image elements. Match the companion epub2cbz fallback by also
        // checking package-listed stylesheets when the spine document has no
        // direct image references.
        let pages_before_css = pages.len();
        let mut css_collector = CssImageCollector::new(
            archive,
            &mut pages,
            &package.image_media_types,
            &references.selectors,
        );
        for source in &references.stylesheets {
            match source {
                CssSource::InlineDeclaration(css) => {
                    css_collector.add_inline_declaration(path, css)?
                }
                CssSource::InlineStylesheet(css) => css_collector.add_stylesheet(path, css, 0)?,
                CssSource::Linked(href) => {
                    let Some(css_path) = resolve_epub_reference(Some(path), href) else {
                        continue;
                    };
                    css_collector.add_linked_stylesheet(&css_path, 0)?;
                }
            }
        }
        if css_collector.page_count() == pages_before_css {
            for css_path in &package.stylesheets {
                css_collector.add_linked_stylesheet(css_path, 0)?;
            }
        }
    }

    ensure!(
        !pages.is_empty(),
        "EPUB contains no supported page images referenced by its spine"
    );
    Ok(pages)
}

fn parse_container(bytes: &[u8]) -> Result<String> {
    let mut reader = xml_reader(bytes);
    let mut first_rootfile = None;
    let mut package_rootfile = None;
    loop {
        match reader.read_event()? {
            Event::Start(element) | Event::Empty(element)
                if element
                    .local_name()
                    .as_ref()
                    .eq_ignore_ascii_case(b"rootfile") =>
            {
                let attributes = element_attributes(&element, &reader)?;
                let Some(path) = attribute(&attributes, "full-path") else {
                    continue;
                };
                let Some(path) = resolve_epub_reference(None, path) else {
                    continue;
                };
                if first_rootfile.is_none() {
                    first_rootfile = Some(path.clone());
                }
                if attribute(&attributes, "media-type").is_some_and(|media| {
                    media.eq_ignore_ascii_case("application/oebps-package+xml")
                }) {
                    package_rootfile = Some(path);
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    package_rootfile
        .or(first_rootfile)
        .context("EPUB container.xml has no usable package document rootfile")
}

fn parse_package_document(bytes: &[u8], package_path: &str) -> Result<PackageDocument> {
    let mut reader = xml_reader(bytes);
    let mut manifest = HashMap::new();
    let mut image_media_types = HashMap::new();
    let mut stylesheets = Vec::new();
    let mut spine = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(element) | Event::Empty(element) => {
                let name = element.local_name();
                if name.as_ref().eq_ignore_ascii_case(b"item") {
                    let attributes = element_attributes(&element, &reader)?;
                    let (Some(id), Some(href), Some(media_type)) = (
                        attribute(&attributes, "id"),
                        attribute(&attributes, "href"),
                        attribute(&attributes, "media-type"),
                    ) else {
                        continue;
                    };
                    let path = resolve_epub_reference(Some(package_path), href);
                    if media_type.eq_ignore_ascii_case("text/css") {
                        if let Some(css_path) = &path {
                            stylesheets.push(css_path.clone());
                        }
                    }
                    if media_type.to_ascii_lowercase().starts_with("image/") {
                        if let Some(image_path) = &path {
                            image_media_types
                                .insert(image_path.clone(), media_type.to_ascii_lowercase());
                        }
                    }
                    manifest.insert(
                        id.to_owned(),
                        ManifestItem {
                            path,
                            media_type: media_type.to_owned(),
                        },
                    );
                } else if name.as_ref().eq_ignore_ascii_case(b"itemref") {
                    let attributes = element_attributes(&element, &reader)?;
                    if let Some(idref) = attribute(&attributes, "idref") {
                        spine.push(idref.to_owned());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(
        !spine.is_empty(),
        "EPUB package document has an empty spine"
    );
    Ok(PackageDocument {
        manifest,
        image_media_types,
        stylesheets,
        spine,
    })
}

fn parse_document_references(bytes: &[u8]) -> Result<DocumentReferences> {
    let mut reader = xml_reader(bytes);
    let mut references = DocumentReferences::default();
    let mut style_text: Option<String> = None;
    let mut in_body = false;
    let mut found_page_div = false;
    loop {
        match reader.read_event()? {
            Event::Start(element) => {
                let name = element.local_name();
                let attributes = element_attributes(&element, &reader)?;
                if name.as_ref().eq_ignore_ascii_case(b"body") {
                    add_selector_attributes(
                        &mut references.selectors.body_ids,
                        &mut references.selectors.body_classes,
                        &attributes,
                    );
                    in_body = true;
                } else if in_body && !found_page_div && name.as_ref().eq_ignore_ascii_case(b"div") {
                    add_selector_attributes(
                        &mut references.selectors.page_ids,
                        &mut references.selectors.page_classes,
                        &attributes,
                    );
                    found_page_div = true;
                }
                collect_document_element(&element, &reader, &mut references)?;
                if name.as_ref().eq_ignore_ascii_case(b"style") {
                    style_text = Some(String::new());
                }
            }
            Event::Empty(element) => {
                let name = element.local_name();
                let attributes = element_attributes(&element, &reader)?;
                if name.as_ref().eq_ignore_ascii_case(b"body") {
                    add_selector_attributes(
                        &mut references.selectors.body_ids,
                        &mut references.selectors.body_classes,
                        &attributes,
                    );
                } else if in_body && !found_page_div && name.as_ref().eq_ignore_ascii_case(b"div") {
                    add_selector_attributes(
                        &mut references.selectors.page_ids,
                        &mut references.selectors.page_classes,
                        &attributes,
                    );
                    found_page_div = true;
                }
                collect_document_element(&element, &reader, &mut references)?;
            }
            Event::Text(text) => {
                if let Some(style) = style_text.as_mut() {
                    let decoded = text.xml_content()?;
                    style.push_str(&quick_xml::escape::unescape(&decoded)?);
                }
            }
            Event::CData(text) => {
                if let Some(style) = style_text.as_mut() {
                    style.push_str(&text.decode()?);
                }
            }
            Event::End(element) if element.local_name().as_ref().eq_ignore_ascii_case(b"style") => {
                if let Some(style) = style_text.take() {
                    references
                        .stylesheets
                        .push(CssSource::InlineStylesheet(style));
                }
            }
            Event::End(element) if element.local_name().as_ref().eq_ignore_ascii_case(b"body") => {
                in_body = false;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(references)
}

fn collect_document_element(
    element: &BytesStart<'_>,
    reader: &Reader<&[u8]>,
    references: &mut DocumentReferences,
) -> Result<()> {
    let name = element.local_name();
    let attributes = element_attributes(element, reader)?;
    if name.as_ref().eq_ignore_ascii_case(b"img") {
        if let Some(src) = attribute(&attributes, "src") {
            references.images.push(src.to_owned());
        }
    } else if name.as_ref().eq_ignore_ascii_case(b"image") {
        if let Some(href) = attribute(&attributes, "href") {
            references.images.push(href.to_owned());
        }
    } else if name.as_ref().eq_ignore_ascii_case(b"link") {
        let is_stylesheet = attribute(&attributes, "rel").is_some_and(|rel| {
            rel.split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("stylesheet"))
        });
        if is_stylesheet {
            if let Some(href) = attribute(&attributes, "href") {
                references
                    .stylesheets
                    .push(CssSource::Linked(href.to_owned()));
            }
        }
    }
    if let Some(style) = attribute(&attributes, "style") {
        references
            .stylesheets
            .push(CssSource::InlineDeclaration(style.to_owned()));
    }
    Ok(())
}

fn add_selector_attributes(
    ids: &mut HashSet<String>,
    classes: &mut HashSet<String>,
    attributes: &HashMap<String, String>,
) {
    if let Some(id) = attribute(attributes, "id") {
        ids.insert(id.to_owned());
    }
    if let Some(class) = attribute(attributes, "class") {
        classes.extend(class.split_ascii_whitespace().map(str::to_owned));
    }
}

struct CssImageCollector<'a, R: Read + Seek> {
    archive: &'a mut ZipArchive<R>,
    pages: &'a mut Vec<EpubPage>,
    image_media_types: &'a HashMap<String, String>,
    selectors: &'a CssSelectorContext,
    parsed_stylesheets: HashSet<String>,
}

impl<'a, R: Read + Seek> CssImageCollector<'a, R> {
    fn new(
        archive: &'a mut ZipArchive<R>,
        pages: &'a mut Vec<EpubPage>,
        image_media_types: &'a HashMap<String, String>,
        selectors: &'a CssSelectorContext,
    ) -> Self {
        Self {
            archive,
            pages,
            image_media_types,
            selectors,
            parsed_stylesheets: HashSet::new(),
        }
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn add_inline_declaration(&mut self, base_path: &str, css: &str) -> Result<()> {
        for reference in css_urls(css) {
            let Some(path) = resolve_epub_reference(Some(base_path), &reference) else {
                continue;
            };
            if !path.to_ascii_lowercase().ends_with(".css") {
                self.add_image(&path)?;
            }
        }
        Ok(())
    }

    fn add_linked_stylesheet(&mut self, css_path: &str, depth: usize) -> Result<()> {
        if depth >= 8 || !self.parsed_stylesheets.insert(css_path.to_owned()) {
            return Ok(());
        }
        let Ok(css_bytes) = read_metadata_member(self.archive, css_path) else {
            return Ok(());
        };
        let css = String::from_utf8_lossy(&css_bytes);
        self.add_stylesheet(css_path, &css, depth)
    }

    fn add_stylesheet(&mut self, css_path: &str, css: &str, depth: usize) -> Result<()> {
        if depth >= 8 {
            return Ok(());
        }
        for import in css_imports(css) {
            let Some(import_path) = resolve_epub_reference(Some(css_path), &import) else {
                continue;
            };
            if import_path.to_ascii_lowercase().ends_with(".css") {
                self.add_linked_stylesheet(&import_path, depth + 1)?;
            }
        }
        for reference in css_stylesheet_urls(css, self.selectors) {
            let Some(path) = resolve_epub_reference(Some(css_path), &reference) else {
                continue;
            };
            if path.to_ascii_lowercase().ends_with(".css") {
                self.add_linked_stylesheet(&path, depth + 1)?;
            } else {
                self.add_image(&path)?;
            }
        }
        Ok(())
    }

    fn add_image(&mut self, path: &str) -> Result<()> {
        let media_type = self.image_media_types.get(path).map(String::as_str);
        if let Some(extension) = image_extension(path, media_type) {
            require_member(self.archive, path)?;
            push_page(self.pages, path, &extension);
        }
        Ok(())
    }
}

fn add_referenced_image<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    pages: &mut Vec<EpubPage>,
    image_media_types: &HashMap<String, String>,
    path: &str,
) -> Result<()> {
    let media_type = image_media_types.get(path).map(String::as_str);
    if let Some(extension) = image_extension(path, media_type) {
        require_member(archive, path)?;
        push_page(pages, path, &extension);
    }
    Ok(())
}

fn push_page(pages: &mut Vec<EpubPage>, archive_path: &str, extension: &str) {
    let page_number = pages.len() + 1;
    pages.push(EpubPage {
        archive_path: archive_path.to_owned(),
        entry_name: format!("{page_number:010}.{extension}"),
    });
}

fn image_extension(path: &str, media_type: Option<&str>) -> Option<String> {
    if is_image(path) {
        return Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
    }
    let extension = match media_type?.to_ascii_lowercase().as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/gif" => "gif",
        _ => return None,
    };
    Some(extension.to_owned())
}

fn require_member<R: Read + Seek>(archive: &mut ZipArchive<R>, path: &str) -> Result<()> {
    archive
        .by_name(path)
        .with_context(|| format!("EPUB references a missing page image: {path}"))?;
    Ok(())
}

fn read_metadata_member<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    path: &str,
) -> Result<Vec<u8>> {
    let mut member = archive
        .by_name(path)
        .with_context(|| format!("EPUB member not found: {path}"))?;
    ensure!(
        member.size() <= MAX_EPUB_METADATA_BYTES,
        "EPUB metadata member is too large: {path}"
    );
    let mut bytes = Vec::with_capacity(member.size() as usize);
    member
        .read_to_end(&mut bytes)
        .with_context(|| format!("Failed to read EPUB member: {path}"))?;
    Ok(bytes)
}

fn xml_reader(bytes: &[u8]) -> Reader<&[u8]> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    reader
}

fn element_attributes(
    element: &BytesStart<'_>,
    reader: &Reader<&[u8]>,
) -> Result<HashMap<String, String>> {
    let mut attributes = HashMap::new();
    for attribute in element.attributes().with_checks(false) {
        let attribute = attribute?;
        let key = String::from_utf8_lossy(attribute.key.local_name().as_ref()).into_owned();
        let value = attribute
            .decode_and_unescape_value(reader.decoder())?
            .into_owned();
        attributes.insert(key, value);
    }
    Ok(attributes)
}

fn attribute<'a>(attributes: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn css_urls(css: &str) -> Vec<String> {
    let bytes = css.as_bytes();
    let mut urls = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"/*") {
            index = skip_css_comment(bytes, index);
            continue;
        }
        if bytes[index] == b'\'' || bytes[index] == b'"' {
            index = skip_css_string(bytes, index);
            continue;
        }
        if index + 4 <= bytes.len()
            && bytes[index..index + 3].eq_ignore_ascii_case(b"url")
            && bytes[index + 3] == b'('
            && (index == 0 || !is_css_identifier(bytes[index - 1]))
        {
            if let Some((url, after)) = parse_css_url(bytes, index + 4) {
                if !url.is_empty() {
                    urls.push(url);
                }
                index = after;
                continue;
            }
        }
        index += 1;
    }
    urls
}

fn css_stylesheet_urls(css: &str, selectors: &CssSelectorContext) -> Vec<String> {
    let bytes = css.as_bytes();
    let mut urls = Vec::new();
    let mut blocks = Vec::new();
    let mut selector_start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"/*") {
            index = skip_css_comment(bytes, index);
            continue;
        }
        if bytes[index] == b'\'' || bytes[index] == b'"' {
            index = skip_css_string(bytes, index);
            continue;
        }
        match bytes[index] {
            b'{' => {
                let selector = String::from_utf8_lossy(&bytes[selector_start..index]);
                blocks.push((index + 1, selector_matches(&selector, selectors)));
                selector_start = index + 1;
            }
            b'}' => {
                if let Some((declarations_start, selected)) = blocks.pop() {
                    if selected {
                        urls.extend(css_declaration_urls(&String::from_utf8_lossy(
                            &bytes[declarations_start..index],
                        )));
                    }
                }
                selector_start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    urls
}

fn selector_matches(selector_list: &str, context: &CssSelectorContext) -> bool {
    selector_list.split(',').any(|selector| {
        let selector = selector.trim();
        if selector.is_empty() {
            return false;
        }
        let mut ids = Vec::new();
        let mut classes = Vec::new();
        let bytes = selector.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'#' || bytes[index] == b'.' {
                let marker = bytes[index];
                index += 1;
                let start = index;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'-'))
                {
                    index += 1;
                }
                if start < index {
                    let value = String::from_utf8_lossy(&bytes[start..index]).into_owned();
                    if marker == b'#' {
                        ids.push(value);
                    } else {
                        classes.push(value);
                    }
                }
            } else {
                index += 1;
            }
        }
        if ids.is_empty() && classes.is_empty() {
            let simple_selector = selector
                .split_ascii_whitespace()
                .all(|part| matches!(part, "html" | "body" | "div" | "*" | ">"));
            return simple_selector
                && (selector.contains("body")
                    || selector.contains("html")
                    || !context.page_ids.is_empty()
                    || !context.page_classes.is_empty());
        }
        ids.iter()
            .all(|id| context.body_ids.contains(id) || context.page_ids.contains(id))
            && classes.iter().all(|class| {
                context.body_classes.contains(class) || context.page_classes.contains(class)
            })
    })
}

fn css_declaration_urls(declarations: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for declaration in declarations.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        if matches!(
            property.as_str(),
            "background" | "background-image" | "content"
        ) {
            urls.extend(css_urls(value));
        }
    }
    urls
}

fn css_imports(css: &str) -> Vec<String> {
    let bytes = css.as_bytes();
    let mut imports = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"/*") {
            index = skip_css_comment(bytes, index);
            continue;
        }
        if bytes[index] == b'\'' || bytes[index] == b'"' {
            index = skip_css_string(bytes, index);
            continue;
        }
        if index + 7 <= bytes.len()
            && bytes[index..index + 7].eq_ignore_ascii_case(b"@import")
            && (index == 0 || !is_css_identifier(bytes[index - 1]))
        {
            index += 7;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if bytes[index..].starts_with(b"url(")
                || (index + 4 <= bytes.len()
                    && bytes[index..index + 4].eq_ignore_ascii_case(b"url("))
            {
                if let Some((url, after)) = parse_css_url(bytes, index + 4) {
                    imports.push(url);
                    index = after;
                    continue;
                }
            } else if index < bytes.len() && matches!(bytes[index], b'\'' | b'"') {
                let start = index + 1;
                index = skip_css_string(bytes, index);
                if index > start + 1 {
                    imports.push(String::from_utf8_lossy(&bytes[start..index - 1]).into_owned());
                }
                continue;
            }
        }
        index += 1;
    }
    imports
}

fn parse_css_url(bytes: &[u8], mut index: usize) -> Option<(String, usize)> {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let quote = match bytes.get(index).copied()? {
        b'\'' | b'"' => {
            let quote = bytes[index];
            index += 1;
            Some(quote)
        }
        _ => None,
    };
    let start = index;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if let Some(quote) = quote {
            if byte == quote {
                let value = String::from_utf8_lossy(&bytes[start..index]).into_owned();
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
                return (bytes.get(index) == Some(&b')')).then_some((value, index + 1));
            }
        } else if byte == b')' {
            let value = String::from_utf8_lossy(&bytes[start..index])
                .trim()
                .to_owned();
            return Some((value, index + 1));
        }
        index += 1;
    }
    None
}

fn skip_css_comment(bytes: &[u8], mut index: usize) -> usize {
    index += 2;
    while index + 1 < bytes.len() {
        if bytes[index..index + 2] == *b"*/" {
            return index + 2;
        }
        index += 1;
    }
    bytes.len()
}

fn skip_css_string(bytes: &[u8], mut index: usize) -> usize {
    let quote = bytes[index];
    index += 1;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == quote {
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn is_css_identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn resolve_epub_reference(base_file: Option<&str>, reference: &str) -> Option<String> {
    let reference = reference.trim();
    if reference.is_empty()
        || reference.starts_with("//")
        || reference.starts_with('#')
        || reference.starts_with("data:")
    {
        return None;
    }
    let path = reference.split(['?', '#']).next()?.trim();
    if path.is_empty() || has_uri_scheme(path) {
        return None;
    }
    let decoded = String::from_utf8(percent_decode(path.as_bytes())?).ok()?;
    let decoded = decoded.replace('\\', "/");
    let relative = if decoded.starts_with('/') {
        decoded.trim_start_matches('/').to_owned()
    } else if let Some(base_file) = base_file {
        let parent = base_file.rsplit_once('/').map_or("", |(parent, _)| parent);
        if parent.is_empty() {
            decoded
        } else {
            format!("{parent}/{decoded}")
        }
    } else {
        decoded
    };

    let mut components = Vec::new();
    for component in relative.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            component => components.push(component),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn has_uri_scheme(path: &str) -> bool {
    let Some((scheme, _)) = path.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || (index > 0 && (byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')))
        })
}

fn percent_decode(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex_value(high)? * 16 + hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Some(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
