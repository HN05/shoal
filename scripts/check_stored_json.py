"""Fail when SQL reads, writes or builds a path inside stored JSON, as
AGENTS.md, under Daemon code, says.

It reads each package's `.sql` files under `src/` and `tests/`, and the string
literals of its Rust files there. It leaves out the store's migrations and
their tests, which read the shapes older releases stored, but reads the
schema they generate. `json_each` and `json_tree` may read a bound parameter,
and a `journal_append` call takes a `json_object` source, whose keys are its
table's column names. It reads `->` and `->>` in a Rust literal only when the
literal holds a keyword of SQL."""
import re
import sys
from dataclasses import dataclass
from pathlib import Path

from affected_packages import Workspace, cargo_metadata

# Paths, relative to the workspace root, that hold the migrations and their
# tests; one ending in `/` is a directory.
MIGRATIONS = (
    "crates/shoal-store/src/migrations.rs",
    "crates/shoal-store/src/migrations/",
    "crates/shoal-store/tests/fixtures/",
)
# Generated from the migrations, and read as any other source.
SCHEMA = "crates/shoal-store/src/migrations/schema.sql"
SOURCE_DIRECTORIES = ("src/", "tests/")

PATHS = "reads or writes a path inside stored JSON: read a column, or decode the value in Rust"
BUILDS = "builds JSON in SQL: select the columns and build the value in Rust"
ARRAY = ("reads the elements of stored JSON: keep them in a table of their own, "
         "or bind the array as a parameter")
# SQLite reads function names whatever their case.
FUNCTIONS = (
    (re.compile(r"\b(jsonb?_(?:extract|set|insert|replace|remove|patch|type|valid|array_length))"
                r"\s*\(", re.IGNORECASE), PATHS),
    (re.compile(r"\b(jsonb?_(?:object|array|group_array|group_object))\s*\(", re.IGNORECASE),
     BUILDS),
    # A bound parameter: `?`, `?1`, or a name after `:`, `@` or `$`.
    (re.compile(r"\b(jsonb?_(?:each|tree))\s*\((?!\s*(?:\?\d*|[:@$]\w+)\s*\))", re.IGNORECASE),
     ARRAY),
)
# Whatever their right operand, such as a path, a label or a column.
OPERATORS = ((re.compile(r"(->>?)"), PATHS),)
# A word only SQL puts in a Rust literal that also holds `->`, which text
# such as `{name} -> {port}` does too.
SQL_KEYWORD = re.compile(r"\b(?:select|insert|update|delete|from|where|values|join)\b",
                         re.IGNORECASE)
JOURNAL_SOURCE = re.compile(r"\bjournal_append\s*\(", re.IGNORECASE)
JSON_OBJECT = re.compile(r"json_object(?=\s*\()", re.IGNORECASE)
# A string, a quoted identifier or a comment, each to the end of the text
# when it is left open; group `name` is an identifier's word.
SQL_QUOTED = re.compile(
    r"'(?:[^']|'')*(?:'|$)"
    r"|\"(?:(?P<name>\w+)\"|(?:[^\"]|\"\")*(?:\"|$))"
    r"|`(?:(?P<backticked>\w+)`|[^`]*(?:`|$))"
    r"|\[(?:(?P<bracketed>\w+)\]|[^\]]*(?:\]|$))"
    r"|--[^\n]*|/\*.*?(?:\*/|$)",
    re.DOTALL)
# A Rust line comment, the start of a block comment, or a literal with its
# body as group `raw` or `text`.
RUST_TOKEN = re.compile(
    r"//[^\n]*|(?P<block>/\*)"
    r"|(?<!\w)[bc]?r(?P<hashes>#*)\"(?P<raw>.*?)\"(?P=hashes)"
    r"|(?<!\w)[bc]?\"(?P<text>(?:\\.|[^\"\\])*)\""
    r"|'(?:\\.|[^'\\\n])'",
    re.DOTALL)
# A line continuation takes the whitespace that starts the next line with it.
RUST_ESCAPE = re.compile(r"\\(\n\s*|x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)", re.DOTALL)
# What the escapes that stand for no character of their own stand for.
RUST_ESCAPED = {"n": "\n", "r": "\r", "t": "\t", "0": "\0"}


@dataclass(frozen=True, order=True)
class Problem:
    path: str
    line: int
    function: str
    reason: str

    def __str__(self):
        return f"{self.path}:{self.line}: `{self.function}` {self.reason}"


def blank(match):
    """*match*'s text with everything but its line breaks as spaces, so that
    offsets and line numbers stay."""
    return re.sub(r"[^\n]", " ", match.group(0))


def executable(sql):
    """*sql* with its strings and comments blanked, and each quoted
    identifier that is a word left as that word, since SQLite calls
    `"json_extract"(...)` as it calls `json_extract(...)`."""
    def unquoted(match):
        name = match.group("name") or match.group("backticked") or match.group("bracketed")
        return f" {name} " if name else blank(match)
    return SQL_QUOTED.sub(unquoted, sql)


def call_end(sql, start):
    """The offset just past the parenthesis that closes the one before
    *start* in the [`executable`] *sql*, or its end when none does."""
    depth = 1
    for match in re.finditer(r"[()]", sql[start:]):
        depth += 1 if match.group(0) == "(" else -1
        if depth == 0:
            return start + match.end()
    return len(sql)


def without_journal_sources(sql):
    """The [`executable`] *sql* with the `json_object` that starts each
    `journal_append` call's source blanked; whatever the source holds is
    still read."""
    for call in JOURNAL_SOURCE.finditer(sql):
        source = JSON_OBJECT.search(sql, call.end(), call_end(sql, call.end()))
        if source:
            sql = sql[:source.start()] + " " * len(source.group(0)) + sql[source.end():]
    return sql


def sql_problems(sql, path, line_of, statement=True):
    """Each refused use in *sql*, at the line `line_of` gives its offset.
    Unless *sql* is a `statement`, it reads the operators only when *sql*
    holds a keyword of SQL."""
    sql = without_journal_sources(executable(sql))
    refused = FUNCTIONS + (OPERATORS if statement or SQL_KEYWORD.search(sql) else ())
    return [Problem(path, line_of(match.start(1)), match.group(1), reason)
            for pattern, reason in refused for match in pattern.finditer(sql)]


def rust_literals(source):
    """(text, offsets) of each string literal in Rust *source*, outside
    comments: its text with each escape decoded, and the offset in *source*
    of each character of that text."""
    at = 0
    while token := RUST_TOKEN.search(source, at):
        at = token.end()
        if token.group("block"):
            at = block_comment_end(source, at)
        elif token.group("raw") is not None:
            start = token.start("raw")
            yield token.group("raw"), range(start, token.end("raw"))
        elif token.group("text") is not None:
            yield decoded(token.group("text"), token.start("text"))


def block_comment_end(source, at):
    """The offset past the end of the Rust block comment whose `/*` ends
    at *at*; Rust nests block comments."""
    depth = 1
    for mark in re.finditer(r"/\*|\*/", source[at:]):
        depth += 1 if mark.group(0) == "/*" else -1
        if depth == 0:
            return at + mark.end()
    return len(source)


def decoded(body, start):
    """(text, offsets) of the body of a Rust string literal at *start*."""
    text, offsets, at = [], [], 0
    for escape in RUST_ESCAPE.finditer(body):
        text.append(body[at:escape.start()])
        offsets.extend(range(start + at, start + escape.start()))
        character = escaped(escape.group(1))
        text.append(character)
        offsets.extend([start + escape.start()] * len(character))
        at = escape.end()
    text.append(body[at:])
    offsets.extend(range(start + at, start + len(body)))
    return "".join(text), offsets


def escaped(code):
    """The text the Rust escape `\\{code}` stands for: one character, or
    none for a line continuation."""
    if code[0] == "\n":
        return ""
    if code[0] in "xu" and len(code) > 1:
        return chr(int(code.strip("xu{}").replace("_", ""), 16))
    return RUST_ESCAPED.get(code, code)


def file_problems(path, text):
    """Each refused use in the file at *path*, which holds *text*."""
    def line_at(offset):
        return text.count("\n", 0, offset) + 1
    if path.endswith(".sql"):
        return sql_problems(text, path, line_at)
    return [problem for body, offsets in rust_literals(text)
            for problem in sql_problems(body, path, lambda at, offsets=offsets: line_at(offsets[at]),
                                        statement=False)]


def skipped(path):
    return path != SCHEMA and any(
        path.startswith(prefix) if prefix.endswith("/") else path == prefix
        for prefix in MIGRATIONS)


def sources(root, directories):
    """The workspace-relative path of each Rust and SQL file in `src/` and
    `tests/` of the package *directories* under *root*."""
    root = Path(root)
    for directory in sorted(set(directories)):
        roots = (directory,) if directory in SOURCE_DIRECTORIES else (
            directory + source for source in SOURCE_DIRECTORIES)
        for source_root in roots:
            for path in sorted((root / source_root).rglob("*")):
                relative = path.relative_to(root).as_posix()
                if path.suffix in (".rs", ".sql") and path.is_file() and not skipped(relative):
                    yield relative


def problems(root, directories):
    """Every refused use in the packages whose *directories* lie under
    *root*, sorted."""
    return sorted(problem for path in sources(root, directories)
                  for problem in file_problems(path, (Path(root) / path).read_text()))


def main():
    metadata = cargo_metadata()
    directories = [directory for owned in Workspace.from_metadata(metadata).directories.values()
                   for directory in owned]
    found = problems(metadata["workspace_root"], directories)
    for problem in found:
        print(f"::error file={problem.path},line={problem.line}::{problem}", file=sys.stderr)
    if found:
        sys.exit(1)


if __name__ == "__main__":
    main()
