use std::{fmt, path::Path};

use epub::doc::{DocError, EpubDoc, MetadataItem};
use serde::Serialize;

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
    let doc = EpubDoc::new(epub_path).map_err(map_epub_error)?;
    Ok(normalize_doc_metadata(&doc))
}

fn map_epub_error(error: DocError) -> EpubMetadataError {
    match error {
        DocError::IOError(error) => EpubMetadataError::UnreadableEpub {
            message: error.to_string(),
        },
        DocError::ArchiveError(error) => {
            let message = error.to_string();
            if message.contains("META-INF/container.xml") {
                EpubMetadataError::MissingContainer
            } else {
                EpubMetadataError::UnreadableEpub { message }
            }
        }
        DocError::XmlError(error) => EpubMetadataError::MalformedPackageDocument {
            path: "package document".to_string(),
            message: error.to_string(),
        },
        DocError::InvalidEpub => EpubMetadataError::MalformedPackageDocument {
            path: "package document".to_string(),
            message: "invalid EPUB package document".to_string(),
        },
    }
}

fn normalize_doc_metadata<R: std::io::Read + std::io::Seek>(
    doc: &EpubDoc<R>,
) -> NormalizedMetadata {
    let SeriesSelection {
        series,
        series_index,
        warnings,
    } = select_series(&doc.metadata);

    NormalizedMetadata {
        title: first_metadata_value(&doc.metadata, "title"),
        authors: doc
            .metadata
            .iter()
            .filter(|item| item.property == "creator" && is_author(item))
            .map(|item| item.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
        language: first_metadata_value(&doc.metadata, "language"),
        identifiers: doc
            .metadata
            .iter()
            .filter(|item| item.property == "identifier")
            .map(|item| MetadataIdentifier {
                value: item.value.trim().to_string(),
                scheme: item.refinement("scheme").map(|scheme| scheme.value.clone()),
                is_unique: doc
                    .unique_identifier
                    .as_deref()
                    .is_some_and(|unique| unique == item.value.trim()),
            })
            .filter(|identifier| !identifier.value.is_empty())
            .collect(),
        series,
        series_index,
        warnings,
    }
}

fn first_metadata_value(metadata: &[MetadataItem], property: &str) -> Option<String> {
    metadata
        .iter()
        .find(|item| item.property == property)
        .map(|item| item.value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn is_author(item: &MetadataItem) -> bool {
    let roles: Vec<&str> = item
        .refined
        .iter()
        .filter(|refinement| refinement.property == "role")
        .map(|refinement| refinement.value.as_str())
        .collect();

    roles.is_empty()
        || roles.iter().any(|role| {
            role.split_whitespace()
                .any(|part| part.eq_ignore_ascii_case("aut"))
        })
}

fn select_series(metadata: &[MetadataItem]) -> SeriesSelection {
    let supported_epub3_series: Vec<SeriesValue> = metadata
        .iter()
        .filter(|item| item.property == "belongs-to-collection")
        .filter(|item| {
            item.refinement("collection-type")
                .is_some_and(|collection_type| {
                    collection_type.value.trim().eq_ignore_ascii_case("series")
                })
        })
        .map(|item| SeriesValue {
            series: item.value.trim().to_string(),
            series_index: item
                .refinement("group-position")
                .map(|position| position.value.trim())
                .filter(|position| !position.is_empty())
                .map(ToString::to_string),
        })
        .filter(|series| !series.series.is_empty())
        .collect();

    let calibre = first_metadata_value(metadata, "calibre:series").map(|series| SeriesValue {
        series,
        series_index: first_metadata_value(metadata, "calibre:series_index"),
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
            <package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf" unique-identifier="book-id">
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
            <package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:custom="urn:not-dc" unique-identifier="book-id">
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
            <package xmlns="http://www.idpf.org/2007/opf" unique-identifier="book-id">
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
            <package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
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
            <package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
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
            <package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
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
            <package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
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
            <package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
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
        zip.write_all(add_minimal_reading_order(opf).as_bytes())
            .expect("write OPF");
        zip.start_file("chapter.xhtml", options)
            .expect("start chapter");
        zip.write_all(b"<html><body>Chapter</body></html>")
            .expect("write chapter");
        zip.finish().expect("finish EPUB");
    }

    fn add_minimal_reading_order(opf: &str) -> String {
        let additions = r#"
              <manifest>
                <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
              </manifest>
              <spine>
                <itemref idref="chapter"/>
              </spine>
        "#;
        opf.replacen("</package>", &format!("{additions}</package>"), 1)
    }
}
