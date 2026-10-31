//! JSON / YAML / XML parsing, formatting, conversion and path queries.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use formflux_shared::fs as sfs;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{Map, Value};

use crate::error::{FormfluxError, Result};
use crate::io;

/// Structured data formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataFormat {
    /// JSON.
    Json,
    /// YAML.
    Yaml,
    /// XML.
    Xml,
}

impl DataFormat {
    /// File extension.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Xml => "xml",
        }
    }

    /// Parse a name or extension.
    pub fn parse(value: &str) -> Result<Self> {
        match value
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str()
        {
            "json" => Ok(Self::Json),
            "yaml" | "yml" => Ok(Self::Yaml),
            "xml" => Ok(Self::Xml),
            other => Err(FormfluxError::unsupported(format!(
                "unknown data format '{other}' (try json, yaml or xml)"
            ))),
        }
    }

    /// Detect from a path.
    pub fn from_path(path: &Path) -> Result<Self> {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| FormfluxError::invalid("input has no extension"))?;
        Self::parse(extension)
    }
}

/// Parse JSON.
pub fn parse_json(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|error| {
        FormfluxError::invalid(format!(
            "invalid JSON at line {}, column {}: {}",
            error.line(),
            error.column(),
            error
        ))
    })
}

/// Parse YAML into a JSON value tree.
pub fn parse_yaml(text: &str) -> Result<Value> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(text)?;
    Ok(serde_json::to_value(yaml)?)
}

/// Parse XML into a JSON value tree.  Attributes become `@name`, text becomes
/// `#text`, repeated children become arrays.
pub fn parse_xml(text: &str) -> Result<Value> {
    #[derive(Debug, Default)]
    struct Node {
        name: String,
        attributes: BTreeMap<String, String>,
        children: Vec<Node>,
        text: String,
    }

    impl Node {
        fn into_value(self) -> Value {
            if self.attributes.is_empty() && self.children.is_empty() {
                let trimmed = self.text.trim();
                return Value::String(trimmed.to_string());
            }
            let mut object = Map::new();
            for (name, value) in self.attributes {
                object.insert(format!("@{name}"), Value::String(value));
            }
            let trimmed = self.text.trim();
            if !trimmed.is_empty() {
                object.insert("#text".to_string(), Value::String(trimmed.to_string()));
            }
            let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            for child in self.children {
                let name = child.name.clone();
                groups.entry(name).or_default().push(child.into_value());
            }
            for (name, mut values) in groups {
                let value = if values.len() == 1 {
                    values.remove(0)
                } else {
                    Value::Array(values)
                };
                object.insert(name, value);
            }
            Value::Object(object)
        }
    }

    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = String::from_utf8_lossy(event.name().as_ref()).into_owned();
                if name.is_empty() {
                    return Err(FormfluxError::invalid("XML element has an empty name"));
                }
                let mut node = Node {
                    name,
                    ..Node::default()
                };
                for attribute in event.attributes().flatten() {
                    let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
                    let value = attribute
                        .decode_and_unescape_value(reader.decoder())
                        .map_err(|error| {
                            FormfluxError::invalid(format!("invalid XML attribute: {error}"))
                        })?
                        .into_owned();
                    node.attributes.insert(key, value);
                }
                stack.push(node);
            }
            Ok(Event::Empty(event)) => {
                let name = String::from_utf8_lossy(event.name().as_ref()).into_owned();
                let mut node = Node {
                    name,
                    ..Node::default()
                };
                for attribute in event.attributes().flatten() {
                    let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
                    let value = attribute
                        .decode_and_unescape_value(reader.decoder())
                        .map_err(|error| {
                            FormfluxError::invalid(format!("invalid XML attribute: {error}"))
                        })?
                        .into_owned();
                    node.attributes.insert(key, value);
                }
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                } else if root.is_none() {
                    root = Some(node);
                } else {
                    return Err(FormfluxError::invalid("XML has multiple root elements"));
                }
            }
            Ok(Event::Text(event)) => {
                if let Some(parent) = stack.last_mut() {
                    let decoded = event.unescape().map_err(|error| {
                        FormfluxError::invalid(format!("invalid XML text: {error}"))
                    })?;
                    parent.text.push_str(&decoded);
                }
            }
            Ok(Event::CData(event)) => {
                if let Some(parent) = stack.last_mut() {
                    parent
                        .text
                        .push_str(&String::from_utf8_lossy(event.as_ref()));
                }
            }
            Ok(Event::End(_)) => {
                let node = stack
                    .pop()
                    .ok_or_else(|| FormfluxError::invalid("unbalanced XML closing tag"))?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                } else if root.is_none() {
                    root = Some(node);
                } else {
                    return Err(FormfluxError::invalid("XML has multiple root elements"));
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(FormfluxError::invalid(format!(
                    "invalid XML at position {}: {error}",
                    reader.buffer_position()
                )))
            }
        }
    }
    if !stack.is_empty() {
        return Err(FormfluxError::invalid("XML has unclosed elements"));
    }
    let root = root.ok_or_else(|| FormfluxError::invalid("XML has no root element"))?;
    let mut object = Map::new();
    let name = root.name.clone();
    object.insert(name, root.into_value());
    Ok(Value::Object(object))
}

/// Serialize JSON with optional pretty printing.
pub fn to_json(value: &Value, pretty: bool) -> Result<String> {
    if pretty {
        Ok(serde_json::to_string_pretty(value)?)
    } else {
        Ok(serde_json::to_string(value)?)
    }
}

/// Serialize YAML.
pub fn to_yaml(value: &Value) -> Result<String> {
    Ok(serde_yaml::to_string(value)?)
}

/// Serialize a JSON tree as XML with a configurable root name.
pub fn to_xml(value: &Value, root: &str) -> Result<String> {
    let root = root.trim();
    if root.is_empty()
        || !root
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(FormfluxError::invalid("invalid XML root name"));
    }
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    write_xml_node(&mut out, root, value, 0);
    Ok(out)
}

fn write_xml_node(out: &mut String, name: &str, value: &Value, depth: usize) {
    let indent = "  ".repeat(depth);
    match value {
        Value::Array(items) => {
            for item in items {
                write_xml_node(out, name, item, depth);
            }
        }
        Value::Object(object) => {
            let attributes: Vec<(&String, &Value)> = object
                .iter()
                .filter(|(key, _)| key.starts_with('@'))
                .collect();
            out.push_str(&indent);
            out.push('<');
            out.push_str(name);
            for (key, value) in &attributes {
                if let Value::String(text) = value {
                    out.push(' ');
                    out.push_str(key.trim_start_matches('@'));
                    out.push_str("=\"");
                    out.push_str(&escape_xml(text));
                    out.push('"');
                }
            }
            let text = object
                .get("#text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let children: Vec<(&String, &Value)> = object
                .iter()
                .filter(|(key, _)| !key.starts_with('@') && key.as_str() != "#text")
                .collect();
            if children.is_empty() && text.is_empty() {
                out.push_str("/>\n");
                return;
            }
            out.push('>');
            if !text.is_empty() {
                out.push_str(&escape_xml(text));
            }
            if !children.is_empty() {
                out.push('\n');
                for (key, value) in children {
                    write_xml_node(out, key, value, depth + 1);
                }
                out.push_str(&indent);
            }
            out.push_str("</");
            out.push_str(name);
            out.push_str(">\n");
        }
        Value::Null => {
            out.push_str(&format!("{indent}<{name}/>\n"));
        }
        other => {
            out.push_str(&format!(
                "{indent}<{name}>{}</{name}>\n",
                escape_xml(&xml_scalar(other))
            ));
        }
    }
}

fn xml_scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => String::new(),
    }
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Render structured text into another format.
pub fn transform(text: &str, from: DataFormat, to: DataFormat, pretty: bool) -> Result<String> {
    let value = match from {
        DataFormat::Json => parse_json(text)?,
        DataFormat::Yaml => parse_yaml(text)?,
        DataFormat::Xml => parse_xml(text)?,
    };
    match to {
        DataFormat::Json => to_json(&value, pretty),
        DataFormat::Yaml => to_yaml(&value),
        DataFormat::Xml => to_xml(&value, "root"),
    }
}

/// Validate a document, returning its format-specific parse error if any.
pub fn validate(text: &str, format: DataFormat) -> Result<()> {
    match format {
        DataFormat::Json => {
            parse_json(text)?;
        }
        DataFormat::Yaml => {
            parse_yaml(text)?;
        }
        DataFormat::Xml => {
            parse_xml(text)?;
        }
    }
    Ok(())
}

/// Convert a file into `output_dir`.
pub fn convert_file(
    input: &Path,
    output_dir: &Path,
    target: DataFormat,
    pretty: bool,
    overwrite: bool,
) -> Result<PathBuf> {
    let input = io::existing_file(input)?;
    io::ensure_dir(output_dir)?;
    let source = DataFormat::from_path(&input)?;
    let text = io::read_text(&input, io::TEXT_LIMIT)?;
    let rendered = transform(&text, source, target, pretty)?;
    let output = sfs::destination_for(&input, output_dir, Some(target.extension()), overwrite)?;
    io::write_text(&output, &rendered, overwrite)?;
    Ok(output)
}

/// A parsed query step.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
}

fn parse_query(path: &str) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    let mut buffer = String::new();
    let mut chars = path.trim().trim_start_matches('.').chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '.' => {
                if !buffer.is_empty() {
                    steps.push(Step::Key(std::mem::take(&mut buffer)));
                }
            }
            '[' => {
                if !buffer.is_empty() {
                    steps.push(Step::Key(std::mem::take(&mut buffer)));
                }
                let mut index = String::new();
                for next in chars.by_ref() {
                    if next == ']' {
                        break;
                    }
                    index.push(next);
                }
                let index = index.trim().trim_matches(['"', '\'']);
                let parsed = index.parse::<usize>().map_err(|_| {
                    FormfluxError::invalid(format!("invalid array index '{index}'"))
                })?;
                steps.push(Step::Index(parsed));
            }
            _ => buffer.push(character),
        }
    }
    if !buffer.is_empty() {
        steps.push(Step::Key(buffer));
    }
    if steps.is_empty() {
        return Err(FormfluxError::invalid("query path is empty"));
    }
    Ok(steps)
}

/// Query a JSON/YAML/XML document with a jq-like path (`.a.b[0].c`).
pub fn query(text: &str, format: DataFormat, path: &str) -> Result<Value> {
    let value = match format {
        DataFormat::Json => parse_json(text)?,
        DataFormat::Yaml => parse_yaml(text)?,
        DataFormat::Xml => parse_xml(text)?,
    };
    let steps = parse_query(path)?;
    let mut current = &value;
    for step in steps {
        current = match (step, current) {
            (Step::Key(key), Value::Object(object)) => object
                .get(&key)
                .ok_or_else(|| FormfluxError::NotFound(PathBuf::from(key)))?,
            (Step::Index(index), Value::Array(items)) => items.get(index).ok_or_else(|| {
                FormfluxError::invalid(format!("array index {index} is out of range"))
            })?,
            (step, _) => {
                return Err(FormfluxError::invalid(format!(
                    "query step {step:?} does not match the document shape"
                )))
            }
        };
    }
    Ok(current.clone())
}

/// Pretty-print JSON text.
pub fn format_json(text: &str, pretty: bool) -> Result<String> {
    let value = parse_json(text)?;
    to_json(&value, pretty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip() {
        let text = r#"{"name":"Formflux","items":[1,2,3],"ok":true}"#;
        let value = parse_json(text).unwrap();
        assert_eq!(value["name"], "Formflux");
        let pretty = to_json(&value, true).unwrap();
        assert!(pretty.contains("\n  \"name\""));
        assert_eq!(parse_json(&pretty).unwrap(), value);
    }

    #[test]
    fn yaml_converts_to_json() {
        let text = "name: Formflux\nitems:\n  - 1\n  - 2\n";
        let value = parse_yaml(text).unwrap();
        assert_eq!(value["name"], "Formflux");
        assert_eq!(value["items"][1], 2);
    }

    #[test]
    fn xml_round_trip() {
        let text = r#"<root><item id="1">one</item><item id="2">two</item></root>"#;
        let value = parse_xml(text).unwrap();
        assert_eq!(value["root"]["item"][0]["@id"], "1");
        assert_eq!(value["root"]["item"][1]["#text"], "two");
        let xml = to_xml(&value, "root").unwrap();
        assert!(xml.contains("<root>"));
        assert!(xml.contains("id=\"1\""));
    }

    #[test]
    fn query_supports_keys_and_indices() {
        let text = r#"{"a":{"b":[{"c":42}]}}"#;
        assert_eq!(query(text, DataFormat::Json, ".a.b[0].c").unwrap(), 42);
        assert!(query(text, DataFormat::Json, ".a.missing").is_err());
        assert!(query(text, DataFormat::Json, ".a.b[9]").is_err());
    }

    #[test]
    fn invalid_documents_are_reported() {
        assert!(parse_json("{").is_err());
        assert!(parse_xml("<a><b></a>").is_err());
        assert!(validate("a: [", DataFormat::Yaml).is_err());
    }

    #[test]
    fn file_conversion_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("data.json");
        std::fs::write(&input, r#"{"a":1}"#).unwrap();
        let output = convert_file(&input, dir.path(), DataFormat::Yaml, true, false).unwrap();
        assert!(output.exists());
        assert!(std::fs::read_to_string(output).unwrap().contains("a: 1"));
    }

    #[test]
    fn query_parser_handles_empty_segments() {
        assert_eq!(
            parse_query(".a..b[0]").unwrap(),
            vec![Step::Key("a".into()), Step::Key("b".into()), Step::Index(0)]
        );
    }
}
