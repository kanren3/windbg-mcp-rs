use std::{collections::HashMap, sync::LazyLock};

use rmcp::schemars::JsonSchema;
use serde::Deserialize;

const RESOURCE_SCHEME: &str = "windbg://command/";
const FULL_RESOURCE_SCHEME: &str = "windbg://command-full/";
const INDEX_URI: &str = "windbg://catalog/index";
const TEMPLATE_URI: &str = "windbg://command/{id}";
const FULL_TEMPLATE_URI: &str = "windbg://command-full/{id}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogResourceKind {
    Compact,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRouting {
    ExecuteCommand,
    InterruptTarget,
    DocumentationOnly,
}

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
    pub summary: String,
    pub tokens: Vec<String>,
    pub supports_text_execution: bool,
    pub user_mode_syntax: Option<String>,
    pub kernel_mode_syntax: Option<String>,
    pub documentation: String,
}

impl CatalogEntry {
    pub fn resource_uri(&self) -> String {
        format!("{RESOURCE_SCHEME}{}", self.id)
    }

    pub fn full_resource_uri(&self) -> String {
        format!("{FULL_RESOURCE_SCHEME}{}", self.id)
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

    pub fn syntax_preview(&self) -> Option<&str> {
        self.user_mode_syntax
            .as_deref()
            .or(self.kernel_mode_syntax.as_deref())
    }

    pub fn syntax_block(&self) -> Option<String> {
        format_structured_syntax(
            self.user_mode_syntax.as_deref(),
            self.kernel_mode_syntax.as_deref(),
        )
        .or_else(|| infer_syntax_block(&self.documentation))
    }

    pub fn tool_routing(&self) -> ToolRouting {
        if self.supports_text_execution {
            return ToolRouting::ExecuteCommand;
        }

        if self
            .tokens
            .iter()
            .any(|token| token.eq_ignore_ascii_case("CTRL+C"))
        {
            return ToolRouting::InterruptTarget;
        }

        ToolRouting::DocumentationOnly
    }

    pub fn recommended_tool(&self) -> Option<&'static str> {
        match self.tool_routing() {
            ToolRouting::ExecuteCommand => Some("windbg_execute_command"),
            ToolRouting::InterruptTarget => Some("windbg_interrupt_target"),
            ToolRouting::DocumentationOnly => None,
        }
    }

    pub fn tool_routing_name(&self) -> &'static str {
        match self.tool_routing() {
            ToolRouting::ExecuteCommand => "execute_command",
            ToolRouting::InterruptTarget => "interrupt_target",
            ToolRouting::DocumentationOnly => "documentation_only",
        }
    }
}

#[derive(Debug)]
pub struct Catalog {
    entries: Vec<CatalogEntry>,
    by_id: HashMap<String, usize>,
}

impl Catalog {
    pub fn load() -> Self {
        let entries: Vec<CatalogEntry> = serde_json::from_str(include_str!("catalog.json"))
            .expect("embedded command catalog must be valid JSON");
        let mut by_id = HashMap::with_capacity(entries.len());

        for (index, entry) in entries.iter().enumerate() {
            by_id.insert(entry.id.clone(), index);
        }

        Self { entries, by_id }
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

    pub fn full_command_template_uri(&self) -> &'static str {
        FULL_TEMPLATE_URI
    }

    pub fn get_by_id(&self, id: &str) -> Option<&CatalogEntry> {
        self.by_id.get(id).map(|index| &self.entries[*index])
    }

    pub fn get_by_resource_uri(&self, uri: &str) -> Option<&CatalogEntry> {
        uri.strip_prefix(RESOURCE_SCHEME)
            .and_then(|id| self.get_by_id(id))
    }

    pub fn resolve_resource_uri(&self, uri: &str) -> Option<(CatalogResourceKind, &CatalogEntry)> {
        if let Some(entry) = uri
            .strip_prefix(RESOURCE_SCHEME)
            .and_then(|id| self.get_by_id(id))
        {
            return Some((CatalogResourceKind::Compact, entry));
        }

        uri.strip_prefix(FULL_RESOURCE_SCHEME)
            .and_then(|id| self.get_by_id(id))
            .map(|entry| (CatalogResourceKind::Full, entry))
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

        let terms = search_terms(query);

        let mut scored: Vec<(i32, usize, &CatalogEntry)> = self
            .entries
            .iter()
            .filter(|entry| section.is_none_or(|value| value == entry.section))
            .filter_map(|entry| {
                let (score, matched_terms) = score_entry(entry, &needle, &terms);
                (score > 0).then_some((score, matched_terms, entry))
            })
            .collect();

        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.id.cmp(&right.2.id))
        });
        scored
            .into_iter()
            .take(limit)
            .map(|(_, _, entry)| entry)
            .collect()
    }

    pub fn render_index(&self) -> String {
        let mut output = String::new();
        let command_count = self
            .entries
            .iter()
            .filter(|entry| entry.section == CatalogSection::Command)
            .count();
        let meta_count = self.len().saturating_sub(command_count);

        output.push_str("WinDbg MCP guide\n\n");
        output.push_str("Recommended flow:\n");
        output.push_str("1. Call `windbg_search_catalog` with the user request.\n");
        output.push_str("2. Read `windbg://command/{id}` for the best match; it is optimized for low context.\n");
        output.push_str(
            "3. Read `windbg://command-full/{id}` only when the compact card is insufficient.\n",
        );
        output.push_str("4. Call `windbg_get_state` before execution.\n");
        output.push_str("5. If the debugger is running or busy, call `windbg_interrupt_target` and then verify state again.\n");
        output.push_str(
            "6. Call `windbg_execute_command` only when the debugger is ready for commands.\n\n",
        );
        output.push_str(&format!("Total entries: {}\n", self.len()));
        output.push_str(&format!("Commands: {}\n", command_count));
        output.push_str(&format!("Meta-commands: {}\n", meta_count));
        output.push_str("Execution state tool: windbg_get_state\n");
        output.push_str(&format!("Compact template: {}\n", TEMPLATE_URI));
        output.push_str(&format!("Full template: {}\n\n", FULL_TEMPLATE_URI));

        output
    }
}

fn format_structured_syntax(user_mode: Option<&str>, kernel_mode: Option<&str>) -> Option<String> {
    let user_mode = cleaned_block(user_mode);
    let kernel_mode = cleaned_block(kernel_mode);
    if user_mode.is_none() && kernel_mode.is_none() {
        return None;
    }

    let mut output = String::new();
    if let Some(user_mode) = user_mode {
        output.push_str("User-Mode Syntax\n");
        output.push_str(&user_mode);
    }

    if let Some(kernel_mode) = kernel_mode {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str("Kernel-Mode Syntax\n");
        output.push_str(&kernel_mode);
    }

    Some(output)
}

fn infer_syntax_block(documentation: &str) -> Option<String> {
    let lines: Vec<&str> = documentation.lines().collect();
    if lines.is_empty() {
        return None;
    }

    let mut index = 0;

    while index < lines.len() && lines[index].trim().is_empty() {
        index += 1;
    }
    while index < lines.len() && !lines[index].trim().is_empty() {
        index += 1;
    }
    while index < lines.len() && lines[index].trim().is_empty() {
        index += 1;
    }
    while index < lines.len() && !lines[index].trim().is_empty() {
        index += 1;
    }
    while index < lines.len() && lines[index].trim().is_empty() {
        index += 1;
    }

    let start = index;
    while index < lines.len() {
        let line = lines[index].trim();
        if is_syntax_section_boundary(line) {
            break;
        }
        index += 1;
    }

    cleaned_block(Some(&lines[start..index].join("\n")))
}

fn cleaned_block(block: Option<&str>) -> Option<String> {
    let block = block?.trim();
    if block.is_empty() {
        return None;
    }

    let lines: Vec<&str> = block.lines().collect();
    let start = lines.iter().position(|line| !line.trim().is_empty())?;
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .unwrap_or(start);

    let mut output = String::new();
    let mut last_blank = false;
    for line in &lines[start..=end] {
        let trimmed_end = line.trim_end();
        if trimmed_end.trim().is_empty() {
            if !last_blank {
                output.push('\n');
            }
            last_blank = true;
            continue;
        }

        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(trimmed_end.trim_start());
        last_blank = false;
    }

    Some(output)
}

fn is_syntax_section_boundary(line: &str) -> bool {
    matches!(
        line,
        "Parameters"
            | "Parameter"
            | "Environment"
            | "Remarks"
            | "Remark"
            | "Additional Information"
            | "Examples"
            | "Example"
            | "Targets"
            | "Platforms"
            | "Modes"
            | "Note"
            | "Notes"
    )
}

fn search_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter_map(|term| normalize_search_term(term))
        .collect()
}

fn normalize_search_term(term: &str) -> Option<String> {
    let trimmed = term
        .trim_matches(|ch: char| {
            !ch.is_ascii_alphanumeric()
                && ch != '.'
                && ch != '_'
                && ch != '$'
                && ch != '?'
                && ch != '!'
                && ch != '<'
                && ch != '>'
        })
        .to_ascii_lowercase();

    (!trimmed.is_empty()).then_some(trimmed)
}

fn score_entry(entry: &CatalogEntry, needle: &str, terms: &[String]) -> (i32, usize) {
    let id = entry.id.to_ascii_lowercase();
    let title = entry.title.to_ascii_lowercase();
    let summary = entry.summary.to_ascii_lowercase();
    let syntax = entry
        .syntax_block()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let tokens: Vec<String> = entry
        .tokens
        .iter()
        .map(|token| token.to_ascii_lowercase())
        .collect();

    let mut score = 0;
    let mut matched_terms = 0;

    if id == needle {
        score += 180;
    }
    if tokens.iter().any(|token| token == needle) {
        score += 160;
    }
    if title.contains(needle) {
        score += 80;
    }
    if summary.contains(needle) {
        score += 50;
    }
    if !syntax.is_empty() && syntax.contains(needle) {
        score += 45;
    }

    for term in terms {
        let mut matched = false;

        if id == *term {
            score += 70;
            matched = true;
        } else if id.contains(term) {
            score += 18;
            matched = true;
        }

        if tokens.iter().any(|token| token == term) {
            score += 90;
            matched = true;
        } else if tokens.iter().any(|token| token.contains(term)) {
            score += 30;
            matched = true;
        }

        if title.contains(term) {
            score += 28;
            matched = true;
        }
        if summary.contains(term) {
            score += 20;
            matched = true;
        }
        if !syntax.is_empty() && syntax.contains(term) {
            score += 18;
            matched = true;
        }

        if matched {
            matched_terms += 1;
        }
    }

    if matched_terms > 1 {
        score += (matched_terms as i32) * 8;
    }

    (score, matched_terms)
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

    #[test]
    fn catalog_search_handles_natural_language_query() {
        let catalog = Catalog::global();
        let results = catalog.search("display type structure dt command", None, 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].primary_token(), "dt");
    }

    #[test]
    fn catalog_ids_are_normalized() {
        let catalog = Catalog::global();
        let entry = catalog.lookup("dt").expect("dt should exist in catalog");
        assert_eq!(entry.id, "dt_display_type");
        assert!(
            !catalog
                .entries()
                .iter()
                .any(|entry| entry.id.contains("__"))
        );
    }

    #[test]
    fn catalog_exposes_full_resource_uri() {
        let entry = Catalog::global()
            .lookup("dt")
            .expect("dt should exist in catalog");
        assert_eq!(
            entry.full_resource_uri(),
            "windbg://command-full/dt_display_type"
        );
    }

    #[test]
    fn catalog_infers_syntax_block_from_documentation() {
        let entry = Catalog::global()
            .lookup("bp")
            .expect("bp entry should exist in catalog");
        let syntax = entry.syntax_block().expect("bp syntax should be inferred");
        assert!(syntax.contains("User-Mode"));
        assert!(syntax.contains("bp[ID] [Options] [Address [Passes]] [\"CommandString\"]"));
        assert!(syntax.contains("Kernel-Mode"));
        assert!(!syntax.contains("Parameters"));
    }

    #[test]
    fn catalog_index_prefers_search_driven_resource_flow() {
        let index = Catalog::global().render_index();
        assert!(index.contains("windbg_search_catalog"));
        assert!(index.contains("windbg://command/{id}"));
        assert!(index.contains("windbg://command-full/{id}"));
        assert!(!index.contains("dt_display_type"));
    }
}
