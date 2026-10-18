use crate::services::config::DatabaseConfig;
use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};

pub struct ExcludeMatcher {
    set: GlobSet,
    literal: Vec<PathBuf>,
}

impl ExcludeMatcher {
    pub fn new(patterns: &[String]) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for raw in patterns {
            let pattern = raw.trim();
            if pattern.is_empty() {
                continue;
            }
            let glob = match pattern.strip_prefix('/') {
                Some(anchored) => anchored.trim_end_matches('/').to_string(),
                None => format!("**/{}", pattern.trim_end_matches('/')),
            };
            builder.add(
                GlobBuilder::new(&glob)
                    .literal_separator(true)
                    .build()
                    .with_context(|| format!("invalid exclude pattern '{pattern}'"))?,
            );
        }
        Ok(Self {
            set: builder.build()?,
            literal: Vec::new(),
        })
    }

    pub fn from_config(cfg: &DatabaseConfig) -> Result<Self> {
        Self::new(&exclude_patterns(cfg))
    }

    pub fn exclude_path(&mut self, relative: PathBuf) {
        self.literal.push(relative);
    }

    pub fn is_excluded(&self, relative: &Path) -> bool {
        self.set.is_match(relative) || self.literal.iter().any(|p| p == relative)
    }
}

pub fn exclude_patterns(cfg: &DatabaseConfig) -> Vec<String> {
    cfg.options
        .get("exclude")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub fn one_file_system(cfg: &DatabaseConfig) -> bool {
    cfg.options
        .get("one_file_system")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub fn source_root(cfg: &DatabaseConfig) -> Result<PathBuf> {
    let root = std::fs::canonicalize(&cfg.path)
        .with_context(|| format!("cannot read source directory {}", cfg.path))?;
    if !root.is_dir() {
        bail!("source path {} is not a directory", root.display());
    }
    Ok(root)
}

pub(crate) fn bracket_negation(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        out.push(c);
        match c {
            '\\' => out.extend(chars.next()),
            '[' if !in_class => {
                in_class = true;
                if chars.as_str().starts_with('!') {
                    chars.next();
                    out.push('^');
                }
            }
            ']' => in_class = false,
            _ => {}
        }
    }
    out
}
