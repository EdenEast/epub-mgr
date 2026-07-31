use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

use crate::enrichment::merge::{FieldPatch, MetadataField};

#[derive(Debug)]
pub enum EpubWriteError {
    OutputExists,
    Io { message: String },
    Zip { message: String },
    MissingContainer,
    MissingRootfile,
    MissingPackageDocument { path: String },
    MalformedPackageDocument { message: String },
}

impl fmt::Display for EpubWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputExists => write!(formatter, "output path already exists"),
            Self::Io { message } => write!(formatter, "I/O error: {message}"),
            Self::Zip { message } => write!(formatter, "ZIP error: {message}"),
            Self::MissingContainer => write!(formatter, "missing META-INF/container.xml"),
            Self::MissingRootfile => write!(
                formatter,
                "META-INF/container.xml has no rootfile full-path"
            ),
            Self::MissingPackageDocument { path } => {
                write!(formatter, "package document not found at {path}")
            }
            Self::MalformedPackageDocument { message } => {
                write!(formatter, "malformed package document: {message}")
            }
        }
    }
}

impl std::error::Error for EpubWriteError {}

pub fn copy_epub_with_metadata_patches(
    source_path: &Path,
    target_path: &Path,
    patches: &[FieldPatch],
) -> Result<(), EpubWriteError> {
    if target_path.try_exists().map_err(map_io)? {
        return Err(EpubWriteError::OutputExists);
    }

    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent).map_err(map_io)?;
    }

    let temp_path = temp_output_path(target_path);
    if temp_path.try_exists().map_err(map_io)? {
        fs::remove_file(&temp_path).map_err(map_io)?;
    }

    let result = rewrite_epub(source_path, &temp_path, patches).and_then(|()| {
        fs::rename(&temp_path, target_path).map_err(map_io)?;
        Ok(())
    });

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }

    result
}

fn rewrite_epub(
    source_path: &Path,
    target_path: &Path,
    patches: &[FieldPatch],
) -> Result<(), EpubWriteError> {
    let source = File::open(source_path).map_err(map_io)?;
    let mut archive = ZipArchive::new(source).map_err(map_zip)?;
    let container = read_zip_string(&mut archive, "META-INF/container.xml")?
        .ok_or(EpubWriteError::MissingContainer)?;
    let opf_path = rootfile_path(&container).ok_or(EpubWriteError::MissingRootfile)?;

    let target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target_path)
        .map_err(map_io)?;
    let mut writer = ZipWriter::new(target);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(map_zip)?;
        let name = file.name().to_string();
        if file.is_dir() {
            writer.add_directory(name, options).map_err(map_zip)?;
            continue;
        }

        let mut contents = Vec::new();
        file.read_to_end(&mut contents).map_err(map_io)?;
        if name == opf_path {
            let opf = String::from_utf8(contents).map_err(|error| {
                EpubWriteError::MalformedPackageDocument {
                    message: error.to_string(),
                }
            })?;
            contents = apply_patches_to_opf(&opf, patches)?.into_bytes();
        }

        writer.start_file(name, options).map_err(map_zip)?;
        writer.write_all(&contents).map_err(map_io)?;
    }

    writer.finish().map_err(map_zip)?;
    Ok(())
}

fn read_zip_string(
    archive: &mut ZipArchive<File>,
    path: &str,
) -> Result<Option<String>, EpubWriteError> {
    let Ok(mut file) = archive.by_name(path) else {
        return Ok(None);
    };
    let mut contents = String::new();
    file.read_to_string(&mut contents).map_err(map_io)?;
    Ok(Some(contents))
}

fn rootfile_path(container: &str) -> Option<String> {
    let marker = "full-path=\"";
    let start = container.find(marker)? + marker.len();
    let end = container[start..].find('"')? + start;
    Some(container[start..end].to_string())
}

pub fn apply_patches_to_opf(opf: &str, patches: &[FieldPatch]) -> Result<String, EpubWriteError> {
    let metadata_start =
        opf.find("<metadata")
            .ok_or_else(|| EpubWriteError::MalformedPackageDocument {
                message: "missing metadata element".to_string(),
            })?;
    let metadata_open_end = opf[metadata_start..]
        .find('>')
        .map(|index| metadata_start + index + 1)
        .ok_or_else(|| EpubWriteError::MalformedPackageDocument {
            message: "unclosed metadata element".to_string(),
        })?;
    let metadata_close = opf[metadata_open_end..]
        .find("</metadata>")
        .map(|index| metadata_open_end + index)
        .ok_or_else(|| EpubWriteError::MalformedPackageDocument {
            message: "missing metadata close element".to_string(),
        })?;

    let mut metadata = opf[metadata_open_end..metadata_close].to_string();
    for patch in patches.iter().filter(|patch| patch.applied) {
        match patch.field {
            MetadataField::Title => replace_or_insert_dc(&mut metadata, "title", &patch.new_value),
            MetadataField::Author => {
                replace_or_insert_dc(&mut metadata, "creator", &patch.new_value)
            }
            MetadataField::Series | MetadataField::SeriesIndex => {}
        }
    }

    let series = patches
        .iter()
        .find(|patch| patch.applied && patch.field == MetadataField::Series)
        .map(|patch| patch.new_value.as_str());
    let series_index = patches
        .iter()
        .find(|patch| patch.applied && patch.field == MetadataField::SeriesIndex)
        .map(|patch| patch.new_value.as_str());
    if series.is_some() || series_index.is_some() {
        remove_existing_series_metadata(&mut metadata);
        let series_value = series.unwrap_or("");
        metadata.push_str(&format!(
            "\n    <meta property=\"belongs-to-collection\" id=\"epub-mgr-series\">{}</meta>\n    <meta refines=\"#epub-mgr-series\" property=\"collection-type\">series</meta>",
            escape_xml(series_value)
        ));
        if let Some(series_index) = series_index {
            metadata.push_str(&format!(
                "\n    <meta refines=\"#epub-mgr-series\" property=\"group-position\">{}</meta>",
                escape_xml(series_index)
            ));
        }
        if let Some(series) = series {
            metadata.push_str(&format!(
                "\n    <meta name=\"calibre:series\" content=\"{}\"/>",
                escape_xml(series)
            ));
        }
        if let Some(series_index) = series_index {
            metadata.push_str(&format!(
                "\n    <meta name=\"calibre:series_index\" content=\"{}\"/>",
                escape_xml(series_index)
            ));
        }
        metadata.push('\n');
    }

    let mut updated = String::new();
    updated.push_str(&opf[..metadata_open_end]);
    updated.push_str(&metadata);
    updated.push_str(&opf[metadata_close..]);
    Ok(updated)
}

fn replace_or_insert_dc(metadata: &mut String, local_name: &str, value: &str) {
    let open = format!("<dc:{local_name}");
    let close = format!("</dc:{local_name}>");
    if let Some(start) = metadata.find(&open) {
        if let Some(open_end) = metadata[start..].find('>').map(|index| start + index + 1) {
            if let Some(close_start) = metadata[open_end..]
                .find(&close)
                .map(|index| open_end + index)
            {
                metadata.replace_range(open_end..close_start, &escape_xml(value));
                return;
            }
        }
    }

    metadata.push_str(&format!(
        "\n    <dc:{local_name}>{}</dc:{local_name}>",
        escape_xml(value)
    ));
}

fn remove_existing_series_metadata(metadata: &mut String) {
    for marker in [
        "calibre:series_index",
        "calibre:series",
        "belongs-to-collection",
        "group-position",
        "collection-type",
    ] {
        while let Some(index) = metadata.find(marker) {
            let line_start = metadata[..index].rfind('\n').map_or(0, |index| index + 1);
            let line_end = metadata[index..]
                .find('\n')
                .map_or(metadata.len(), |end| index + end + 1);
            metadata.replace_range(line_start..line_end, "");
        }
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn temp_output_path(target_path: &Path) -> PathBuf {
    let mut file_name = target_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "epub-mgr-output".into());
    file_name.push(".tmp");
    target_path.with_file_name(file_name)
}

fn map_io(error: io::Error) -> EpubWriteError {
    EpubWriteError::Io {
        message: error.to_string(),
    }
}

fn map_zip(error: zip::result::ZipError) -> EpubWriteError {
    EpubWriteError::Zip {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::enrichment::{
        merge::{FieldPatch, MetadataField},
        Confidence, Provenance,
    };

    use super::apply_patches_to_opf;

    fn patch(field: MetadataField, value: &str) -> FieldPatch {
        FieldPatch {
            field,
            old_value: None,
            new_value: value.to_string(),
            confidence: Confidence::High,
            provenance: Provenance {
                source: "test".to_string(),
                record_id: "record".to_string(),
                url: "https://example.test".to_string(),
            },
            applied: true,
            reason: "test".to_string(),
        }
    }

    #[test]
    fn writes_title_author_and_epub3_series_metadata() {
        let opf = r#"<package xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>Old</dc:title><dc:creator>Old Author</dc:creator></metadata></package>"#;

        let updated = apply_patches_to_opf(
            opf,
            &[
                patch(MetadataField::Title, "The Way of Kings"),
                patch(MetadataField::Author, "Brandon Sanderson"),
                patch(MetadataField::Series, "The Stormlight Archive"),
                patch(MetadataField::SeriesIndex, "1"),
            ],
        )
        .expect("patches apply");

        assert!(updated.contains("<dc:title>The Way of Kings</dc:title>"));
        assert!(updated.contains("<dc:creator>Brandon Sanderson</dc:creator>"));
        assert!(updated.contains("property=\"belongs-to-collection\""));
        assert!(updated.contains("The Stormlight Archive"));
        assert!(updated.contains("property=\"group-position\">1</meta>"));
        assert!(updated.contains("name=\"calibre:series\""));
    }
}
