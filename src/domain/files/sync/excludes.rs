use crate::domain::files::matcher::bracket_negation;

pub fn sync_excludes(patterns: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in patterns {
        let pattern = raw.trim();
        let (anchored, rest) = match pattern.strip_prefix('/') {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let mut rest = rest.trim_end_matches('/');
        if !anchored {
            while let Some(stripped) = rest.strip_prefix("**/") {
                rest = stripped;
            }
        }
        if rest.is_empty() {
            continue;
        }
        let rest = bracket_negation(rest);
        let rest = collapse_double_stars(&rest);
        let full = if anchored { format!("/{rest}") } else { rest };
        for variant in zero_dir_variants(&full) {
            out.push(variant.clone());
            out.push(format!("{variant}/**"));
        }
    }
    out
}

fn collapse_double_stars(pattern: &str) -> String {
    let mut result = pattern.to_string();
    while result.contains("/**/**/") {
        result = result.replace("/**/**/", "/**/");
    }
    result
}

fn zero_dir_variants(pattern: &str) -> Vec<String> {
    let parts: Vec<&str> = pattern.split("/**/").collect();
    let gaps = parts.len() - 1;
    if gaps > 4 {
        return vec![pattern.to_string(), pattern.replace("/**/", "/")];
    }
    (0..1u32 << gaps)
        .map(|mask| {
            let mut variant = parts[0].to_string();
            for (i, part) in parts[1..].iter().enumerate() {
                variant.push_str(if mask & (1 << i) == 0 { "/**/" } else { "/" });
                variant.push_str(part);
            }
            variant
        })
        .collect()
}
