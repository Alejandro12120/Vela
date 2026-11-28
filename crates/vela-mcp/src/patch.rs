//! Edición quirúrgica de texto y generación de un diff unificado compacto.
//!
//! Todas las ediciones se validan antes de producir un resultado: una búsqueda
//! vacía, ausente o ambigua aborta la operación sin devolver contenido alguno,
//! de modo que una aprobación nunca puede aplicar un parche corrupto.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Una sustitución literal; `old_text` debe aparecer exactamente una vez.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextEdit {
    pub old_text: String,
    pub new_text: String,
}

/// Resultado de aplicar una lista de ediciones.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PatchResult {
    pub updated: String,
    pub unified_diff: String,
    pub hunks_applied: usize,
}

/// Fallo de validación de una edición.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PatchError {
    #[error("la búsqueda está vacía")]
    EmptySearch,
    #[error("el texto a sustituir no existe en el archivo")]
    NotFound { text: String },
    #[error("el texto a sustituir aparece {count} veces; la edición es ambigua")]
    AmbiguousMatch { text: String, count: usize },
}

/// Aplica las ediciones en orden sobre `original`.
pub fn apply_edits(original: &str, edits: &[TextEdit]) -> Result<PatchResult, PatchError> {
    let mut updated = original.to_owned();
    for edit in edits {
        if edit.old_text.is_empty() {
            return Err(PatchError::EmptySearch);
        }
        let count = updated.matches(&edit.old_text).count();
        if count == 0 {
            return Err(PatchError::NotFound {
                text: edit.old_text.clone(),
            });
        }
        if count > 1 {
            return Err(PatchError::AmbiguousMatch {
                text: edit.old_text.clone(),
                count,
            });
        }
        updated = updated.replacen(&edit.old_text, &edit.new_text, 1);
    }
    Ok(PatchResult {
        unified_diff: unified_diff(original, &updated),
        updated,
        hunks_applied: edits.len(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tag {
    Context,
    Added,
    Removed,
}

impl Tag {
    fn glyph(self) -> char {
        match self {
            Self::Context => ' ',
            Self::Added => '+',
            Self::Removed => '-',
        }
    }

    fn is_change(self) -> bool {
        self != Self::Context
    }
}

#[derive(Clone, Debug)]
struct Line {
    tag: Tag,
    text: String,
}

/// Máximo de celdas de la tabla LCS: por encima se usa un diff de reemplazo
/// completo, para no convertir un archivo grande en una matriz enorme.
const LCS_CELL_LIMIT: usize = 200_000;
const CONTEXT_LINES: usize = 3;

fn diff_lines(old: &str, new: &str) -> Vec<Line> {
    let before: Vec<&str> = old.lines().collect();
    let after: Vec<&str> = new.lines().collect();
    let cells = (before.len() + 1).saturating_mul(after.len() + 1);
    if cells > LCS_CELL_LIMIT {
        let mut lines = Vec::with_capacity(before.len() + after.len());
        lines.extend(before.into_iter().map(|text| Line {
            tag: Tag::Removed,
            text: text.to_owned(),
        }));
        lines.extend(after.into_iter().map(|text| Line {
            tag: Tag::Added,
            text: text.to_owned(),
        }));
        return lines;
    }

    let (n, m) = (before.len(), after.len());
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if before[i] == after[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut lines = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if before[i] == after[j] {
            lines.push(Line {
                tag: Tag::Context,
                text: before[i].to_owned(),
            });
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            lines.push(Line {
                tag: Tag::Removed,
                text: before[i].to_owned(),
            });
            i += 1;
        } else {
            lines.push(Line {
                tag: Tag::Added,
                text: after[j].to_owned(),
            });
            j += 1;
        }
    }
    while i < n {
        lines.push(Line {
            tag: Tag::Removed,
            text: before[i].to_owned(),
        });
        i += 1;
    }
    while j < m {
        lines.push(Line {
            tag: Tag::Added,
            text: after[j].to_owned(),
        });
        j += 1;
    }
    lines
}

/// Diff unificado con tres líneas de contexto, listo para la aprobación.
pub fn unified_diff(old: &str, new: &str) -> String {
    let lines = diff_lines(old, new);
    let mut changes = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.tag.is_change() {
            changes.push(index);
        }
    }
    if changes.is_empty() {
        return String::new();
    }

    let mut groups: Vec<(usize, usize)> = Vec::new();
    for index in changes {
        let start = index.saturating_sub(CONTEXT_LINES);
        let end = (index + CONTEXT_LINES + 1).min(lines.len());
        if let Some(last) = groups.last_mut()
            && start <= last.1
        {
            last.1 = end;
        } else {
            groups.push((start, end));
        }
    }

    let mut output = String::new();
    for (start, end) in groups {
        let mut old_start = 1usize;
        let mut new_start = 1usize;
        for line in &lines[..start] {
            match line.tag {
                Tag::Added => new_start += 1,
                Tag::Removed => old_start += 1,
                Tag::Context => {
                    old_start += 1;
                    new_start += 1;
                }
            }
        }
        let (old_count, new_count) =
            lines[start..end]
                .iter()
                .fold((0, 0), |(o, n), line| match line.tag {
                    Tag::Added => (o, n + 1),
                    Tag::Removed => (o + 1, n),
                    Tag::Context => (o + 1, n + 1),
                });
        output.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
        for line in &lines[start..end] {
            output.push(line.tag.glyph());
            output.push_str(&line.text);
            output.push('\n');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_multiple_edits_in_order_with_unified_diff() {
        let original = "uno\ndos\ntres\n";
        let result = apply_edits(
            original,
            &[
                TextEdit {
                    old_text: "dos".into(),
                    new_text: "DOS".into(),
                },
                TextEdit {
                    old_text: "tres".into(),
                    new_text: "TRES".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(result.updated, "uno\nDOS\nTRES\n");
        assert_eq!(result.hunks_applied, 2);
        assert!(result.unified_diff.contains("@@ -1,3 +1,3 @@"));
        assert!(result.unified_diff.contains("-dos"));
        assert!(result.unified_diff.contains("+DOS"));
        assert!(result.unified_diff.contains("-tres"));
        assert!(result.unified_diff.contains("+TRES"));
        assert!(result.unified_diff.contains(" uno\n"));
    }

    #[test]
    fn identical_text_produces_no_diff() {
        let result = apply_edits(
            "igual\n",
            &[TextEdit {
                old_text: "igual".into(),
                new_text: "igual".into(),
            }],
        )
        .unwrap();
        assert_eq!(result.unified_diff, "");
        assert_eq!(result.updated, "igual\n");
    }

    #[test]
    fn missing_search_is_rejected_without_output() {
        let error = apply_edits(
            "contenido\n",
            &[TextEdit {
                old_text: "ausente".into(),
                new_text: "new".into(),
            }],
        )
        .unwrap_err();
        assert_eq!(
            error,
            PatchError::NotFound {
                text: "ausente".into()
            }
        );
    }

    #[test]
    fn ambiguous_search_is_rejected() {
        let error = apply_edits(
            "repetido y repetido\n",
            &[TextEdit {
                old_text: "repetido".into(),
                new_text: "otro".into(),
            }],
        )
        .unwrap_err();
        assert_eq!(
            error,
            PatchError::AmbiguousMatch {
                text: "repetido".into(),
                count: 2
            }
        );
    }

    #[test]
    fn empty_search_is_rejected() {
        let error = apply_edits(
            "texto",
            &[TextEdit {
                old_text: String::new(),
                new_text: "nuevo".into(),
            }],
        )
        .unwrap_err();
        assert_eq!(error, PatchError::EmptySearch);
    }

    #[test]
    fn separate_changes_produce_separate_hunks() {
        let old = (0..20)
            .map(|index| format!("linea {index}\n"))
            .collect::<String>();
        let new = old
            .replace("linea 1\n", "LINEA 1\n")
            .replace("linea 18\n", "LINEA 18\n");
        let diff = unified_diff(&old, &new);
        assert_eq!(diff.matches("@@ ").count(), 2);
        assert!(diff.contains("+LINEA 1\n"));
        assert!(diff.contains("+LINEA 18\n"));
    }
}
