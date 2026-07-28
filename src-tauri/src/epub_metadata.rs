use std::{collections::HashMap, fmt, fs::File, io::Read, path::Path};

use quick_xml::{
    escape::unescape,
    events::{BytesStart, Event},
    Reader, XmlVersion,
};
use serde::Serialize;
use zip::ZipArchive;

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct NormalizedMetadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub language: Option<String>,
    pub identifiers: Vec<MetadataIdentifier>,
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

    let mut unique_identifier_id = None;
    let mut in_metadata = false;
    let mut metadata_depth = 0usize;
    let mut current = None;
    let mut current_role_refinement = None;
    let mut parsed = ParsedPackage::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
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
                    if let Some(target) = text_target_from_element(&reader, &element, element_name)
                        .map_err(|message| package_xml_error(package_path, message))?
                    {
                        current = Some(target);
                    } else if element_name == b"meta" {
                        current_role_refinement =
                            role_refinement_from_element(&reader, &element)
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
                    let _ = role_refinement_from_element(&reader, &element)
                        .map_err(|message| package_xml_error(package_path, message))?;
                }
            }
            Ok(Event::Text(text)) => {
                let decoded = text
                    .xml10_content()
                    .map_err(|error| package_xml_error(package_path, error.to_string()))?;
                let unescaped = unescape(&decoded)
                    .map_err(|error| package_xml_error(package_path, error.to_string()))?;
                append_text(&mut current, &mut current_role_refinement, &unescaped);
            }
            Ok(Event::CData(text)) => {
                let decoded = text
                    .decode()
                    .map_err(|error| package_xml_error(package_path, error.to_string()))?;
                append_text(&mut current, &mut current_role_refinement, &decoded);
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
                        if let Some(refinement) = current_role_refinement.take() {
                            parsed.push_role_refinement(refinement);
                        }
                    }

                    if element_name == b"metadata" {
                        in_metadata = false;
                        metadata_depth = 0;
                    } else if metadata_depth > 0 {
                        metadata_depth -= 1;
                    }
                }
            }
            Ok(Event::Eof) => {
                if in_metadata || current.is_some() || current_role_refinement.is_some() {
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
    role_refinement: &mut Option<RoleRefinement>,
    text: &str,
) {
    if let Some(target) = current {
        target.text.push_str(text);
    }
    if let Some(refinement) = role_refinement {
        refinement.role.push_str(text);
    }
}

fn text_target_from_element(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
    element_name: &[u8],
) -> Result<Option<TextTarget>, String> {
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

fn role_refinement_from_element(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
) -> Result<Option<RoleRefinement>, String> {
    let property = attribute_value(reader, element, b"property")?;
    let refines = attribute_value(reader, element, b"refines")?;

    Ok(match (property, refines) {
        (Some(property), Some(refines))
            if property.trim() == "role" && refines.trim().starts_with('#') =>
        {
            Some(RoleRefinement {
                creator_id: refines.trim().trim_start_matches('#').to_string(),
                role: String::new(),
            })
        }
        _ => None,
    })
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
struct ParsedPackage {
    title: Option<String>,
    creators: Vec<Creator>,
    language: Option<String>,
    identifiers: Vec<Identifier>,
    role_refinements: HashMap<String, Vec<String>>,
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

    fn push_role_refinement(&mut self, refinement: RoleRefinement) {
        let role = refinement.role.trim();
        if role.is_empty() {
            return;
        }

        self.role_refinements
            .entry(refinement.creator_id)
            .or_default()
            .push(role.to_string());
    }

    fn into_normalized(self, unique_identifier_id: Option<&str>) -> NormalizedMetadata {
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

struct RoleRefinement {
    creator_id: String,
    role: String,
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
