//! Every `POST` route must take a request body.
//!
//! # Why this is a test and not a comment
//!
//! The CSRF check lives in the form extractor (`csrf::CsrfForm`), and the route
//! macro only runs an extractor for a handler that declares a **body
//! parameter**. A `POST` handler written without one compiles, works, and
//! silently accepts a token-less submission.
//!
//! That is not hypothetical: the AI briefing route was written as
//! `async fn generate_briefing(cx: &Cx)` because it needs no fields — the
//! contact is in the path — and a cross-site page could therefore have made the
//! app spend the operator's API credit. It was found by comparing a token-less
//! `POST` against the other routes, which is precisely the check this test
//! automates.
//!
//! Source-level, because the property is about the shape of a declaration
//! rather than its behaviour. It reads the page modules rather than requiring a
//! database, so it runs with `cargo test` and fails in a second.

use std::fs;
use std::path::{Path, PathBuf};

/// A `POST` handler's signature, as found in the source.
struct Handler {
    file: PathBuf,
    line: usize,
    signature: String,
}

impl Handler {
    /// Whether this handler declares a body parameter.
    ///
    /// Parameters are `cx: &Cx` plus at most one other, so "more than one
    /// parameter that is not `cx`" is the signal — computed by counting commas
    /// at the top level of the parameter list.
    fn takes_a_body(&self) -> bool {
        let Some(params) = self.signature.split_once('(').and_then(|(_, rest)| rest.split_once(") -> ")) else {
            return false;
        };
        let params = params.0;

        let mut depth = 0usize;
        let mut count = 0usize;
        if !params.trim().is_empty() {
            count = 1;
        }
        for ch in params.chars() {
            match ch {
                '<' | '(' | '[' => depth += 1,
                '>' | ')' | ']' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => count += 1,
                _ => {}
            }
        }
        count >= 2
    }
}

/// Collect every `#[route(POST …)]` and `#[page(POST …)]` handler.
fn post_handlers() -> Vec<Handler> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_files(&root, &mut files);
    files.sort();

    // (file, line, signature) for every POST route found.
    let mut signatures: Vec<(PathBuf, usize, String)> = Vec::new();
    for file in files {
        let Ok(contents) = fs::read_to_string(&file) else {
            continue;
        };
        let lines: Vec<&str> = contents.lines().collect();

        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            // `#[route(POST "/x")]` or `#[page(POST "/x")]`, however the
            // attribute is split across the line.
            let attribute = (trimmed.starts_with("#[route(") || trimmed.starts_with("#[page("))
                && trimmed.contains("POST");
            if !attribute {
                continue;
            }

            // The declaration follows, past any attributes and doc comments, and
            // may wrap across lines. Collect until the return type is closed.
            let mut signature = String::new();
            let mut started = false;
            for candidate in lines.iter().skip(index + 1).take(25) {
                let candidate = candidate.trim();
                if !started {
                    if candidate.starts_with("//") || candidate.starts_with("#[") || candidate.is_empty() {
                        continue;
                    }
                    if !candidate.starts_with("async fn") && !candidate.starts_with("pub async fn") {
                        // Something unexpected between the attribute and the
                        // declaration; stop rather than guess.
                        break;
                    }
                    started = true;
                }
                signature.push_str(candidate);
                // A complete signature ends at the closing brace or the body.
                if candidate.contains("-> ") || candidate.ends_with('{') {
                    break;
                }
            }

            if signature.is_empty() {
                continue;
            }
            signatures.push((file.clone(), index + 1, signature));
        }
    }
    signatures
        .into_iter()
        .map(|(file, line, signature)| Handler {
            file,
            line,
            signature,
        })
        .collect()
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_post_route_takes_a_body_so_its_csrf_token_is_checked() {
    let handlers = post_handlers();
    assert!(
        handlers.len() >= 5,
        "expected to find the app's POST routes, found {} — the scanner is probably broken",
        handlers.len()
    );

    let missing: Vec<String> = handlers
        .iter()
        .filter(|handler| !handler.takes_a_body())
        .map(|handler| {
            format!(
                "{}:{}  {}",
                handler
                    .file
                    .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                    .unwrap_or(&handler.file)
                    .display(),
                handler.line,
                handler.signature
            )
        })
        .collect();

    assert!(
        missing.is_empty(),
        "these POST handlers declare no body parameter, so no extractor runs and their CSRF \
         token is never checked. Give each one a `body: crate::csrf::CsrfForm<T>` parameter, \
         with an empty struct if it needs no fields:\n  {}",
        missing.join("\n  ")
    );
}
