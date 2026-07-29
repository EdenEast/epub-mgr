use std::{fmt, path::PathBuf};

const FALLBACK_TITLE: &str = "Unknown Title";
const FALLBACK_AUTHOR: &str = "Unknown Author";
const MAX_SEGMENT_CHARS: usize = 180;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub authors: Option<String>,
    pub author_sort: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<String>,
    pub language: Option<String>,
    pub identifier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedOutputPath {
    pub relative_path: PathBuf,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRenderError {
    message: String,
}

impl PathRenderError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PathRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PathRenderError {}

pub fn render_output_path(
    template: &str,
    metadata: &NormalizedMetadata,
) -> Result<RenderedOutputPath, PathRenderError> {
    let mut warnings = Vec::new();
    let rendered = render_template(template, metadata, false, &mut warnings)?;
    let relative_path = sanitize_rendered_path(&rendered)?;

    Ok(RenderedOutputPath {
        relative_path,
        warnings,
    })
}

fn render_template(
    template: &str,
    metadata: &NormalizedMetadata,
    optional_context: bool,
    warnings: &mut Vec<String>,
) -> Result<String, PathRenderError> {
    let mut rendered = String::new();
    let mut rest = template;

    while let Some(offset) = rest.find(['[', '{']) {
        let (literal, tail) = rest.split_at(offset);
        rendered.push_str(literal);

        if tail.starts_with('[') {
            let close = find_optional_close(tail)?;
            let section = &tail[1..close];
            if optional_section_has_required_values(section, metadata)? {
                rendered.push_str(&render_template(section, metadata, true, warnings)?);
            }
            rest = &tail[close + 1..];
            continue;
        }

        let close = tail
            .find('}')
            .ok_or_else(|| PathRenderError::new("unclosed placeholder in Output Path Template"))?;
        let placeholder = &tail[1..close];
        let value = render_placeholder(placeholder, metadata, optional_context, warnings)?;
        rendered.push_str(&value);
        rest = &tail[close + 1..];
    }

    rendered.push_str(rest);
    Ok(rendered)
}

fn find_optional_close(input: &str) -> Result<usize, PathRenderError> {
    let mut depth = 0;

    for (index, character) in input.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(index);
                }
            }
            _ => {}
        }
    }

    Err(PathRenderError::new(
        "unclosed optional section in Output Path Template",
    ))
}

fn optional_section_has_required_values(
    section: &str,
    metadata: &NormalizedMetadata,
) -> Result<bool, PathRenderError> {
    let mut placeholders = Vec::new();
    let mut rest = section;

    while let Some(open) = rest.find('{') {
        let tail = &rest[open..];
        let close = tail
            .find('}')
            .ok_or_else(|| PathRenderError::new("unclosed placeholder in Output Path Template"))?;
        placeholders.push(parse_placeholder(&tail[1..close])?);
        rest = &tail[close + 1..];
    }

    if placeholders.is_empty() {
        return Ok(false);
    }

    Ok(placeholders.iter().all(|placeholder| {
        metadata_value(metadata, placeholder.field).is_some_and(|value| !value.trim().is_empty())
    }))
}

fn render_placeholder(
    raw_placeholder: &str,
    metadata: &NormalizedMetadata,
    optional_context: bool,
    warnings: &mut Vec<String>,
) -> Result<String, PathRenderError> {
    let placeholder = parse_placeholder(raw_placeholder)?;

    let raw_value =
        metadata_value(metadata, placeholder.field).filter(|value| !value.trim().is_empty());

    let (value, fallback_used) = match raw_value {
        Some(value) => (value.trim().to_string(), false),
        None if optional_context => return Ok(String::new()),
        None => match fallback_for(placeholder.field) {
            Some(fallback) => {
                warnings.push(format!(
                    "missing {}; using fallback {fallback}",
                    placeholder.field.as_str()
                ));
                (fallback.to_string(), true)
            }
            None if placeholder.field == Field::Identifier => {
                warnings.push("missing identifier; rendering empty string".to_string());
                return Ok(String::new());
            }
            None => {
                return Err(PathRenderError::new(format!(
                    "missing required metadata field {} for Output Path Template",
                    placeholder.field.as_str()
                )))
            }
        },
    };

    let formatted = format_placeholder_value(placeholder, &value, fallback_used, warnings)?;
    Ok(sanitize_segment(&formatted))
}

fn format_placeholder_value(
    placeholder: Placeholder,
    value: &str,
    fallback_used: bool,
    warnings: &mut Vec<String>,
) -> Result<String, PathRenderError> {
    match placeholder.format {
        None => Ok(value.to_string()),
        Some("02") if placeholder.field == Field::SeriesIndex => {
            if fallback_used {
                return Ok(value.to_string());
            }

            if value.chars().all(|character| character.is_ascii_digit()) {
                let digits = value.trim_start_matches('0');
                let digits = if digits.is_empty() { "0" } else { digits };
                Ok(if digits.len() < 2 {
                    format!("0{digits}")
                } else {
                    digits.to_string()
                })
            } else {
                warnings.push(format!(
                    "series_index value {value:?} is not integer-looking; leaving unpadded"
                ));
                Ok(value.to_string())
            }
        }
        Some(format) => Err(PathRenderError::new(format!(
            "unsupported placeholder format :{format} for {}",
            placeholder.field.as_str()
        ))),
    }
}

fn sanitize_rendered_path(rendered: &str) -> Result<PathBuf, PathRenderError> {
    let segments: Vec<String> = rendered
        .split(['/', '\\'])
        .map(sanitize_segment)
        .filter(|segment| !segment.is_empty())
        .collect();

    if segments.is_empty() {
        return Err(PathRenderError::new(
            "Output Path Template rendered an empty path",
        ));
    }

    Ok(segments.iter().collect())
}

fn sanitize_segment(segment: &str) -> String {
    let mut sanitized = String::new();
    let mut previous_was_whitespace = false;

    for character in segment.trim().chars() {
        if is_forbidden_path_character(character) {
            sanitized.push('_');
            previous_was_whitespace = false;
        } else if character.is_whitespace() {
            if !previous_was_whitespace {
                sanitized.push(' ');
                previous_was_whitespace = true;
            }
        } else {
            sanitized.push(character);
            previous_was_whitespace = false;
        }
    }

    let mut sanitized = sanitized
        .trim_matches(|character| character == ' ' || character == '.')
        .chars()
        .take(MAX_SEGMENT_CHARS)
        .collect::<String>();

    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        sanitized = "_".to_string();
    }

    if is_reserved_name(&sanitized) {
        sanitized.insert(0, '_');
    }

    sanitized
}

fn is_forbidden_path_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*'
        )
}

fn is_reserved_name(segment: &str) -> bool {
    let base_name = segment.split('.').next().unwrap_or(segment);
    let upper = base_name.to_ascii_uppercase();

    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper.strip_prefix("COM").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || upper.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
}

fn metadata_value(metadata: &NormalizedMetadata, field: Field) -> Option<&str> {
    match field {
        Field::Title => metadata.title.as_deref(),
        Field::Author => metadata.author.as_deref(),
        Field::Authors => metadata.authors.as_deref(),
        Field::AuthorSort => metadata.author_sort.as_deref(),
        Field::Series => metadata.series.as_deref(),
        Field::SeriesIndex => metadata.series_index.as_deref(),
        Field::Language => metadata.language.as_deref(),
        Field::Identifier => metadata.identifier.as_deref(),
    }
}

fn fallback_for(field: Field) -> Option<&'static str> {
    match field {
        Field::Title => Some(FALLBACK_TITLE),
        Field::Author | Field::Authors => Some(FALLBACK_AUTHOR),
        Field::AuthorSort
        | Field::Series
        | Field::SeriesIndex
        | Field::Language
        | Field::Identifier => None,
    }
}

fn parse_placeholder(input: &str) -> Result<Placeholder<'_>, PathRenderError> {
    let (field, format) = input
        .split_once(':')
        .map_or((input, None), |(field, format)| (field, Some(format)));
    let field = Field::parse(field)?;

    Ok(Placeholder { field, format })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placeholder<'a> {
    field: Field,
    format: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Title,
    Author,
    Authors,
    AuthorSort,
    Series,
    SeriesIndex,
    Language,
    Identifier,
}

impl Field {
    fn parse(input: &str) -> Result<Self, PathRenderError> {
        match input {
            "title" => Ok(Self::Title),
            "author" => Ok(Self::Author),
            "authors" => Ok(Self::Authors),
            "author_sort" => Ok(Self::AuthorSort),
            "series" => Ok(Self::Series),
            "series_index" => Ok(Self::SeriesIndex),
            "language" => Ok(Self::Language),
            "identifier" => Ok(Self::Identifier),
            _ => Err(PathRenderError::new(format!(
                "unsupported metadata field {input:?} in Output Path Template"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Author => "author",
            Self::Authors => "authors",
            Self::AuthorSort => "author_sort",
            Self::Series => "series",
            Self::SeriesIndex => "series_index",
            Self::Language => "language",
            Self::Identifier => "identifier",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> NormalizedMetadata {
        NormalizedMetadata {
            title: Some("The Fellowship".to_string()),
            author: Some("J. R. R. Tolkien".to_string()),
            series: Some("The Lord of the Rings".to_string()),
            series_index: Some("1".to_string()),
            ..NormalizedMetadata::default()
        }
    }

    #[test]
    fn renders_default_template_with_series_and_padded_series_index() {
        let rendered = render_output_path(
            "{author}/[{series}/{series_index:02} ]{title}.epub",
            &metadata(),
        )
        .expect("render path");

        assert_eq!(
            rendered.relative_path,
            PathBuf::from("J. R. R. Tolkien/The Lord of the Rings/01 The Fellowship.epub")
        );
        assert!(rendered.warnings.is_empty());
    }

    #[test]
    fn omits_optional_sections_missing_non_fallback_values() {
        let rendered = render_output_path(
            "{author}/[{series}/{series_index:02} ]{title}.epub",
            &NormalizedMetadata::default(),
        )
        .expect("render path");

        assert_eq!(
            rendered.relative_path,
            PathBuf::from("Unknown Author/Unknown Title.epub")
        );
        assert_eq!(
            rendered.warnings,
            vec![
                "missing author; using fallback Unknown Author".to_string(),
                "missing title; using fallback Unknown Title".to_string()
            ]
        );
    }

    #[test]
    fn leaves_non_integer_looking_series_index_unpadded_with_warning() {
        let mut metadata = metadata();
        metadata.series_index = Some("Volume 1".to_string());

        let rendered = render_output_path(
            "{author}/[{series}/{series_index:02} ]{title}.epub",
            &metadata,
        )
        .expect("render path");

        assert_eq!(
            rendered.relative_path,
            PathBuf::from("J. R. R. Tolkien/The Lord of the Rings/Volume 1 The Fellowship.epub")
        );
        assert_eq!(
            rendered.warnings,
            vec!["series_index value \"Volume 1\" is not integer-looking; leaving unpadded"]
        );
    }

    #[test]
    fn missing_required_non_fallback_field_outside_optional_section_is_error() {
        let error = render_output_path("{series}/{title}.epub", &NormalizedMetadata::default())
            .expect_err("missing series should fail");

        assert_eq!(
            error.to_string(),
            "missing required metadata field series for Output Path Template"
        );
    }

    #[test]
    fn missing_authors_outside_optional_section_uses_author_fallback() {
        let rendered = render_output_path("{authors}/{title}.epub", &NormalizedMetadata::default())
            .expect("render path");

        assert_eq!(
            rendered.relative_path,
            PathBuf::from("Unknown Author/Unknown Title.epub")
        );
        assert_eq!(
            rendered.warnings,
            vec![
                "missing authors; using fallback Unknown Author".to_string(),
                "missing title; using fallback Unknown Title".to_string()
            ]
        );
    }

    #[test]
    fn missing_identifier_outside_optional_section_renders_empty_with_warning() {
        let rendered =
            render_output_path("{identifier}-{title}.epub", &metadata()).expect("render path");

        assert_eq!(
            rendered.relative_path,
            PathBuf::from("-The Fellowship.epub")
        );
        assert_eq!(
            rendered.warnings,
            vec!["missing identifier; rendering empty string"]
        );
    }

    #[test]
    fn optional_section_with_missing_identifier_disappears_without_warning() {
        let rendered = render_output_path("{title}/[{identifier}/]copy.epub", &metadata())
            .expect("render path");

        assert_eq!(
            rendered.relative_path,
            PathBuf::from("The Fellowship/copy.epub")
        );
        assert!(rendered.warnings.is_empty());
    }

    #[test]
    fn sanitizes_rendered_path_segments() {
        let metadata = NormalizedMetadata {
            author: Some(" CON ".to_string()),
            title: Some(" Bad / Name \u{0} With   Spaces ".to_string()),
            ..NormalizedMetadata::default()
        };

        let rendered = render_output_path("{author}/{title}.epub", &metadata).expect("render path");

        assert_eq!(
            rendered.relative_path,
            PathBuf::from("_CON/Bad _ Name _ With Spaces.epub")
        );
    }
}
