use std::{collections::HashMap, sync::LazyLock};

use rmcp::schemars::JsonSchema;
use serde::Deserialize;

const COMMAND_PREFIX: &str = "windbg_cmd_";
const PROMPT_PREFIX: &str = "windbg_prompt_";
const RESOURCE_SCHEME: &str = "windbg://command/";
const INDEX_URI: &str = "windbg://catalog/index";
const TEMPLATE_URI: &str = "windbg://command/{id}";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSection {
    Command,
    MetaCommand,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub section: CatalogSection,
    pub title: String,
    pub topic_path: String,
    pub summary: String,
    pub tokens: Vec<String>,
    pub supports_text_execution: bool,
    pub user_mode_syntax: Option<String>,
    pub kernel_mode_syntax: Option<String>,
    pub documentation: String,
}

impl CatalogEntry {
    pub fn tool_name(&self) -> String {
        format!("{COMMAND_PREFIX}{}", self.id)
    }

    pub fn prompt_name(&self) -> String {
        format!("{PROMPT_PREFIX}{}", self.id)
    }

    pub fn resource_uri(&self) -> String {
        format!("{RESOURCE_SCHEME}{}", self.id)
    }

    pub fn primary_token(&self) -> &str {
        self.tokens.first().map(String::as_str).unwrap_or("")
    }

    pub fn variants_required(&self) -> bool {
        self.tokens.len() > 1
    }

    pub fn short_label(&self) -> String {
        format!("{} - {}", self.primary_token(), self.summary)
    }
}

#[derive(Debug)]
pub struct Catalog {
    entries: Vec<CatalogEntry>,
    by_id: HashMap<String, usize>,
    by_tool_name: HashMap<String, usize>,
    by_prompt_name: HashMap<String, usize>,
}

impl Catalog {
    pub fn load() -> Self {
        let entries: Vec<CatalogEntry> = serde_json::from_str(include_str!("command_catalog.json"))
            .expect("embedded command catalog must be valid JSON");
        let mut by_id = HashMap::with_capacity(entries.len());
        let mut by_tool_name = HashMap::with_capacity(entries.len());
        let mut by_prompt_name = HashMap::with_capacity(entries.len());

        for (index, entry) in entries.iter().enumerate() {
            by_id.insert(entry.id.clone(), index);
            by_tool_name.insert(entry.tool_name(), index);
            by_prompt_name.insert(entry.prompt_name(), index);
        }

        Self {
            entries,
            by_id,
            by_tool_name,
            by_prompt_name,
        }
    }

    pub fn global() -> &'static Self {
        static CATALOG: LazyLock<Catalog> = LazyLock::new(Catalog::load);
        &CATALOG
    }

    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn command_index_uri(&self) -> &'static str {
        INDEX_URI
    }

    pub fn command_template_uri(&self) -> &'static str {
        TEMPLATE_URI
    }

    pub fn get_by_id(&self, id: &str) -> Option<&CatalogEntry> {
        self.by_id.get(id).map(|index| &self.entries[*index])
    }

    pub fn get_by_tool_name(&self, name: &str) -> Option<&CatalogEntry> {
        self.by_tool_name
            .get(name)
            .map(|index| &self.entries[*index])
    }

    pub fn get_by_prompt_name(&self, name: &str) -> Option<&CatalogEntry> {
        self.by_prompt_name
            .get(name)
            .map(|index| &self.entries[*index])
    }

    pub fn get_by_resource_uri(&self, uri: &str) -> Option<&CatalogEntry> {
        uri.strip_prefix(RESOURCE_SCHEME)
            .and_then(|id| self.get_by_id(id))
    }

    pub fn find_by_token(&self, token: &str) -> Vec<&CatalogEntry> {
        let needle = token.trim().to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|entry| {
                entry
                    .tokens
                    .iter()
                    .any(|item| item.eq_ignore_ascii_case(&needle))
            })
            .collect()
    }

    pub fn lookup(&self, query: &str) -> Option<&CatalogEntry> {
        self.get_by_id(query).or_else(|| {
            let mut matches = self.find_by_token(query);
            if matches.len() == 1 {
                matches.pop()
            } else {
                None
            }
        })
    }

    pub fn search<'a>(
        &'a self,
        query: &str,
        section: Option<CatalogSection>,
        limit: usize,
    ) -> Vec<&'a CatalogEntry> {
        let needle = query.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return self
                .entries
                .iter()
                .filter(|entry| section.is_none_or(|value| value == entry.section))
                .take(limit)
                .collect();
        }

        let mut scored: Vec<(i32, &CatalogEntry)> = self
            .entries
            .iter()
            .filter(|entry| section.is_none_or(|value| value == entry.section))
            .filter_map(|entry| {
                let mut score = 0;
                if entry.id.eq_ignore_ascii_case(&needle) {
                    score += 100;
                }
                if entry.title.to_ascii_lowercase().contains(&needle) {
                    score += 40;
                }
                if entry.summary.to_ascii_lowercase().contains(&needle) {
                    score += 25;
                }
                if entry
                    .tokens
                    .iter()
                    .any(|token| token.eq_ignore_ascii_case(&needle))
                {
                    score += 80;
                } else if entry
                    .tokens
                    .iter()
                    .any(|token| token.to_ascii_lowercase().contains(&needle))
                {
                    score += 30;
                }
                (score > 0).then_some((score, entry))
            })
            .collect();

        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.id.cmp(&right.1.id))
        });
        scored
            .into_iter()
            .take(limit)
            .map(|(_, entry)| entry)
            .collect()
    }

    pub fn render_index(&self) -> String {
        let mut output = String::new();
        output.push_str("WinDbg command catalog extracted from debugger.chm\n\n");
        output.push_str(&format!("Total entries: {}\n\n", self.len()));

        for section in [CatalogSection::Command, CatalogSection::MetaCommand] {
            let heading = match section {
                CatalogSection::Command => "Commands",
                CatalogSection::MetaCommand => "Meta-Commands",
            };
            output.push_str(heading);
            output.push('\n');
            output.push_str(&"=".repeat(heading.len()));
            output.push_str("\n\n");

            for entry in self.entries.iter().filter(|entry| entry.section == section) {
                output.push_str(&format!("- {}\n", entry.short_label()));
                output.push_str(&format!("  id: {}\n", entry.id));
                output.push_str(&format!("  resource: {}\n", entry.resource_uri()));
                output.push_str(&format!("  tool: {}\n", entry.tool_name()));
                output.push_str(&format!("  prompt: {}\n\n", entry.prompt_name()));
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_dt_entry() {
        let catalog = Catalog::global();
        let dt = catalog.lookup("dt").expect("dt should exist in catalog");
        assert_eq!(dt.primary_token(), "dt");
        assert!(dt.documentation.contains("Display Type"));
    }

    #[test]
    fn catalog_search_prefers_exact_token() {
        let catalog = Catalog::global();
        let results = catalog.search("dt", None, 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].primary_token(), "dt");
    }
}
