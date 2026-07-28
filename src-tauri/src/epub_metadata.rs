use std::{collections::HashMap, fmt, fs::File, io::Read, path::Path, str};

use quick_xml::{
    escape::unescape,
    events::{BytesStart, Event},
    Reader, XmlVersion,
};
use serde::Serialize;
use zip::ZipArchive;

const DUBLIN_CORE_NAMESPACE: &[u8] = b"http://purl.org/dc/elements/1.1/";
const AMBIGUOUS_SERIES_WARNING: &str = "ambiguous_series";
const SERIES_CONFLICT_WARNING: &str = "series_conflict";

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct NormalizedMetadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub language: Option<String>,
    pub identifiers: Vec<MetadataIdentifier>,
    pub series: Option<String>,
    pub series_index: Option<String>,
    #[serde(skip)]
    pub warnings: Vec<String>,
}

impl NormalizedMetadata {
    pub fn missing_required_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.title.is_none() {
            warnings.push("missing_title".to_string());
        }
        if self.authors.is_empty() {
            warnings.push("missing_author".to_string());
        }
        if self.language.is_none() {
            warnings.push("missing_language".to_string());
        }

        warnings
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataIdentifier {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    pub is_unique: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpubMetadataError {
    UnreadableEpub { message: String },
    MissingContainer,
    MalformedContainer { message: String },
    MissingRootfile,
    MissingPackageDocument { path: String },
    MalformedPackageDocument { path: String, message: String },
}

impl EpubMetadataError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnreadableEpub { .. } => "unreadable_epub",
            Self::MissingContainer => "missing_container_xml",
            Self::MalformedContainer { .. } => "malformed_container_xml",
            Self::MissingRootfile => "missing_rootfile",
            Self::MissingPackageDocument { .. } => "missing_package_document",
            Self::MalformedPackageDocument { .. } => "malformed_package_document",
        }
    }
}

impl fmt::Display for EpubMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnreadableEpub { message } => write!(formatter, "unreadable EPUB: {message}"),
            Self::MissingContainer => write!(formatter, "missing META-INF/container.xml"),
            Self::MalformedContainer { message } => {
                write!(formatter, "malformed META-INF/container.xml: {message}")
            }
            Self::MissingRootfile => {
                write!(
                    formatter,
                    "META-INF/container.xml has no rootfile full-path"
                )
            }
            Self::MissingPackageDocument { path } => {
                write!(formatter, "package document not found at {path}")
            }
            Self::MalformedPackageDocument { path, message } => {
                write!(formatter, "malformed package document {path}: {message}")
            }
        }
    }
}

impl std::error::Error for EpubMetadataError {}

pub fn read_embedded_metadata(epub_path: &Path) -> Result<NormalizedMetadata, EpubMetadataError> {
    let file = File::open(epub_path).map_err(|error| EpubMetadataError::UnreadableEpub {
        message: error.to_string(),
    })?;
    let mut archive = ZipArchive::new(file).map_err(|error| EpubMetadataError::UnreadableEpub {
        message: error.to_string(),
    })?;

    let container_xml = read_container_xml(&mut archive)?;
    let package_path = package_document_path(&container_xml)?;
    let package_xml = read_package_document(&mut archive, &package_path)?;

    parse_package_metadata(&package_xml, &package_path)
}

fn read_container_xml(archive: &mut ZipArchive<File>) -> Result<String, EpubMetadataError> {
    let mut container = archive
        .by_name("META-INF/container.xml")
        .map_err(|_| EpubMetadataError::MissingContainer)?;
    read_zip_file_to_string(&mut container)
        .map_err(|error| EpubMetadataError::MalformedContainer { message: error })
}

fn read_package_document(
    archive: &mut ZipArchive<File>,
    package_path: &str,
) -> Result<String, EpubMetadataError> {
    let mut package =
        archive
            .by_name(package_path)
            .map_err(|_| EpubMetadataError::MissingPackageDocument {
                path: package_path.to_string(),
            })?;
    read_zip_file_to_string(&mut package).map_err(|message| {
        EpubMetadataError::MalformedPackageDocument {
            path: package_path.to_string(),
            message,
        }
    })
}

fn read_zip_file_to_string<R: Read>(reader: &mut R) -> Result<String, String> {
    let mut contents = String::new();
    reader
        .read_to_string(&mut contents)
        .map_err(|error| error.to_string())?;
    Ok(contents)
}

fn package_document_path(container_xml: &str) -> Result<String, EpubMetadataError> {
    let mut reader = Reader::from_str(container_xml);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element))
                if local_name(element.name().as_ref()) == b"rootfile" =>
            {
                let path = attribute_value(&reader, &element, b"full-path")
                    .map_err(|message| EpubMetadataError::MalformedContainer { message })?;

                return path
                    .filter(|path| !path.trim().is_empty())
                    .map(|path| path.trim().to_string())
                    .ok_or(EpubMetadataError::MissingRootfile);
            }
            Ok(Event::Eof) => return Err(EpubMetadataError::MissingRootfile),
            Err(error) => {
                return Err(EpubMetadataError::MalformedContainer {
                    message: error.to_string(),
                })
            }
            _ => {}
        }
    }
}

fn parse_package_metadata(
    package_xml: &str,
    package_path: &str,
) -> Result<NormalizedMetadata, EpubMetadataError> {
    let mut reader = Reader::from_str(package_xml);
    reader.config_mut().trim_text(true);

    let mut namespaces = NamespaceContext::default();
    let mut unique_identifier_id = None;
    let mut in_metadata = false;
    let mut metadata_depth = 0usize;
    let mut current = None;
    let mut current_meta = None;
    let mut parsed = ParsedPackage::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                namespaces
                    .push_from_element(&reader, &element)
                    .map_err(|message| package_xml_error(package_path, message))?;
                let name = element.name();
                let element_name = local_name(name.as_ref());

                if element_name == b"package" {
                    unique_identifier_id = attribute_value(&reader, &element, b"unique-identifier")
                        .map_err(|message| package_xml_error(package_path, message))?;
                } else if element_name == b"metadata" {
                    in_metadata = true;
                    metadata_depth = 1;
                } else if in_metadata {
                    metadata_depth += 1;
                    if let Some(target) =
                        text_target_from_element(&reader, &element, &namespaces)
                            .map_err(|message| package_xml_error(package_path, message))?
                    {
                        current = Some(target);
                    } else if element_name == b"meta" {
                        current_meta = meta_target_from_element(&reader, &element)
                            .map_err(|message| package_xml_error(package_path, message))?;
                    }
                }
            }
            Ok(Event::Empty(element)) => {
                let name = element.name();
                let element_name = local_name(name.as_ref());

                if element_name == b"package" {
                    unique_identifier_id = attribute_value(&reader, &element, b"unique-identifier")
                        .map_err(|message| package_xml_error(package_path, message))?;
                } else if in_metadata && element_name == b"meta" {
                    if let Some(meta) = meta_target_from_element(&reader, &element)
                        .map_err(|message| package_xml_error(package_path, message))?
                    {
                        parsed.push_meta_target(meta);
                    }
                }
            }
            Ok(Event::Text(text)) => {
                let decoded = text
                    .xml10_content()
                    .map_err(|error| package_xml_error(package_path, error.to_string()))?;
                let unescaped = unescape(&decoded)
                    .map_err(|error| package_xml_error(package_path, error.to_string()))?;
                append_text(&mut current, &mut current_meta, &unescaped);
            }
            Ok(Event::CData(text)) => {
                let decoded = text
                    .decode()
                    .map_err(|error| package_xml_error(package_path, error.to_string()))?;
                append_text(&mut current, &mut current_meta, &decoded);
            }
            Ok(Event::End(element)) => {
                let name = element.name();
                let element_name = local_name(name.as_ref());

                if in_metadata {
                    if current
                        .as_ref()
                        .is_some_and(|target| target.kind.local_name() == element_name)
                    {
                        if let Some(target) = current.take() {
                            parsed.push_text_target(target);
                        }
                    } else if element_name == b"meta" {
                        if let Some(meta) = current_meta.take() {
                            parsed.push_meta_target(meta);
                        }
                    }

                    if element_name == b"metadata" {
                        in_metadata = false;
                        metadata_depth = 0;
                    } else if metadata_depth > 0 {
                        metadata_depth -= 1;
                    }
                }

                namespaces.pop();
            }
            Ok(Event::Eof) => {
                if in_metadata || current.is_some() || current_meta.is_some() {
                    return Err(package_xml_error(
                        package_path,
                        "unexpected end of package document".to_string(),
                    ));
                }
                break;
            }
            Err(error) => return Err(package_xml_error(package_path, error.to_string())),
            _ => {}
        }
    }

    Ok(parsed.into_normalized(unique_identifier_id.as_deref()))
}

fn package_xml_error(package_path: &str, message: String) -> EpubMetadataError {
    EpubMetadataError::MalformedPackageDocument {
        path: package_path.to_string(),
        message,
    }
}

fn append_text(
    current: &mut Option<TextTarget>,
    current_meta: &mut Option<MetaTarget>,
    text: &str,
) {
    if let Some(target) = current {
        target.text.push_str(text);
    }
    if let Some(meta) = current_meta {
        meta.text.push_str(text);
    }
}

fn text_target_from_element(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
    namespaces: &NamespaceContext,
) -> Result<Option<TextTarget>, String> {
    let name = element.name();
    let Some(element_name) = dublin_core_local_name(name.as_ref(), namespaces) else {
        return Ok(None);
    };

    match element_name {
        b"title" => Ok(Some(TextTarget::new(TextKind::Title))),
        b"language" => Ok(Some(TextTarget::new(TextKind::Language))),
        b"creator" => Ok(Some(TextTarget {
            kind: TextKind::Creator {
                id: attribute_value(reader, element, b"id")?,
                role: attribute_value(reader, element, b"role")?,
            },
            text: String::new(),
        })),
        b"identifier" => Ok(Some(TextTarget {
            kind: TextKind::Identifier {
                id: attribute_value(reader, element, b"id")?,
                scheme: attribute_value(reader, element, b"scheme")?,
            },
            text: String::new(),
        })),
        _ => Ok(None),
    }
}

fn meta_target_from_element(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
) -> Result<Option<MetaTarget>, String> {
    let property = attribute_value(reader, element, b"property")?;
    let refines = attribute_value(reader, element, b"refines")?;
    let id = attribute_value(reader, element, b"id")?;
    let name = attribute_value(reader, element, b"name")?;
    let content = attribute_value(reader, element, b"content")?.unwrap_or_default();

    if let Some(name) = name.as_deref().map(str::trim) {
        match name {
            "calibre:series" => {
                return Ok(Some(MetaTarget {
                    kind: MetaKind::CalibreSeries,
                    text: content,
                }))
            }
            "calibre:series_index" => {
                return Ok(Some(MetaTarget {
                    kind: MetaKind::CalibreSeriesIndex,
                    text: content,
                }))
            }
            _ => {}
        }
    }

    let refined_target_id = refines.as_deref().and_then(refined_target_id);

    Ok(
        match (property.as_deref().map(str::trim), refined_target_id) {
            (Some("role"), Some(creator_id)) => Some(MetaTarget {
                kind: MetaKind::RoleRefinement { creator_id },
                text: String::new(),
            }),
            (Some("belongs-to-collection"), _) => id.map(|id| MetaTarget {
                kind: MetaKind::Epub3Collection { id },
                text: String::new(),
            }),
            (Some("collection-type"), Some(collection_id)) => Some(MetaTarget {
                kind: MetaKind::CollectionType { collection_id },
                text: String::new(),
            }),
            (Some("group-position"), Some(collection_id)) => Some(MetaTarget {
                kind: MetaKind::GroupPosition { collection_id },
                text: String::new(),
            }),
            _ => None,
        },
    )
}

fn refined_target_id(refines: &str) -> Option<String> {
    refines
        .trim()
        .strip_prefix('#')
        .filter(|target_id| !target_id.is_empty())
        .map(ToString::to_string)
}

fn attribute_value(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
    attribute_name: &[u8],
) -> Result<Option<String>, String> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;
        if local_name(attribute.key.as_ref()) == attribute_name {
            return attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                .map(|value| Some(value.into_owned()))
                .map_err(|error| error.to_string());
        }
    }

    Ok(None)
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':' || *byte == b'}')
        .next()
        .unwrap_or(name)
}

#[derive(Default)]
struct NamespaceContext {
    stack: Vec<HashMap<String, String>>,
}

impl NamespaceContext {
    fn push_from_element(
        &mut self,
        reader: &Reader<&[u8]>,
        element: &BytesStart<'_>,
    ) -> Result<(), String> {
        let mut namespaces = self.stack.last().cloned().unwrap_or_default();
        apply_namespace_declarations(reader, element, &mut namespaces)?;
        self.stack.push(namespaces);
        Ok(())
    }

    fn pop(&mut self) {
        self.stack.pop();
    }

    fn namespace_uri(&self, prefix: &[u8]) -> Option<&str> {
        let prefix = str::from_utf8(prefix).ok()?;
        self.stack
            .last()
            .and_then(|namespaces| namespaces.get(prefix))
            .map(String::as_str)
    }
}

fn apply_namespace_declarations(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
    namespaces: &mut HashMap<String, String>,
) -> Result<(), String> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;
        let key = attribute.key.as_ref();
        let Some(prefix) = namespace_declaration_prefix(key) else {
            continue;
        };
        let prefix = str::from_utf8(prefix).map_err(|error| error.to_string())?;
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
            .map_err(|error| error.to_string())?;
        namespaces.insert(prefix.to_string(), value.into_owned());
    }

    Ok(())
}

fn namespace_declaration_prefix(name: &[u8]) -> Option<&[u8]> {
    if name == b"xmlns" {
        Some(b"")
    } else {
        name.strip_prefix(b"xmlns:")
    }
}

fn dublin_core_local_name<'a>(name: &'a [u8], namespaces: &NamespaceContext) -> Option<&'a [u8]> {
    if let Some((uri, local_name)) = expanded_name_parts(name) {
        return (uri == DUBLIN_CORE_NAMESPACE).then_some(local_name);
    }

    let (prefix, local_name) = prefixed_name_parts(name);
    let prefix = prefix.unwrap_or(b"");
    namespaces
        .namespace_uri(prefix)
        .filter(|uri| uri.as_bytes() == DUBLIN_CORE_NAMESPACE)
        .map(|_| local_name)
}

fn expanded_name_parts(name: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = name.iter().position(|byte| *byte == b'}')?;
    let uri = if name.first() == Some(&b'{') {
        &name[1..separator]
    } else {
        &name[..separator]
    };
    let local_name = &name[separator + 1..];

    if uri.is_empty() || local_name.is_empty() {
        None
    } else {
        Some((uri, local_name))
    }
}

fn prefixed_name_parts(name: &[u8]) -> (Option<&[u8]>, &[u8]) {
    if let Some(separator) = name.iter().position(|byte| *byte == b':') {
        (Some(&name[..separator]), &name[separator + 1..])
    } else {
        (None, name)
    }
}

#[derive(Default)]
struct ParsedPackage {
    title: Option<String>,
    creators: Vec<Creator>,
    language: Option<String>,
    identifiers: Vec<Identifier>,
    role_refinements: HashMap<String, Vec<String>>,
    epub3_collections: Vec<Epub3Collection>,
    collection_refinements: HashMap<String, CollectionRefinement>,
    calibre_series: Option<String>,
    calibre_series_index: Option<String>,
}

impl ParsedPackage {
    fn push_text_target(&mut self, target: TextTarget) {
        let text = target.text.trim();
        if text.is_empty() {
            return;
        }

        match target.kind {
            TextKind::Title if self.title.is_none() => self.title = Some(text.to_string()),
            TextKind::Language if self.language.is_none() => self.language = Some(text.to_string()),
            TextKind::Creator { id, role } => self.creators.push(Creator {
                name: text.to_string(),
                id,
                role,
            }),
            TextKind::Identifier { id, scheme } => self.identifiers.push(Identifier {
                value: text.to_string(),
                id,
                scheme,
            }),
            _ => {}
        }
    }

    fn push_meta_target(&mut self, meta: MetaTarget) {
        let text = meta.text.trim();
        if text.is_empty() {
            return;
        }

        match meta.kind {
            MetaKind::RoleRefinement { creator_id } => {
                self.role_refinements
                    .entry(creator_id)
                    .or_default()
                    .push(text.to_string());
            }
            MetaKind::Epub3Collection { id } => self.epub3_collections.push(Epub3Collection {
                id,
                name: text.to_string(),
            }),
            MetaKind::CollectionType { collection_id } => {
                self.collection_refinements
                    .entry(collection_id)
                    .or_default()
                    .collection_type = Some(text.to_string());
            }
            MetaKind::GroupPosition { collection_id } => {
                self.collection_refinements
                    .entry(collection_id)
                    .or_default()
                    .group_position = Some(text.to_string());
            }
            MetaKind::CalibreSeries if self.calibre_series.is_none() => {
                self.calibre_series = Some(text.to_string());
            }
            MetaKind::CalibreSeriesIndex if self.calibre_series_index.is_none() => {
                self.calibre_series_index = Some(text.to_string());
            }
            _ => {}
        }
    }

    fn into_normalized(self, unique_identifier_id: Option<&str>) -> NormalizedMetadata {
        let SeriesSelection {
            series,
            series_index,
            warnings,
        } = self.select_series();

        let authors = self
            .creators
            .into_iter()
            .filter(|creator| creator.is_author(&self.role_refinements))
            .map(|creator| creator.name)
            .collect();

        let identifiers = self
            .identifiers
            .into_iter()
            .map(|identifier| MetadataIdentifier {
                is_unique: unique_identifier_id
                    .zip(identifier.id.as_deref())
                    .is_some_and(|(unique_id, identifier_id)| unique_id == identifier_id),
                value: identifier.value,
                scheme: identifier.scheme,
            })
            .collect();

        NormalizedMetadata {
            title: self.title,
            authors,
            language: self.language,
            identifiers,
            series,
            series_index,
            warnings,
        }
    }

    fn select_series(&self) -> SeriesSelection {
        let supported_epub3_series: Vec<SeriesValue> = self
            .epub3_collections
            .iter()
            .filter_map(|collection| {
                let refinement = self.collection_refinements.get(&collection.id)?;
                refinement
                    .collection_type
                    .as_deref()
                    .is_some_and(|collection_type| {
                        collection_type.trim().eq_ignore_ascii_case("series")
                    })
                    .then(|| SeriesValue {
                        series: collection.name.clone(),
                        series_index: refinement
                            .group_position
                            .as_deref()
                            .map(str::trim)
                            .filter(|position| !position.is_empty())
                            .map(ToString::to_string),
                    })
            })
            .collect();

        let calibre = self.calibre_series.as_ref().map(|series| SeriesValue {
            series: series.clone(),
            series_index: self.calibre_series_index.clone(),
        });

        let mut warnings = Vec::new();

        if let Some(epub3) = supported_epub3_series.first() {
            if supported_epub3_series.len() > 1 {
                push_warning_once(&mut warnings, AMBIGUOUS_SERIES_WARNING);
            }
            if calibre
                .as_ref()
                .is_some_and(|calibre| series_values_conflict(epub3, calibre))
            {
                push_warning_once(&mut warnings, SERIES_CONFLICT_WARNING);
            }

            return SeriesSelection {
                series: Some(epub3.series.clone()),
                series_index: epub3.series_index.clone(),
                warnings,
            };
        }

        if let Some(calibre) = calibre {
            return SeriesSelection {
                series: Some(calibre.series),
                series_index: calibre.series_index,
                warnings,
            };
        }

        SeriesSelection {
            series: None,
            series_index: None,
            warnings,
        }
    }
}

struct TextTarget {
    kind: TextKind,
    text: String,
}

impl TextTarget {
    fn new(kind: TextKind) -> Self {
        Self {
            kind,
            text: String::new(),
        }
    }
}

enum TextKind {
    Title,
    Creator {
        id: Option<String>,
        role: Option<String>,
    },
    Language,
    Identifier {
        id: Option<String>,
        scheme: Option<String>,
    },
}

impl TextKind {
    fn local_name(&self) -> &[u8] {
        match self {
            Self::Title => b"title",
            Self::Creator { .. } => b"creator",
            Self::Language => b"language",
            Self::Identifier { .. } => b"identifier",
        }
    }
}

struct Creator {
    name: String,
    id: Option<String>,
    role: Option<String>,
}

impl Creator {
    fn is_author(&self, role_refinements: &HashMap<String, Vec<String>>) -> bool {
        let mut roles = Vec::new();

        if let Some(role) = &self.role {
            roles.push(role.as_str());
        }
        if let Some(id) = &self.id {
            if let Some(refined_roles) = role_refinements.get(id) {
                roles.extend(refined_roles.iter().map(String::as_str));
            }
        }

        roles.is_empty()
            || roles.iter().any(|role| {
                role.split_whitespace()
                    .any(|part| part.eq_ignore_ascii_case("aut"))
            })
    }
}

struct Identifier {
    value: String,
    id: Option<String>,
    scheme: Option<String>,
}

struct MetaTarget {
    kind: MetaKind,
    text: String,
}

enum MetaKind {
    RoleRefinement { creator_id: String },
    Epub3Collection { id: String },
    CollectionType { collection_id: String },
    GroupPosition { collection_id: String },
    CalibreSeries,
    CalibreSeriesIndex,
}

struct Epub3Collection {
    id: String,
    name: String,
}

#[derive(Default)]
struct CollectionRefinement {
    collection_type: Option<String>,
    group_position: Option<String>,
}

struct SeriesValue {
    series: String,
    series_index: Option<String>,
}

struct SeriesSelection {
    series: Option<String>,
    series_index: Option<String>,
    warnings: Vec<String>,
}

fn series_values_conflict(epub3: &SeriesValue, calibre: &SeriesValue) -> bool {
    epub3.series != calibre.series
        || epub3
            .series_index
            .as_ref()
            .zip(calibre.series_index.as_ref())
            .is_some_and(|(epub3_index, calibre_index)| epub3_index != calibre_index)
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, io::Write};
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

    #[test]
    fn reads_package_document_referenced_by_container_xml() {
        let temp = tempfile::tempdir().expect("tempdir");
        let epub_path = temp.path().join("book.epub");
        write_epub(
            &epub_path,
            "OPS/content.opf",
            r##"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/" unique-identifier="book-id">
              <metadata>
                <dc:title>Container Selected Title</dc:title>
                <dc:creator opf:role="aut">Author One</dc:creator>
                <dc:creator opf:role="edt">Editor One</dc:creator>
                <dc:creator id="c2">Author Two</dc:creator>
                <meta property="role" refines="#c2">aut</meta>
                <dc:language>en</dc:language>
                <dc:identifier id="book-id" opf:scheme="uuid">urn:uuid:123</dc:identifier>
                <dc:identifier>isbn:9780000000000</dc:identifier>
              </metadata>
            </package>"##,
        );

        let metadata = read_embedded_metadata(&epub_path).expect("metadata");

        assert_eq!(metadata.title.as_deref(), Some("Container Selected Title"));
        assert_eq!(metadata.authors, vec!["Author One", "Author Two"]);
        assert_eq!(metadata.language.as_deref(), Some("en"));
        assert_eq!(
            metadata.identifiers,
            vec![
                MetadataIdentifier {
                    value: "urn:uuid:123".to_string(),
                    scheme: Some("uuid".to_string()),
                    is_unique: true,
                },
                MetadataIdentifier {
                    value: "isbn:9780000000000".to_string(),
                    scheme: None,
                    is_unique: false,
                },
            ]
        );
    }

    #[test]
    fn ignores_non_dublin_core_metadata_elements() {
        let temp = tempfile::tempdir().expect("tempdir");
        let epub_path = temp.path().join("book.epub");
        write_epub(
            &epub_path,
            "content.opf",
            r#"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:custom="urn:not-dc" unique-identifier="book-id">
              <metadata>
                <custom:title>Wrong Title</custom:title>
                <title>Wrong Unqualified Title</title>
                <custom:creator>Wrong Author</custom:creator>
                <custom:language>xx</custom:language>
                <custom:identifier id="book-id">wrong-id</custom:identifier>
                <dc:title>Right Title</dc:title>
                <dc:creator>Right Author</dc:creator>
                <dc:language>en</dc:language>
                <dc:identifier id="book-id">right-id</dc:identifier>
              </metadata>
            </package>"#,
        );

        let metadata = read_embedded_metadata(&epub_path).expect("metadata");

        assert_eq!(metadata.title.as_deref(), Some("Right Title"));
        assert_eq!(metadata.authors, vec!["Right Author"]);
        assert_eq!(metadata.language.as_deref(), Some("en"));
        assert_eq!(
            metadata.identifiers,
            vec![MetadataIdentifier {
                value: "right-id".to_string(),
                scheme: None,
                is_unique: true,
            }]
        );
    }

    #[test]
    fn reads_dublin_core_elements_from_default_namespace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let epub_path = temp.path().join("book.epub");
        write_epub(
            &epub_path,
            "content.opf",
            r#"<?xml version="1.0"?>
            <package unique-identifier="book-id">
              <metadata xmlns="http://purl.org/dc/elements/1.1/">
                <title>Default Namespace Title</title>
                <creator>Default Namespace Author</creator>
                <language>en</language>
                <identifier id="book-id">default-id</identifier>
              </metadata>
            </package>"#,
        );

        let metadata = read_embedded_metadata(&epub_path).expect("metadata");

        assert_eq!(metadata.title.as_deref(), Some("Default Namespace Title"));
        assert_eq!(metadata.authors, vec!["Default Namespace Author"]);
        assert_eq!(metadata.language.as_deref(), Some("en"));
        assert_eq!(metadata.identifiers[0].value, "default-id");
    }

    #[test]
    fn reads_epub3_collection_series_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let epub_path = temp.path().join("book.epub");
        write_epub(
            &epub_path,
            "content.opf",
            r##"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:title>Series Book</dc:title>
                <dc:creator>Series Author</dc:creator>
                <dc:language>en</dc:language>
                <meta property="belongs-to-collection" id="collection-1">Epic Cycle</meta>
                <meta property="collection-type" refines="#collection-1">series</meta>
                <meta property="group-position" refines="#collection-1">1</meta>
              </metadata>
            </package>"##,
        );

        let metadata = read_embedded_metadata(&epub_path).expect("metadata");

        assert_eq!(metadata.series.as_deref(), Some("Epic Cycle"));
        assert_eq!(metadata.series_index.as_deref(), Some("1"));
        assert!(metadata.warnings.is_empty());
    }

    #[test]
    fn falls_back_to_calibre_series_metadata_without_epub3_series() {
        let temp = tempfile::tempdir().expect("tempdir");
        let epub_path = temp.path().join("book.epub");
        write_epub(
            &epub_path,
            "content.opf",
            r#"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:title>Calibre Book</dc:title>
                <dc:creator>Calibre Author</dc:creator>
                <dc:language>en</dc:language>
                <meta name="calibre:series" content="Fallback Saga"/>
                <meta name="calibre:series_index" content="2"/>
              </metadata>
            </package>"#,
        );

        let metadata = read_embedded_metadata(&epub_path).expect("metadata");

        assert_eq!(metadata.series.as_deref(), Some("Fallback Saga"));
        assert_eq!(metadata.series_index.as_deref(), Some("2"));
        assert!(metadata.warnings.is_empty());
    }

    #[test]
    fn warns_and_chooses_first_epub3_series_when_multiple_are_supported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let epub_path = temp.path().join("book.epub");
        write_epub(
            &epub_path,
            "content.opf",
            r##"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:title>Ambiguous Book</dc:title>
                <dc:creator>Ambiguous Author</dc:creator>
                <dc:language>en</dc:language>
                <meta property="belongs-to-collection" id="first">First Series</meta>
                <meta property="collection-type" refines="#first">series</meta>
                <meta property="group-position" refines="#first">3</meta>
                <meta property="belongs-to-collection" id="second">Second Series</meta>
                <meta property="collection-type" refines="#second">series</meta>
                <meta property="group-position" refines="#second">4</meta>
              </metadata>
            </package>"##,
        );

        let metadata = read_embedded_metadata(&epub_path).expect("metadata");

        assert_eq!(metadata.series.as_deref(), Some("First Series"));
        assert_eq!(metadata.series_index.as_deref(), Some("3"));
        assert_eq!(metadata.warnings, vec!["ambiguous_series"]);
    }

    #[test]
    fn ignores_unpaired_calibre_series_index() {
        let temp = tempfile::tempdir().expect("tempdir");
        let epub_path = temp.path().join("book.epub");
        write_epub(
            &epub_path,
            "content.opf",
            r#"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:title>Standalone Index</dc:title>
                <dc:creator>Index Author</dc:creator>
                <dc:language>en</dc:language>
                <meta name="calibre:series_index" content="3"/>
              </metadata>
            </package>"#,
        );

        let metadata = read_embedded_metadata(&epub_path).expect("metadata");

        assert_eq!(metadata.series, None);
        assert_eq!(metadata.series_index, None);
    }

    #[test]
    fn warns_and_prefers_epub3_series_when_calibre_disagrees() {
        let temp = tempfile::tempdir().expect("tempdir");
        let epub_path = temp.path().join("book.epub");
        write_epub(
            &epub_path,
            "content.opf",
            r##"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:title>Conflict Book</dc:title>
                <dc:creator>Conflict Author</dc:creator>
                <dc:language>en</dc:language>
                <meta property="belongs-to-collection" id="epub3">Preferred Series</meta>
                <meta property="collection-type" refines="#epub3">series</meta>
                <meta property="group-position" refines="#epub3">5</meta>
                <meta name="calibre:series" content="Other Series"/>
                <meta name="calibre:series_index" content="6"/>
              </metadata>
            </package>"##,
        );

        let metadata = read_embedded_metadata(&epub_path).expect("metadata");

        assert_eq!(metadata.series.as_deref(), Some("Preferred Series"));
        assert_eq!(metadata.series_index.as_deref(), Some("5"));
        assert_eq!(metadata.warnings, vec!["series_conflict"]);
    }

    #[test]
    fn malformed_package_document_is_metadata_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let epub_path = temp.path().join("book.epub");
        write_epub(
            &epub_path,
            "content.opf",
            "<package><metadata><dc:title>Broken",
        );

        let error = read_embedded_metadata(&epub_path).expect_err("malformed OPF should fail");

        assert_eq!(error.code(), "malformed_package_document");
    }

    fn write_epub(path: &Path, opf_path: &str, opf: &str) {
        let file = File::create(path).expect("create EPUB");
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        zip.start_file("META-INF/container.xml", options)
            .expect("start container");
        zip.write_all(
            format!(
                r#"<?xml version="1.0"?>
                <container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
                  <rootfiles>
                    <rootfile full-path="{opf_path}" media-type="application/oebps-package+xml"/>
                  </rootfiles>
                </container>"#
            )
            .as_bytes(),
        )
        .expect("write container");
        zip.start_file("package.opf", options)
            .expect("start decoy package");
        zip.write_all(b"<package><metadata><dc:title>Wrong</dc:title></metadata></package>")
            .expect("write decoy");
        zip.start_file(opf_path, options).expect("start OPF");
        zip.write_all(opf.as_bytes()).expect("write OPF");
        zip.finish().expect("finish EPUB");
    }
}
