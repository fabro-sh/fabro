#!/usr/bin/env python3
"""Extract full digraph DOT examples from Fabro documentation files."""

import os
import re
import sys
from pathlib import Path

DOCS_DIR = Path(__file__).resolve().parent.parent.parent / "docs"
OUTPUT_DIR = Path(__file__).resolve().parent


def extract_dot_blocks(filepath: Path) -> list[dict]:
    """Extract all ```dot code blocks from a file."""
    blocks = []
    with open(filepath) as f:
        lines = f.readlines()

    in_dot = False
    block_start = 0
    block_lines: list[str] = []
    title = ""

    for i, line in enumerate(lines, 1):
        m = re.match(r'\s*```dot(?:\s+title="([^"]*)")?\s*$', line)
        if m and not in_dot:
            in_dot = True
            block_start = i
            block_lines = []
            title = m.group(1) or ""
        elif in_dot and re.match(r"\s*```\s*$", line):
            in_dot = False
            code = "".join(block_lines)
            is_full = "digraph" in code
            blocks.append(
                {
                    "line": block_start,
                    "code": code,
                    "is_full": is_full,
                    "title": title,
                    "num_lines": len(block_lines),
                }
            )
        elif in_dot:
            block_lines.append(line)

    return blocks


def page_dir(filepath: Path) -> Path:
    """Convert docs/tutorials/hello-world.mdx -> tutorials/hello-world/"""
    rel = filepath.relative_to(DOCS_DIR)
    return Path(rel.parent) / rel.stem


def derive_filename(block: dict, index: int) -> str:
    """Derive .dot filename from title or digraph name."""
    if block["title"]:
        name = block["title"]
        if not name.endswith(".dot"):
            name += ".dot"
        return name

    # Extract digraph name
    m = re.search(r"digraph\s+(\w+)", block["code"])
    if m:
        # Convert CamelCase to kebab-case
        name = re.sub(r"(?<!^)(?=[A-Z])", "-", m.group(1)).lower()
        return f"{name}.dot"

    return f"workflow-{index:02d}.dot"


def find_prompt_refs(code: str) -> list[str]:
    """Find @path/to/file.md references in DOT code."""
    return re.findall(r'@([\w./-]+\.md)', code)


def find_custom_vars(code: str) -> list[str]:
    """Find $variable references that aren't $goal or $$-escaped."""
    # Remove $$ escapes first
    cleaned = code.replace("$$", "")
    vars_found = set(re.findall(r'\$([a-zA-Z_]\w*)', cleaned))
    vars_found.discard("goal")
    return sorted(vars_found)


def main():
    skip_pages = {
        "changelog/2026-02-27",        # deprecated handler=codergen syntax
        "human-tools/vnc-access",       # workflows not yet working
        "human-tools/vpn-connections",  # workflows not yet working
    }

    extracted = 0
    skipped_snippets = 0
    prompt_stubs_needed: list[tuple[Path, str]] = []
    var_dots_needed: list[tuple[Path, list[str]]] = []

    for mdx_path in sorted(DOCS_DIR.rglob("*.mdx")):
        blocks = extract_dot_blocks(mdx_path)
        if not blocks:
            continue

        pdir = page_dir(mdx_path)
        if str(pdir) in skip_pages:
            print(f"  SKIP {pdir} (excluded)")
            continue

        full_blocks = [b for b in blocks if b["is_full"]]
        snippet_blocks = [b for b in blocks if not b["is_full"]]

        if not full_blocks:
            skipped_snippets += len(snippet_blocks)
            continue

        out_dir = OUTPUT_DIR / pdir
        out_dir.mkdir(parents=True, exist_ok=True)

        seen: set[str] = set()
        for i, block in enumerate(full_blocks):
            filename = derive_filename(block, i)
            if filename in seen:
                base, ext = os.path.splitext(filename)
                n = 2
                while f"{base}-{n:02d}{ext}" in seen:
                    n += 1
                filename = f"{base}-{n:02d}{ext}"
            seen.add(filename)
            out_path = out_dir / filename
            out_path.write_text(block["code"])
            extracted += 1
            print(f"  WRITE {out_path.relative_to(OUTPUT_DIR)} ({block['num_lines']} lines)")

            # Check for prompt refs
            for ref in find_prompt_refs(block["code"]):
                prompt_stubs_needed.append((out_dir, ref))

            # Check for custom vars
            custom_vars = find_custom_vars(block["code"])
            if custom_vars:
                var_dots_needed.append((out_path, custom_vars))

        skipped_snippets += len(snippet_blocks)

    # Create prompt stubs
    created_stubs = set()
    for dot_dir, ref in prompt_stubs_needed:
        stub_path = dot_dir / ref
        if str(stub_path) in created_stubs:
            continue
        stub_path.parent.mkdir(parents=True, exist_ok=True)
        stub_path.write_text("Stub prompt for testing.\n")
        created_stubs.add(str(stub_path))
        print(f"  STUB  {stub_path.relative_to(OUTPUT_DIR)}")

    # Create run.toml files for variable-using DOTs
    for dot_path, vars_list in var_dots_needed:
        toml_name = f"run-{dot_path.stem}.toml"
        toml_path = dot_path.parent / toml_name
        # Extract goal from the DOT if possible
        dot_content = dot_path.read_text()
        goal_match = re.search(r'goal\s*=\s*"([^"]*)"', dot_content)
        goal = goal_match.group(1) if goal_match else "Test workflow"

        lines = [
            'version = 1',
            f'goal = "{goal}"',
            f'graph = "{dot_path.name}"',
            '',
            '[vars]',
        ]
        for v in vars_list:
            lines.append(f'{v} = "test-{v}"')
        lines.append('')

        toml_path.write_text("\n".join(lines))
        print(f"  TOML  {toml_path.relative_to(OUTPUT_DIR)} (vars: {', '.join(vars_list)})")

    print(f"\nDone: {extracted} full workflows extracted, {skipped_snippets} snippets skipped")
    print(f"  {len(created_stubs)} prompt stubs created")
    print(f"  {len(var_dots_needed)} run.toml configs created")


if __name__ == "__main__":
    main()
