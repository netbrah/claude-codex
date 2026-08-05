//! Per-translation-unit AST visitor that extracts caller→callee edges
//! using libclang's cursor visitor and USR-based identity.

use std::path::Path;

use clang::Clang;
use clang::Entity;
use clang::EntityKind;
use clang::EntityVisitResult;
use clang::Index;
use clang::TranslationUnit;
// TODO: Once clang-rs exposes raw CXCursor, switch from synthetic USR to:
//   use clang_sys::{clang_getCursorUSR, clang_getCString, clang_disposeString, CXCursor};

use super::compile_db::FileCompileArgs;

/// A directed call edge: caller invokes callee.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallEdge {
    /// USR (Unified Symbol Resolution) of the calling function.
    pub caller_usr: String,
    /// Display name of the calling function.
    pub caller_name: String,
    /// File where the caller is defined.
    pub caller_file: String,
    /// USR of the called function.
    pub callee_usr: String,
    /// Display name of the called function.
    pub callee_name: String,
    /// File where the callee is defined (if resolvable).
    pub callee_file: Option<String>,
    /// Whether this is a virtual/dynamic dispatch call.
    pub is_dynamic: bool,
    /// Line number of the call site within the caller.
    pub call_line: u32,
}

/// Metadata about a function discovered during AST traversal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionInfo {
    pub usr: String,
    pub display_name: String,
    pub file: String,
    pub line: u32,
    pub is_definition: bool,
}

/// Parses a single translation unit and extracts call edges.
pub struct TuParser;

impl TuParser {
    /// Parse a single file using its compile arguments and extract all
    /// caller→callee edges.
    ///
    /// This is the core operation: one TU in, Vec<CallEdge> out.
    pub fn extract_edges(clang: &Clang, args: &FileCompileArgs) -> Result<Vec<CallEdge>, String> {
        let index = Index::new(clang, false, false);
        let arg_strs: Vec<&str> = args.arguments.iter().map(|s| s.as_str()).collect();

        let tu = index
            .parser(&args.file)
            .arguments(&arg_strs)
            .parse()
            .map_err(|e| format!("failed to parse {}: {e}", args.file.display()))?;

        let root = tu.get_entity();
        let mut edges = Vec::new();
        let mut current_fn: Option<FunctionInfo> = None;

        Self::visit_recursive(&root, &tu, &mut current_fn, &mut edges);

        Ok(edges)
    }

    /// Extract edges AND function declarations from a TU.
    pub fn extract_edges_and_functions(
        clang: &Clang,
        args: &FileCompileArgs,
    ) -> Result<(Vec<CallEdge>, Vec<FunctionInfo>), String> {
        let index = Index::new(clang, false, false);
        let arg_strs: Vec<&str> = args.arguments.iter().map(|s| s.as_str()).collect();

        let tu = index
            .parser(&args.file)
            .arguments(&arg_strs)
            .parse()
            .map_err(|e| format!("failed to parse {}: {e}", args.file.display()))?;

        let root = tu.get_entity();
        let mut edges = Vec::new();
        let mut functions = Vec::new();
        let mut current_fn: Option<FunctionInfo> = None;

        Self::visit_with_functions(&root, &tu, &mut current_fn, &mut edges, &mut functions);

        Ok((edges, functions))
    }

    fn visit_recursive(
        entity: &Entity<'_>,
        tu: &TranslationUnit<'_>,
        current_fn: &mut Option<FunctionInfo>,
        edges: &mut Vec<CallEdge>,
    ) {
        entity.visit_children(|child, _parent| {
            // Skip entities not from the main file to reduce noise from headers.
            // We still recurse into the main file's AST.
            let location = child.get_location();
            let is_main_file = location.map(|loc| loc.is_in_main_file()).unwrap_or(false);

            match child.get_kind() {
                EntityKind::FunctionDecl
                | EntityKind::Method
                | EntityKind::Constructor
                | EntityKind::Destructor
                | EntityKind::FunctionTemplate => {
                    if is_main_file {
                        let usr = Self::get_usr_safe(&child);
                        let name = child.get_display_name().unwrap_or_default();
                        let (file, line) = Self::get_file_line(&child);

                        let fn_info = FunctionInfo {
                            usr: usr.clone(),
                            display_name: name,
                            file,
                            line,
                            is_definition: child.is_definition(),
                        };

                        let prev = current_fn.replace(fn_info);
                        Self::visit_recursive(&child, tu, current_fn, edges);
                        *current_fn = prev;

                        return EntityVisitResult::Continue;
                    }
                    EntityVisitResult::Continue
                }

                EntityKind::CallExpr => {
                    if let Some(ref caller) = current_fn {
                        let callee_ref = child.get_reference().or_else(|| child.get_definition());

                        let callee_usr = callee_ref
                            .as_ref()
                            .map(|r| Self::get_usr_safe(r))
                            .unwrap_or_default();

                        let callee_name = callee_ref
                            .as_ref()
                            .and_then(|r| r.get_display_name())
                            .or_else(|| child.get_display_name())
                            .unwrap_or_else(|| "<unresolved>".into());

                        let callee_file = callee_ref
                            .as_ref()
                            .map(|r| Self::get_file_line(r).0)
                            .filter(|f| !f.is_empty());

                        let call_line = child
                            .get_location()
                            .map(|loc| loc.get_file_location().line)
                            .unwrap_or(0);

                        if !callee_usr.is_empty() && callee_usr != caller.usr {
                            edges.push(CallEdge {
                                caller_usr: caller.usr.clone(),
                                caller_name: caller.display_name.clone(),
                                caller_file: caller.file.clone(),
                                callee_usr,
                                callee_name,
                                callee_file,
                                is_dynamic: child.is_dynamic_call(),
                                call_line,
                            });
                        }
                    }
                    EntityVisitResult::Recurse
                }

                _ => EntityVisitResult::Recurse,
            }
        });
    }

    fn visit_with_functions(
        entity: &Entity<'_>,
        tu: &TranslationUnit<'_>,
        current_fn: &mut Option<FunctionInfo>,
        edges: &mut Vec<CallEdge>,
        functions: &mut Vec<FunctionInfo>,
    ) {
        entity.visit_children(|child, _parent| {
            let location = child.get_location();
            let is_main_file = location.map(|loc| loc.is_in_main_file()).unwrap_or(false);

            match child.get_kind() {
                EntityKind::FunctionDecl
                | EntityKind::Method
                | EntityKind::Constructor
                | EntityKind::Destructor
                | EntityKind::FunctionTemplate => {
                    if is_main_file {
                        let usr = Self::get_usr_safe(&child);
                        let name = child.get_display_name().unwrap_or_default();
                        let (file, line) = Self::get_file_line(&child);

                        let fn_info = FunctionInfo {
                            usr: usr.clone(),
                            display_name: name,
                            file,
                            line,
                            is_definition: child.is_definition(),
                        };

                        functions.push(fn_info.clone());

                        let prev = current_fn.replace(fn_info);
                        Self::visit_with_functions(&child, tu, current_fn, edges, functions);
                        *current_fn = prev;

                        return EntityVisitResult::Continue;
                    }
                    EntityVisitResult::Continue
                }

                EntityKind::CallExpr => {
                    if let Some(ref caller) = current_fn {
                        let callee_ref = child.get_reference().or_else(|| child.get_definition());

                        let callee_usr = callee_ref
                            .as_ref()
                            .map(|r| Self::get_usr_safe(r))
                            .unwrap_or_default();

                        let callee_name = callee_ref
                            .as_ref()
                            .and_then(|r| r.get_display_name())
                            .or_else(|| child.get_display_name())
                            .unwrap_or_else(|| "<unresolved>".into());

                        let callee_file = callee_ref
                            .as_ref()
                            .map(|r| Self::get_file_line(r).0)
                            .filter(|f| !f.is_empty());

                        let call_line = child
                            .get_location()
                            .map(|loc| loc.get_file_location().line)
                            .unwrap_or(0);

                        if !callee_usr.is_empty() && callee_usr != caller.usr {
                            edges.push(CallEdge {
                                caller_usr: caller.usr.clone(),
                                caller_name: caller.display_name.clone(),
                                caller_file: caller.file.clone(),
                                callee_usr,
                                callee_name,
                                callee_file,
                                is_dynamic: child.is_dynamic_call(),
                                call_line,
                            });
                        }
                    }
                    EntityVisitResult::Recurse
                }

                _ => EntityVisitResult::Recurse,
            }
        });
    }

    /// Compute a best-effort, synthetic "USR" for a clang `Entity`.
    ///
    /// This does *not* currently use `clang-sys` / `CXCursor` USRs. Instead,
    /// we approximate identity using the entity's display name, semantic
    /// parent, and source location. The resulting string is only guaranteed
    /// to be stable within a single translation-unit parse and must not be
    /// treated as a globally stable or ABI-level identifier.
    fn get_usr_safe(entity: &Entity<'_>) -> String {
        // NOTE: clang-rs doesn't yet expose the underlying CXCursor needed to
        // call `clang_getCursorUSR`. Until that is available (or we switch to
        // using clang-sys directly in this code), we fall back to a synthetic
        // identifier derived from name + file:line (and optional parent).
        let name = entity.get_display_name().unwrap_or_default();
        let (file, line) = Self::get_file_line(entity);
        let parent = entity
            .get_semantic_parent()
            .and_then(|p| p.get_display_name())
            .unwrap_or_default();

        if name.is_empty() {
            return String::new();
        }

        // Synthetic USR: parent::name@file:line
        // This is stable within a single parse and unique enough for
        // cross-TU matching when the same function appears in headers.
        if parent.is_empty() {
            format!("{name}@{file}:{line}")
        } else {
            format!("{parent}::{name}@{file}:{line}")
        }
    }

    fn get_file_line(entity: &Entity<'_>) -> (String, u32) {
        entity
            .get_location()
            .and_then(|loc| {
                let file_loc = loc.get_file_location();
                let file = file_loc
                    .file
                    .map(|f| f.get_path().to_string_lossy().into_owned())?;
                Some((file, file_loc.line))
            })
            .unwrap_or_else(|| (String::new(), 0))
    }
}
