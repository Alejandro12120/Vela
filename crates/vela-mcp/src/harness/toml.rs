use toml_edit::{Array, Document, Item, Table, Value, value};

use super::{Edit, EntryState, Problem, VelaCommand};

const GROUP: &str = "mcp_servers";

pub(super) struct Config<'a> {
    doc: Document<&'a str>,
}

impl<'a> Config<'a> {
    pub fn parse(source: &'a str) -> Result<Self, Problem> {
        let doc = Document::parse(source).map_err(|error| {
            Problem::Malformed(error.span().map_or_else(
                || "TOML inválido.".into(),
                |span| format!("TOML inválido cerca del byte {}.", span.start),
            ))
        })?;

        if let Some(group) = doc.get(GROUP) {
            let group = standard_table(group)?;
            if let Some(entry) = group.get("vela") {
                standard_table(entry)?;
            }
        }
        Ok(Self { doc })
    }

    pub fn entry(&self, cmd: &VelaCommand) -> EntryState {
        let Some(entry) = self.doc.get(GROUP).and_then(|group| group.get("vela")) else {
            return EntryState::NotInstalled;
        };
        let program = entry.get("command").and_then(Item::as_str);
        let args = entry.get("args").and_then(Item::as_array);
        let url = entry.get("url").map(|url| url.as_str().unwrap_or_default());
        cmd.classify(
            program,
            args.is_some_and(|args| {
                args.len() == 1 && args.get(0).and_then(toml_edit::Value::as_str) == Some("mcp")
            }) && url.is_none(),
            url,
        )
    }

    pub fn edit(self, edit: Edit<'_>) -> String {
        let source = self.doc.raw();
        let command = match edit {
            Edit::Install(command) => Some(command),
            Edit::Remove => None,
            Edit::UpdateProgram(command) => {
                let span = self.doc[GROUP]["vela"]["command"]
                    .span()
                    .expect("outdated entry has a command span");
                // The value's span excludes its decoration. Edit just the token
                // to preserve every other byte, including mixed line endings.
                let mut updated = source.to_owned();
                updated.replace_range(span, &Value::from(command.program_str()).to_string());
                return updated;
            }
        };
        let final_newline = source.is_empty() || source.ends_with('\n');
        let crlf = source.contains("\r\n")
            && source
                .split_inclusive('\n')
                .filter(|line| line.ends_with('\n'))
                .all(|line| line.ends_with("\r\n"));
        let mut doc = self.doc.into_mut();
        let group = doc
            .entry(GROUP)
            .or_insert_with(|| {
                let mut table = Table::new();
                table.set_implicit(true);
                Item::Table(table)
            })
            .as_table_mut()
            .expect("validated table");
        if let Some(command) = command {
            let mut table = Table::new();
            table["command"] = value(command.program_str());
            let mut args = Array::new();
            args.push("mcp");
            table["args"] = value(args);
            if let Some(old) = group.get("vela").and_then(Item::as_table) {
                table.decor_mut().clone_from(old.decor());
                table.set_position(old.position());
            }
            group.insert("vela", Item::Table(table));
        } else {
            group.remove("vela");
            if group.is_empty() {
                group.set_implicit(false);
            }
        }
        let mut edited = doc.to_string();
        if !final_newline && edited.ends_with('\n') {
            edited.pop();
        }
        if crlf {
            // toml_edit emits LF even when the entire source uses CRLF.
            edited = edited.replace('\n', "\r\n");
        }
        edited
    }
}

fn standard_table(item: &Item) -> Result<&Table, Problem> {
    if let Some(table) = item.as_table().filter(|table| !table.is_dotted()) {
        Ok(table)
    } else if item.is_inline_table() || item.is_table() {
        Err(Problem::Manual(
            "La configuración MCP usa una tabla inline o claves punteadas.".into(),
            false,
        ))
    } else {
        Err(Problem::Malformed(
            "Se esperaba una tabla MCP de Codex.".into(),
        ))
    }
}

pub(super) fn snippet(command: &VelaCommand) -> String {
    Config {
        doc: Document::new(),
    }
    .edit(Edit::Install(command))
}
