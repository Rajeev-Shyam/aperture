"""Mechanical checker for docs/engineering/*.md.

For every back-ticked span in a doc (outside code fences):
  * path-like spans (segments joined by / or \\, last segment with a source extension, or a
    trailing '/'): the path must exist under the repo root, either directly or as a suffix of
    some real file path (docs name files relative to their component); build artifacts
    (target/, models/, *.exe, *.gguf, *.bin, src-tauri/binaries/) are skipped;
  * 7..40-hex tokens are verified as git commits;
  * every other identifier token ([A-Za-z_][A-Za-z0-9_]*, len >= 3) must exist in the token
    universe built from the repo's source, config and design docs (docs/engineering excluded).
Template headings (component / system-design / change) must be present and in order.

Usage: python doccheck.py <repo-root> <doc.md> [<doc.md> ...]
       python doccheck.py <repo-root> --all        (every doc under docs/engineering)
"""
import os
import re
import subprocess
import sys

CODE_EXT = {".rs", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".json", ".toml", ".sql", ".ps1",
            ".html", ".css", ".yml", ".yaml", ".md", ".txt", ".lock", ".cmd", ".bat", ".py",
            ".template", ".nsi", ".xml", ".ini", ".cfg"}
SKIP_DIRS = {"node_modules", "target", "dist", ".git", "models", "binaries", ".vite"}
ROOT_DIRS = ("crates/", "src-tauri/", "ui/", "xtask/", "extension/", "docs/", "config/",
             "scripts/", "models/", "target/", ".github/")

TOKEN_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
HEX_RE = re.compile(r"^[0-9a-f]{7,40}$")
SPAN_RE = re.compile(r"`([^`\n]+)`")
PATH_EXT = (".rs", ".ts", ".tsx", ".js", ".json", ".toml", ".md", ".sql", ".exe", ".gguf",
            ".bin", ".html", ".css", ".ps1", ".lock", ".cmd", ".yml", ".txt", ".py", ".template",
            ".db", ".vscdb", ".gitignore", ".gitkeep", ".nsi")

COMPONENT_HEADINGS = ["Purpose", "Responsibilities", "Public surface", "Data flow",
                      "Internal structure", "Invariants and guards", "Failure modes",
                      "Configuration", "Tests and gates", "Known gaps", "History"]
SYSTEM_HEADINGS = ["The mechanism in one paragraph", "Sequence", "Guarantees",
                   "Where it can break", "History"]
CHANGE_HEADINGS = ["Why", "What changed", "Contracts touched", "Verification", "Follow-ups"]

PER_FILE = {}
PAIR_RE = re.compile(r"(?<![A-Za-z0-9_])([A-Z][A-Za-z0-9_]*)::([a-z_][A-Za-z0-9_]*)(?![A-Za-z0-9_])")
ALLOW = set("""
usize isize u8 u16 u32 u64 i8 i16 i32 i64 f32 f64 bool str String Vec Option Result Some None Ok Err
Box Arc Rc Mutex RwLock impl trait struct enum const static let mut pub mod use crate self Self super
fn async await dyn ref move match if else for while loop return break continue where type as in
true false unsafe extern derive Debug Clone Copy Default PartialEq Eq Hash Send Sync Sized Drop
Iterator IntoIterator From Into TryFrom TryInto AsRef Display Error Deref Fn FnMut FnOnce
HashMap HashSet BTreeMap BTreeSet VecDeque PathBuf Path Duration Instant SystemTime OsString
todo unimplemented panic assert assert_eq assert_ne unreachable println eprintln format vec dbg
serde serde_json tokio tracing anyhow thiserror uuid chrono rusqlite tauri windows
const function var new this null undefined number string boolean void never unknown any object
export import default from interface class extends implements readonly private public protected
async await Promise Array Map Set Record Partial Pick Omit Readonly Required JSON Date Math
useState useEffect useRef useMemo useCallback React ReactDOM
""".split())


def build_universe(root):
    tokens = set()
    ci = set()
    files = []  # repo-relative, forward slashes
    per_file = {}  # rel path -> token set (source files only)
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        # docs/engineering is excluded from the token universe but its files are still indexable
        for fn in filenames:
            p = os.path.join(dirpath, fn)
            rel = os.path.relpath(p, root).replace("\\", "/")
            files.append(rel)
            for seg in rel.split("/"):
                for t in TOKEN_RE.findall(seg):
                    tokens.add(t)
                    ci.add(t.lower())
            if "/engineering/" in "/" + rel:
                continue
            ext = os.path.splitext(fn)[1].lower()
            if ext not in CODE_EXT and fn not in (".gitignore", ".gitkeep"):
                continue
            try:
                with open(p, "r", encoding="utf-8", errors="ignore") as f:
                    text = f.read()
            except OSError:
                continue
            ft = set(TOKEN_RE.findall(text))
            if ext in (".rs", ".ts", ".tsx", ".js"):
                per_file[rel] = ft
            for t in ft:
                tokens.add(t)
                ci.add(t.lower())
    global PER_FILE
    PER_FILE = per_file
    # also the directories themselves, as trailing-slash paths
    dirs = set()
    for f in files:
        parts = f.split("/")
        for i in range(1, len(parts)):
            dirs.add("/".join(parts[:i]) + "/")
    return tokens, ci, files, dirs


def looks_like_path(span):
    s = span.strip()
    if " " in s or "::" in s or "://" in s or "%" in s or "(" in s or "$" in s:
        return False
    if s.startswith("\\") or s.startswith("/") or ".." in s or s.startswith("HKCU") \
            or s.startswith("HKLM") or s.startswith("Software\\") or s.startswith("Microsoft\\") \
            or s.startswith("Code\\"):
        return False
    if s.endswith("/") or s.endswith("\\"):
        return True
    last = s.replace("\\", "/").split("/")[-1]
    return last.endswith(PATH_EXT) or last in (".gitignore", ".gitkeep")


def check_path(span, files, dirs):
    s = span.strip().replace("\\", "/")
    s = re.sub(r":\d+(-\d+)?$", "", s)  # file.rs:123
    if s.startswith("target/") or s.startswith("models/") or s.startswith("src-tauri/binaries/") \
            or "/target/" in s or s.endswith((".exe", ".gguf", ".bin", ".db", ".vscdb")):
        return "artifact"
    if any(ch in s for ch in "{}*<>…?"):
        return "glob"
    if s.endswith("/"):
        if s in dirs or any(d.endswith("/" + s) for d in dirs):
            return "ok"
        return "MISSING"
    if s in files:
        return "ok"
    if any(f.endswith("/" + s) for f in files):
        return "ok"
    return "MISSING"


def check_doc(root, doc_path, universe, universe_ci, files, dirs, hashes):
    with open(doc_path, "r", encoding="utf-8") as f:
        lines = f.read().split("\n")
    problems = []
    headings = [re.sub(r"^##\s+", "", l).strip() for l in lines if l.startswith("## ")]
    rel = doc_path.replace("\\", "/")
    if "/system-design/" in rel:
        kind, want = "system", SYSTEM_HEADINGS
    elif "/changes/" in rel:
        kind, want = "change", CHANGE_HEADINGS
    else:
        kind, want = "component", COMPONENT_HEADINGS
    missing = [h for h in want if h not in headings]
    if missing:
        problems.append((0, "HEADING", "missing sections: " + ", ".join(missing)))
    order = [h for h in headings if h in want]
    if order != [h for h in want if h in headings]:
        problems.append((0, "HEADING", "sections out of template order: " + " > ".join(order)))
    extra = [h for h in headings if h not in want]
    if extra:
        problems.append((0, "HEADING-EXTRA", "non-template sections: " + ", ".join(extra)))
    if kind == "component" and not (len(lines) > 2 and lines[2].startswith("**Code:**")):
        problems.append((3, "HEADER", "line 3 should start with **Code:** … · **Design:** … · **Owner invariants:**"))
    if kind == "change" and not (len(lines) > 2 and lines[2].startswith("**Date:**")):
        problems.append((3, "HEADER", "line 3 should start with **Date:** … · **Commit(s):** … · **Session:**"))
    if kind == "system" and not (len(lines) > 2 and lines[2].startswith("**Design:**")):
        problems.append((3, "HEADER", "line 3 should start with **Design:** … · **Components:** … · **Invariant:**"))

    in_fence = False
    for i, line in enumerate(lines, 1):
        if line.strip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        for span in SPAN_RE.findall(line):
            if looks_like_path(span):
                r = check_path(span, files, dirs)
                if r == "MISSING":
                    problems.append((i, "PATH", span))
                continue
            bad = []
            body = span
            for h in re.findall(r"(?<![A-Za-z0-9_])[0-9a-f]{7,40}(?![A-Za-z0-9_])", body):
                if not h.isalpha() and not h.isdigit():
                    hashes.setdefault(h, []).append((doc_path, i))
                    body = body.replace(h, " ")
            for pre in re.findall(r"([A-Za-z_][A-Za-z0-9_]*_)\*", body):
                if not any(u.startswith(pre) for u in universe):
                    bad.append(pre + "* (no such prefix)")
                body = body.replace(pre + "*", " ")
            body = re.sub(r"<[A-Za-z_][A-Za-z0-9_]*>", " ", body)  # <placeholder>
            for ty, m in PAIR_RE.findall(body):
                if ty in ALLOW or ty in ("Self", "Box", "Arc", "Command", "Instant", "Duration"):
                    continue
                if not any(ty in ft and m in ft for ft in PER_FILE.values()):
                    if ty in universe and m in universe:
                        bad.append(f"{ty}::{m} (pair never co-occurs in one source file)")
            for t in TOKEN_RE.findall(body):
                if len(t) < 3 or t in ALLOW or t in universe:
                    continue
                if t.lower() in universe_ci:
                    bad.append(t + " (case)")
                else:
                    bad.append(t)
            if bad:
                problems.append((i, "TOKEN", f"`{span}` -> {', '.join(bad)}"))
    return problems


def main():
    root = os.path.abspath(sys.argv[1])
    args = sys.argv[2:]
    if args == ["--all"]:
        docs = []
        for sub in ("components", "system-design", "changes"):
            d = os.path.join(root, "docs", "engineering", sub)
            if os.path.isdir(d):
                docs += [os.path.join(d, f) for f in sorted(os.listdir(d)) if f.endswith(".md")]
    else:
        docs = [os.path.abspath(a) for a in args]
    universe, universe_ci, files, dirs = build_universe(root)
    print(f"universe: {len(universe)} tokens, {len(files)} files")
    hashes = {}
    results = {}
    for doc in docs:
        results[doc] = check_doc(root, doc, universe, universe_ci, files, dirs, hashes)
    # verify commit hashes in one go
    bad_hashes = set()
    for h in hashes:
        r = subprocess.run(["git", "-C", root, "cat-file", "-e", h + "^{commit}"],
                           capture_output=True)
        if r.returncode != 0:
            bad_hashes.add(h)
    for h in bad_hashes:
        for doc, ln in hashes[h]:
            results[doc].append((ln, "COMMIT", f"{h} is not a commit in this repo"))
    total = 0
    for doc in docs:
        probs = sorted(results[doc])
        rel = os.path.relpath(doc, root)
        print(f"\n== {rel}: {len(probs)} problem(s)")
        for ln, kind, msg in probs:
            print(f"  L{ln:<4} {kind:<14} {msg}")
        total += len(probs)
    print(f"\nTOTAL problems: {total}")


if __name__ == "__main__":
    main()
