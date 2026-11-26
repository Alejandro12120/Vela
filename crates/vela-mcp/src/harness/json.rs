use std::ops::Range;

use serde::{Serialize, de::IgnoredAny};
use serde_json::{Value, json, ser::PrettyFormatter};

use super::{EntryState, HarnessId, Problem, VelaCommand};

const SERVERS: &str = "mcpServers";
const MODERN: &[&str] = &["mcp", "servers"];
const LEGACY: &[&str] = &["mcp"];

pub(super) struct Config<'a> {
    source: &'a str,
    id: HarnessId,
    legacy: bool,
    container: Option<Object>,
    missing: &'static [&'static str],
}

impl<'a> Config<'a> {
    pub fn parse(id: HarnessId, source: &'a str, jsonc: bool) -> Result<Self, Problem> {
        if jsonc
            || (id == HarnessId::OpenCode
                && !source.trim().is_empty()
                && serde_json::from_str::<IgnoredAny>(source).is_err())
        {
            let stripped = strip_jsonc(source);
            let config = Config::strict(id, stripped.as_deref().unwrap_or(source))?;
            return Err(Problem::Manual(
                "OpenCode usa JSONC. Edita el archivo a mano.".into(),
                config.legacy,
            ));
        }
        Self::strict(id, source)
    }

    fn strict(id: HarnessId, source: &'a str) -> Result<Self, Problem> {
        let root = if source.trim().is_empty() {
            None
        } else {
            serde_json::from_str::<IgnoredAny>(source).map_err(malformed)?;
            Some(Object::parse(source, skip_space(source, 0))?)
        };
        let path = if id == HarnessId::OpenCode {
            MODERN
        } else {
            &[SERVERS]
        };
        let mut config = Self {
            source,
            id,
            legacy: false,
            container: root,
            missing: path,
        };
        while let [key, rest @ ..] = config.missing {
            let Some(value) = config
                .container
                .as_ref()
                .map(|object| object.find(key))
                .transpose()?
                .flatten()
            else {
                break;
            };
            let child = Object::parse(source, value.value.start)?;
            if id == HarnessId::OpenCode
                && *key == "mcp"
                && child.find("servers")?.is_some()
                && child.find("vela")?.is_some()
            {
                return Err(Problem::Manual(
                    "Hay una entrada vela de OpenCode v1 junto a mcp.servers.".into(),
                    false,
                ));
            }
            if id == HarnessId::OpenCode
                && *key == "mcp"
                && child.find("servers")?.is_none()
                && child.members.iter().any(|member| {
                    serde_json::from_str::<Value>(&source[member.value.clone()])
                        .is_ok_and(|value| value.get("type").is_some_and(Value::is_string))
                })
            {
                config.legacy = true;
                config.missing = &[];
            } else {
                config.missing = rest;
            }
            config.container = Some(child);
        }
        if config.missing.is_empty() {
            config.member()?;
        }
        Ok(config)
    }

    fn member(&self) -> Result<Option<&Member>, Problem> {
        if self.missing.is_empty()
            && let Some(container) = &self.container
        {
            return container.find("vela");
        }
        Ok(None)
    }

    pub fn entry(&self, cmd: &VelaCommand) -> EntryState {
        let Some(member) = self.member().expect("validated entry") else {
            return EntryState::NotInstalled;
        };
        let Ok(value) = serde_json::from_str::<Value>(&self.source[member.value.clone()]) else {
            return cmd.classify(None, false);
        };
        let (program, args_match) = if self.id == HarnessId::OpenCode {
            let command = value.get("command").and_then(Value::as_array);
            (
                command
                    .and_then(|items| items.first())
                    .and_then(Value::as_str),
                value.get("type").and_then(Value::as_str) == Some("local")
                    && command.is_some_and(|items| items.len() == 2 && items[1] == "mcp"),
            )
        } else {
            (
                value.get("command").and_then(Value::as_str),
                value.get("type").is_none_or(|kind| kind == "stdio")
                    && ["url", "serverUrl", "httpUrl"]
                        .iter()
                        .all(|key| value.get(key).is_none())
                    && value
                        .get("args")
                        .and_then(Value::as_array)
                        .is_some_and(|items| items.len() == 1 && items[0] == "mcp"),
            )
        };
        cmd.classify(program, args_match)
    }

    pub fn edit(&self, command: Option<&VelaCommand>) -> String {
        let member = self.member().expect("validated entry");
        let Some(command) = command else {
            let member = member.expect("removal requires an entry");
            let container = self.container.as_ref().unwrap();
            let index = container
                .members
                .iter()
                .position(|item| item.start == member.start)
                .unwrap();
            let remove = if index > 0 {
                container.members[index - 1].value.end..member.value.end
            } else if let Some(next) = container.members.get(1) {
                member.start..next.start
            } else {
                container.start + 1..member.value.end
            };
            return replace(self.source, remove, "");
        };

        let format = Format::detect(self.source);
        let entry = entry(self.id, self.legacy, command);
        if let Some(member) = member {
            let base = line_indent(self.source, member.start);
            return replace(
                self.source,
                member.value.clone(),
                &format.value(&entry, base),
            );
        }

        let (key, rest, value) = match self.missing {
            [key, rest @ ..] => (*key, rest, json!({"vela": entry})),
            [] => ("vela", &[][..], entry),
        };
        let value = rest
            .iter()
            .rev()
            .fold(value, |inner, key| json!({*key: inner}));
        let Some(container) = &self.container else {
            return format!(
                "{}{}{}",
                self.source,
                format.value(&json!({key: value}), ""),
                format.newline
            );
        };
        let format = Format {
            compact: !self.source[container.start..container.end].contains('\n'),
            ..format
        };
        let base = container.members.last().map_or_else(
            || {
                format!(
                    "{}{}",
                    line_indent(self.source, container.start),
                    format.indent
                )
            },
            |last| line_indent(self.source, last.start).to_owned(),
        );
        let colon = if format.compact { ":" } else { ": " };
        let insertion = format!("{}{}{}", json!(key), colon, format.value(&value, &base));

        let mut delta = String::new();
        let position = if let Some(last) = container.members.last() {
            delta.push(',');
            last.value.end
        } else {
            container.start + 1
        };
        if !format.compact {
            delta.push_str(format.newline);
            delta.push_str(&base);
        }
        delta.push_str(&insertion);
        replace(self.source, position..position, &delta)
    }
}

fn entry(id: HarnessId, legacy: bool, command: &VelaCommand) -> Value {
    let program = command.program_str();
    match id {
        HarnessId::ClaudeCode => {
            json!({"type": "stdio", "command": program, "args": ["mcp"], "env": {}})
        }
        HarnessId::Cursor => json!({"type": "stdio", "command": program, "args": ["mcp"]}),
        HarnessId::Antigravity => json!({"command": program, "args": ["mcp"]}),
        HarnessId::OpenCode if legacy => {
            json!({"type": "local", "command": [program, "mcp"], "enabled": true})
        }
        HarnessId::OpenCode => json!({"type": "local", "command": [program, "mcp"]}),
        HarnessId::Codex => unreachable!("Codex uses TOML"),
    }
}

pub(super) fn snippet(id: HarnessId, legacy: bool, command: &VelaCommand) -> String {
    let path = match id {
        HarnessId::OpenCode if legacy => LEGACY,
        HarnessId::OpenCode => MODERN,
        _ => &[SERVERS],
    };
    Config {
        source: "",
        id,
        legacy,
        container: None,
        missing: path,
    }
    .edit(Some(command))
}

struct Object {
    start: usize,
    end: usize,
    members: Vec<Member>,
}

struct Member {
    key: String,
    start: usize,
    value: Range<usize>,
}

impl Object {
    // General grammar validation belongs to serde_json. Only index the members in the
    // objects Vela must inspect so unrelated values can be copied verbatim.
    fn parse(source: &str, start: usize) -> Result<Self, Problem> {
        let bytes = source.as_bytes();
        if bytes.get(start) != Some(&b'{') {
            return Err(Problem::Malformed(
                "Se esperaba un objeto JSON para la configuración MCP.".into(),
            ));
        }
        let mut members = Vec::new();
        let mut position = skip_space(source, start + 1);
        while bytes[position] != b'}' {
            let mut key =
                serde_json::Deserializer::from_str(&source[position..]).into_iter::<String>();
            let name = key.next().expect("validated JSON key").map_err(malformed)?;
            let colon = skip_space(source, position + key.byte_offset());
            let value_start = skip_space(source, colon + 1);
            let mut value = serde_json::Deserializer::from_str(&source[value_start..])
                .into_iter::<IgnoredAny>();
            value
                .next()
                .expect("validated JSON value")
                .map_err(malformed)?;
            let end = value_start + value.byte_offset();
            members.push(Member {
                key: name,
                start: position,
                value: value_start..end,
            });
            position = skip_space(source, end);
            if bytes[position] == b',' {
                position = skip_space(source, position + 1);
            }
        }
        Ok(Self {
            start,
            end: position + 1,
            members,
        })
    }

    fn find(&self, name: &str) -> Result<Option<&Member>, Problem> {
        let mut matches = self.members.iter().filter(|member| member.key == name);
        let found = matches.next();
        if matches.next().is_some() {
            return Err(Problem::Malformed(
                "Hay claves duplicadas en la ruta MCP de Vela.".into(),
            ));
        }
        Ok(found)
    }
}

fn skip_space(source: &str, start: usize) -> usize {
    start
        + source.as_bytes()[start..]
            .iter()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count()
}

fn replace(source: &str, range: Range<usize>, replacement: &str) -> String {
    let mut result = source.to_owned();
    result.replace_range(range, replacement);
    result
}

fn line_indent(source: &str, position: usize) -> &str {
    let line_start = source[..position].rfind('\n').map_or(0, |index| index + 1);
    let end = line_start
        + source.as_bytes()[line_start..position]
            .iter()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
    &source[line_start..end]
}

struct Format<'a> {
    indent: &'a str,
    newline: &'static str,
    compact: bool,
}

impl<'a> Format<'a> {
    fn detect(source: &'a str) -> Self {
        let indent = source
            .split_inclusive('\n')
            .filter_map(|line| {
                let offset = line
                    .bytes()
                    .take_while(|byte| matches!(byte, b' ' | b'\t'))
                    .count();
                (offset != 0 && line[offset..].starts_with('"')).then_some(&line[..offset])
            })
            .min_by_key(|indent| indent.len())
            .unwrap_or("  ");
        Self {
            indent,
            newline: if source.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            },
            compact: !source.trim().is_empty() && !source.trim().contains('\n'),
        }
    }

    fn value(&self, value: &Value, base: &str) -> String {
        if self.compact {
            return value.to_string();
        }
        let mut output = Vec::new();
        let formatter = PrettyFormatter::with_indent(self.indent.as_bytes());
        value
            .serialize(&mut serde_json::Serializer::with_formatter(
                &mut output,
                formatter,
            ))
            .expect("serializing JSON value");
        String::from_utf8(output)
            .expect("JSON is UTF-8")
            .replace('\n', &format!("{}{base}", self.newline))
    }
}

fn malformed(error: serde_json::Error) -> Problem {
    Problem::Malformed(format!(
        "JSON inválido en la línea {}, columna {}.",
        error.line(),
        error.column()
    ))
}

fn strip_jsonc(source: &str) -> Option<String> {
    // Replace comments with spaces, respecting quoted strings, then remove trailing commas.
    let mut bytes = source.as_bytes().to_vec();
    let mut pos = 0;
    let mut previous = None;
    let mut comma = None;
    while pos < bytes.len() {
        match bytes[pos] {
            b'"' => {
                pos += 1;
                while pos < bytes.len() && bytes[pos] != b'"' {
                    pos += if bytes[pos] == b'\\' { 2 } else { 1 };
                }
                previous = Some(b'"');
            }
            b'/' if bytes.get(pos + 1) == Some(&b'/') => {
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    bytes[pos] = b' ';
                    pos += 1;
                }
                continue;
            }
            b'/' if bytes.get(pos + 1) == Some(&b'*') => {
                let end = source[pos + 2..].find("*/")? + pos + 4;
                for byte in &mut bytes[pos..end] {
                    if !matches!(byte, b'\n' | b'\r') {
                        *byte = b' ';
                    }
                }
                pos = end;
                continue;
            }
            b',' => {
                comma = previous
                    .filter(|byte| !matches!(byte, b'{' | b'[' | b':' | b','))
                    .map(|_| pos);
                previous = Some(b',');
            }
            byte if byte.is_ascii_whitespace() => (),
            byte => {
                if matches!(byte, b'}' | b']')
                    && previous == Some(b',')
                    && let Some(comma) = comma
                {
                    bytes[comma] = b' ';
                }
                previous = Some(byte);
            }
        }
        pos += 1;
    }
    String::from_utf8(bytes).ok()
}
