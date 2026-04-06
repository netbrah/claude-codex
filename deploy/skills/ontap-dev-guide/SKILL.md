---
name: ontap-dev-guide
description: ONTAP development patterns and codebase navigation. Use when working with ONTAP C/C++ source code, SMF iterators, keymanager subsystem, or writing unit tests.
metadata:
  short-description: ONTAP codebase development guidance
---

# ONTAP Development Guide

## Codebase Navigation

Use the MCP tools (`analyze_iterator`, `call_graph_fast`, `trace_call_chain`, `analyze_symbol_ast`) to explore the ONTAP codebase. These are your primary instruments.

### Key Patterns

1. **SMF Iterators** — Data access objects generated from `.smf` schema files. Use `analyze_iterator` to understand fields, callers, and REST mappings.

2. **CLI → NACL → Iterator chain** — Every CLI command maps to a NACL method which instantiates an iterator. Use `trace_call_chain` to walk this.

3. **Unit tests** — Use `generate_test_plan` or `prepare_unit_test_context` to understand fixtures, mockers, and FIJI fault handles before writing tests.

## Workflow

When asked to investigate or modify ONTAP code:
1. Start with `analyze_symbol_ast` on the function of interest
2. Use `call_graph_fast` to understand who calls it (upstream) 
3. Use `trace_call_chain` to find the CLI entry point and tables touched
4. Check for existing CITs with `find_cits`
5. Check Jira for related bugs with `ask_jira`

## Remote SCS Machine Access

The ONTAP build environment is on a remote RHEL 9 SCS machine. The login shell
is `csh` (LDAP-locked, cannot be changed).

### Shell Hazards — READ THIS

**csh will break your commands** if you use these patterns via `ssh scs`:
- `find -name "*.cc"` → csh glob-expands `*.cc` before `find` runs → "No match"
- `cmd 2>&1` → "Ambiguous output redirect"
- `$()` subshells → not supported in csh

### How to Run Commands Correctly

**Option A** — Use `scs-run` (recommended, handles quoting automatically):
```bash
scs-run 'find /path -name "*.cc" -maxdepth 2 | head -10'
scs-run 'make -f ./bedrock/Makefile.ulibso-l.linux64.debug foo.o 2>&1'
```

**Option B** — Wrap through bash explicitly:
```bash
ssh scs "bash -lc 'find /path -name \"*.cc\" 2>&1'"
```

**Option C** — Use `fd` instead of `find` (faster, no glob issues):
```bash
ssh scs 'fd -e cc -d 3 . /path/to/component/'
ssh scs 'fd -e ut -d 2 . /path/to/component/'
ssh scs 'fd Component.py /path/to/workspace/'
```

### Available Tools on Remote

| Tool | Path | Notes |
|------|------|-------|
| `fd` 10.2.0 | `~/bin/fd` | Static musl binary, NFS-shared, works on all SCS machines |
| `rg` (ripgrep) | system | Installed via dnf |
| `fzf` | system | Installed via dnf |
| `bat` | system | Aliased to `cat` in bashrc |

### ONTAP Build (Single-File)

To compile a single file after editing:
```bash
# .cc/.cpp/.h files → ulibso-l subcomponent
scs-run 'cd /path/to/component && make -f ./bedrock/Makefile.ulibso-l.linux64.debug filename.o'

# .ut files → utest-l subcomponent
scs-run 'cd /path/to/component && make -f ./bedrock/Makefile.utest-l.linux64.debug filename.o'
```

The component directory is the one containing `Component.py`.
