#!/usr/bin/env python3
"""check-wasm-exports: fail a wasm-bindgen build whose externref table export is wrong.

Why this exists: the 2026-09-19 production build of dirmacs-admin exported
`__wbindgen_externrefs` as table 0, the fixed-size function table
(min == max). wasm-bindgen's JS init calls `table.grow(4)` on that export,
which throws `RangeError`, so the app never starts and admin.dirmacs.com
served a blank page. The export was rewritten by an old system wasm-opt
(binaryen 108) that trunk picked up because Trunk.toml pinned no tools.

The checker parses a built `*_bg.wasm` (Python 3 stdlib only) and resolves
the table each checked export points at. It checks:
  - the export that the sibling JS glue's `__wbindgen_init_externref_table`
    grows, whatever its name: the `wasm.<export>` its `const table = ...`
    reads (older wasm-bindgen called it `__wbindgen_export_<n>`), with that
    init's `grow(N)`;
  - `__wbindgen_externrefs`, when the init does not already name it.
It FAILS (exit 1) when any of these hold:
  - a checked export is table 0;
  - it is not a table, or its index is out of range;
  - the table is not an externref table;
  - the table's maximum leaves less room than the init's grow request
    (read from the init; if the glue has no init, from its
    `__wbindgen_externrefs; ... .grow(N)`, else `--grow N`, default 4);
  - the export the init grows is missing, or `__wbindgen_externrefs` is
    missing while the sibling JS glue uses it;
  - the glue names the init but the checker cannot read which export it
    grows, or by how much. That is a failure, never a pass. It includes:
      - the init's name, or any token containing it (a hashed variant such
        as `__wbg___wbindgen_init_externref_table_<hash>`, a quoted key, a
        comment), occurring more times than the definitions it parsed;
      - a grown variable that the init's body names more than once other
        than as `<var>.<member>` (a second assignment, destructuring, any
        other use); it must be read once, `<var> = wasm.<export>`;
  - the module has an externref table (or a table in the typed-reference
    encoding, whose heap type the checker does not record), but the export
    the init grows is not identified: the glue is there and has no init the
    checker could read, or there is no glue and `__wbindgen_externrefs` is
    not exported.
A module with no externref table (a build without reference types keeps its
externrefs in a JS heap instead) passes when the glue neither uses
`__wbindgen_externrefs` nor has an init. A `*_bg.wasm` with no sibling glue
is checked on the `__wbindgen_externrefs` export alone.

Usage:
  scripts/check-wasm-exports.py dist/                  # every *_bg.wasm in dist/
  scripts/check-wasm-exports.py path/to/app_bg.wasm    # explicit files
Exit: 0 every file passes, 1 a check failed, 2 usage or parse error.
"""

import argparse
import glob
import os
import re
import sys

PROG = "check-wasm-exports"
EXPORT_NAME = "__wbindgen_externrefs"
DEFAULT_GROW = 4

SEC_CUSTOM, SEC_IMPORT, SEC_TABLE, SEC_EXPORT = 0, 2, 4, 7
KIND_NAMES = {0: "func", 1: "table", 2: "memory", 3: "global", 4: "tag"}
KIND_TABLE = 1
REFTYPE_NAMES = {0x70: "funcref", 0x6F: "externref"}
EXTERNREF = 0x6F

# `const table = wasm.__wbindgen_externrefs; const offset = table.grow(4);`
GROW_RE = re.compile(r"__wbindgen_externrefs\s*;\s*const\s+\w+\s*=\s*\w+\.grow\(\s*(\d+)\s*\)")

INIT_NAME = "__wbindgen_init_externref_table"
# The init's definition in the JS glue, in the shapes wasm-bindgen has emitted:
#   __wbindgen_init_externref_table: function() {               (0.2.114, import object)
#   imports.wbg.__wbindgen_init_externref_table = function() {  (older --target web)
#   export function __wbindgen_init_externref_table() {         (older)
# and the method-shorthand and arrow forms. The match ends on the body's `{`.
INIT_DEF_RE = re.compile(r"(?<![\w$])" + INIT_NAME +
                         r"\s*(?:[:=]\s*)?(?:function\b\s*[\w$]*\s*)?\([^()]*\)\s*(?:=>\s*)?\{")
# Every token in the glue that contains the init's name: the name itself, and any
# variant of it, such as the hashed `__wbg___wbindgen_init_externref_table_<hash>`
# that wasm-bindgen 0.2.114 already gives other intrinsics. Anywhere: code, quoted
# keys, comments. The glue must not hold more of these than the checker parsed
# as definitions.
INIT_TOKEN_RE = re.compile(r"[\w$]*" + INIT_NAME + r"[\w$]*")
# `<receiver>.grow(N)` in the init's body: `table.grow(4)` or `wasm.<export>.grow(4)`.
BODY_GROW_RE = re.compile(r"(?<![\w$.])([\w$]+(?:\s*\.\s*[\w$]+)?)\s*\.\s*grow\s*\(\s*(\d+)\s*\)")
# `const table = wasm.<export>` (or let/var, or a bare assignment), ending the statement.
ASSIGN_RE = r"(?<![\w$.])%s\s*=(?![=>])\s*([\w$]+)\s*\.\s*([\w$]+)(?=[ \t]*(?:[;,\r\n}/]|$))"
# The grown variable anywhere in the init's body except as `<var>.<member>`: its
# declaration, any other assignment (`=`, `+=`, destructuring), any other use.
# The body must hold exactly one, the `<var> = wasm.<export>` read.
BARE_VAR_RE = r"(?<![\w$])%s(?![\w$])(?!\s*\.)"


class ParseError(Exception):
    pass


class GlueError(Exception):
    """The JS glue names the externref init, but its grown table can't be read."""


class Reader:
    def __init__(self, data, pos=0, end=None):
        self.data = data
        self.pos = pos
        self.end = len(data) if end is None else end

    def byte(self):
        if self.pos >= self.end:
            raise ParseError("unexpected end of data at offset %d" % self.pos)
        b = self.data[self.pos]
        self.pos += 1
        return b

    def uleb(self):
        result = shift = 0
        while True:
            b = self.byte()
            result |= (b & 0x7F) << shift
            if not b & 0x80:
                return result
            shift += 7
            if shift > 63:
                raise ParseError("LEB128 value too long at offset %d" % self.pos)

    def take(self, n):
        if self.pos + n > self.end:
            raise ParseError("unexpected end of data at offset %d" % self.pos)
        b = self.data[self.pos:self.pos + n]
        self.pos += n
        return b

    def name(self):
        return self.take(self.uleb()).decode("utf-8", errors="replace")

    def done(self):
        return self.pos >= self.end


def read_reftype(r):
    code = r.byte()
    if code in (0x63, 0x64):  # (ref null? <heaptype>), typed function references
        r.uleb()
        return code, "typed-ref"
    return code, REFTYPE_NAMES.get(code, "reftype 0x%02x" % code)


def read_limits(r):
    flags = r.byte()
    if flags & ~0x07:
        raise ParseError("unknown limits flags 0x%02x at offset %d" % (flags, r.pos - 1))
    minimum = r.uleb()
    maximum = r.uleb() if flags & 0x01 else None
    return minimum, maximum


def read_tabletype(r):
    code, label = read_reftype(r)
    minimum, maximum = read_limits(r)
    return {"code": code, "type": label, "min": minimum, "max": maximum}


def read_valtype(r):
    code = r.byte()
    if code in (0x63, 0x64):
        r.uleb()


def parse_module(data):
    """Return (tables, exports, producers); tables in index-space order."""
    if data[:4] != b"\0asm":
        raise ParseError("not a wasm module (bad magic)")
    if data[4:8] != b"\x01\x00\x00\x00":
        raise ParseError("unsupported wasm binary version %s" % data[4:8].hex())
    r = Reader(data, 8)
    imported, defined, exports, producers = [], [], [], []
    while not r.done():
        sid = r.byte()
        size = r.uleb()
        if r.pos + size > r.end:
            raise ParseError("section %d is truncated (size %d at offset %d)" % (sid, size, r.pos))
        s = Reader(data, r.pos, r.pos + size)
        r.pos += size
        if sid == SEC_CUSTOM:
            if s.name() == "producers":
                producers = parse_producers(s)
            continue
        if sid == SEC_IMPORT:
            for _ in range(s.uleb()):
                s.name()
                s.name()
                kind = s.byte()
                if kind == 0:
                    s.uleb()
                elif kind == 1:
                    t = read_tabletype(s)
                    t["imported"] = True
                    imported.append(t)
                elif kind == 2:
                    read_limits(s)
                elif kind == 3:
                    read_valtype(s)
                    s.byte()
                elif kind == 4:
                    s.byte()
                    s.uleb()
                else:
                    raise ParseError("unknown import kind 0x%02x" % kind)
        elif sid == SEC_TABLE:
            for _ in range(s.uleb()):
                if s.data[s.pos:s.pos + 1] == b"\x40":
                    raise ParseError("tables with an init expression are not supported")
                t = read_tabletype(s)
                t["imported"] = False
                defined.append(t)
        elif sid == SEC_EXPORT:
            for _ in range(s.uleb()):
                name = s.name()
                kind = s.byte()
                index = s.uleb()
                exports.append((name, kind, index))
        else:
            continue
        if not s.done():
            raise ParseError("section %d has %d trailing bytes" % (sid, s.end - s.pos))
    return imported + defined, exports, producers


def parse_producers(s):
    try:
        out = []
        for _ in range(s.uleb()):
            field = s.name()
            values = []
            for _ in range(s.uleb()):
                values.append((s.name() + " " + s.name()).strip())
            out.append("%s=%s" % (field, ", ".join(values)))
        return out
    except ParseError:
        return ["(unreadable producers section)"]


def describe(i, t):
    return "%d=%s min=%d max=%s%s" % (
        i, t["type"], t["min"], "none" if t["max"] is None else t["max"],
        " (imported)" if t["imported"] else "")


def read_glue(wasm_path):
    """The sibling wasm-bindgen JS glue: app_bg.wasm -> app.js (None if absent)."""
    base = wasm_path[:-len("_bg.wasm")] if wasm_path.endswith("_bg.wasm") else None
    if base is None or not os.path.isfile(base + ".js"):
        return None, None
    with open(base + ".js", encoding="utf-8", errors="replace") as f:
        return base + ".js", f.read()


def brace_body(text, open_pos):
    """The text inside the `{` at open_pos and its matching `}` (None if unbalanced)."""
    depth = 0
    for i in range(open_pos, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[open_pos + 1:i]
    return None


def init_grows(glue):
    """The (export name, grow request) pairs the glue's externref init grows.

    [] when the glue has no init. GlueError when it names the init but the
    export it grows, or the amount, can't be read: the caller fails on that.
    That includes any occurrence of the name, or a variant of it, beyond the
    definitions parsed, and a grown variable the body names more than once.
    """
    defs = list(INIT_DEF_RE.finditer(glue))
    tokens = INIT_TOKEN_RE.findall(glue)
    if len(tokens) > len(defs):
        counts = {}
        for t in tokens:
            counts[t] = counts.get(t, 0) + 1
        raise GlueError("the checker parsed %d definition%s of it, and that name or a variant "
                        "of it occurs %d time%s (%s)"
                        % (len(defs), "" if len(defs) == 1 else "s",
                           len(tokens), "" if len(tokens) == 1 else "s",
                           ", ".join("%s x%d" % (t, n) for t, n in sorted(counts.items()))))
    if not defs:
        return []
    grown = []
    for d in defs:
        body = brace_body(glue, d.end() - 1)
        if body is None:
            raise GlueError("its body has unbalanced braces")
        grows = list(BODY_GROW_RE.finditer(body))
        if not grows:
            raise GlueError("its body has no `<table>.grow(N)` call with a number N")
        for g in grows:
            receiver = re.sub(r"\s+", "", g.group(1))
            if "." in receiver:
                obj, name = receiver.split(".", 1)
            else:
                bare = re.findall(BARE_VAR_RE % re.escape(receiver), body)
                if len(bare) > 1:
                    raise GlueError("its body names `%s` %d times other than as `%s.<member>`, "
                                    "so it may assign it more than once; the variable it grows "
                                    "must be assigned once, from `wasm.<export>`"
                                    % (receiver, len(bare), receiver))
                assigned = list(re.finditer(ASSIGN_RE % re.escape(receiver), body[:g.start()]))
                if not assigned:
                    raise GlueError("its body grows `%s`, which it does not read from "
                                    "`wasm.<export>` first" % receiver)
                obj, name = assigned[0].group(1), assigned[0].group(2)
            if obj != "wasm":
                raise GlueError("its body grows `%s.%s`, not an export read from `wasm`"
                                % (obj, name))
            grown.append((name, int(g.group(2))))
    return grown


def check_table_export(name, exports, tables, grow, grow_src):
    """(failures, pass_line) for the table export `name`; None if it is not exported."""
    matches = [e for e in exports if e[0] == name]
    if not matches:
        return None
    failures, pass_line = [], None
    for _, kind, index in matches:
        if kind != KIND_TABLE:
            failures.append("%s is exported as a %s (index %d), not a table"
                            % (name, KIND_NAMES.get(kind, "kind 0x%02x" % kind), index))
            continue
        if index >= len(tables):
            failures.append("%s is exported from table %d, but the module has %d table(s)"
                            % (name, index, len(tables)))
            continue
        t = tables[index]
        where = "table %d (%s)" % (index, describe(index, t).split("=", 1)[1])
        if index == 0:
            failures.append("%s is exported from table 0 %s; table 0 is the function table, "
                            "so the init's grow(%d) throws RangeError"
                            % (name, where[len("table 0 "):], grow))
        if t["code"] != EXTERNREF:
            failures.append("%s is exported from %s, which is not an externref table"
                            % (name, where))
        if t["max"] is not None and t["min"] + grow > t["max"]:
            failures.append("%s is exported from %s, whose maximum leaves room for %d, "
                            "but the init grows it by %d (%s)"
                            % (name, where, t["max"] - t["min"], grow, grow_src))
        if not failures:
            pass_line = "%s -> %s; the init's grow(%d) (%s) fits" % (name, where, grow, grow_src)
    return failures, pass_line


def check_file(path, grow_option):
    """Return (failures, info_lines). Raises ParseError / OSError."""
    with open(path, "rb") as f:
        data = f.read()
    tables, exports, producers = parse_module(data)
    glue_path, glue = read_glue(path)
    glue_name = os.path.basename(glue_path) if glue_path else None

    info = ["tables: " + (", ".join(describe(i, t) for i, t in enumerate(tables)) or "none")]
    if producers:
        info.append("producers: " + "; ".join(producers))

    grow, grow_src = grow_option, "--grow" if grow_option is not None else "default"
    if grow is None:
        grow = DEFAULT_GROW
    if glue is not None:
        m = GROW_RE.search(glue)
        if m:
            grow, grow_src = int(m.group(1)), "JS glue " + glue_name

    failures, heads = [], []

    # 1. The export the glue's init actually grows, whatever it is called.
    grown = []
    if glue is not None:
        try:
            grown = init_grows(glue)
        except GlueError as e:
            failures.append("the JS glue %s names %s, but %s, so the checker can't tell "
                            "which table it grows" % (glue_name, INIT_NAME, e))
    checked = set()
    for name, n in grown:
        if name in checked:
            continue
        checked.add(name)
        result = check_table_export(name, exports, tables, n, "JS glue " + glue_name)
        if result is None:
            failures.append("%s is not exported, but %s in the JS glue %s grows it"
                            % (name, INIT_NAME, glue_name))
            continue
        failures.extend(result[0])
        if result[1]:
            heads.append(result[1])

    # 2. __wbindgen_externrefs, when the init did not already name it.
    # Tables that hold, or may hold, externrefs: externref, and the typed-reference
    # encodings (`(ref null extern)` among them), whose heap type is not recorded.
    externref_tables = [i for i, t in enumerate(tables)
                        if t["code"] == EXTERNREF or t["type"] == "typed-ref"]
    if EXPORT_NAME not in checked:
        result = check_table_export(EXPORT_NAME, exports, tables, grow, grow_src)
        if result is None:
            if glue is not None and EXPORT_NAME in glue:
                failures.append("%s is not exported, but the JS glue %s uses it"
                                % (EXPORT_NAME, glue_name))
            elif not checked and not externref_tables:
                heads.append("%s is not exported and no JS glue uses it" % EXPORT_NAME)
        else:
            failures.extend(result[0])
            if result[1]:
                heads.append(result[1])

    # 3. Fail closed: a module with an externref table passes only once the
    #    export the init grows is identified: read from the glue's init, or,
    #    with no glue next to the module, the export named __wbindgen_externrefs.
    #    A module with no externref table (built without reference types) has
    #    no such init.
    if externref_tables and not grown:
        where = "; ".join("table %d, %s" % (i, tables[i]["type"]) for i in externref_tables)
        if glue is not None:
            failures.append("the module has an externref table (%s), but the JS glue %s has no "
                            "%s the checker could read, so the export the init grows can't be "
                            "identified" % (where, glue_name, INIT_NAME))
        elif not any(e[0] == EXPORT_NAME for e in exports):
            failures.append("the module has an externref table (%s), but it has no JS glue "
                            "next to it and does not export %s, so the export the init grows "
                            "can't be identified" % (where, EXPORT_NAME))

    return failures, heads + info


def collect(paths):
    files = []
    for p in paths:
        if os.path.isdir(p):
            found = sorted(glob.glob(os.path.join(p, "*_bg.wasm")))
            if not found:
                raise ParseError("no *_bg.wasm in directory %s" % p)
            files.extend(found)
        elif os.path.isfile(p):
            files.append(p)
        else:
            raise ParseError("no such file or directory: %s" % p)
    return files


def main(argv):
    ap = argparse.ArgumentParser(
        prog=PROG,
        description="Fail a wasm-bindgen build whose __wbindgen_externrefs export is wrong.")
    ap.add_argument("paths", nargs="+", metavar="PATH",
                    help="a *_bg.wasm file, or a dist directory (every *_bg.wasm in it)")
    ap.add_argument("--grow", type=int, default=None, metavar="N",
                    help="the init's grow request when the JS glue does not state one "
                         "(default %d)" % DEFAULT_GROW)
    args = ap.parse_args(argv)

    try:
        files = collect(args.paths)
    except ParseError as e:
        print("%s: error: %s" % (PROG, e), file=sys.stderr)
        return 2

    failed = errored = False
    for path in files:
        try:
            failures, info = check_file(path, args.grow)
        except (ParseError, OSError) as e:
            print("%s: error: %s: %s" % (PROG, path, e), file=sys.stderr)
            errored = True
            continue
        if failures:
            failed = True
            for msg in failures:
                print("FAIL %s: %s" % (path, msg))
        else:
            print("PASS %s: %s" % (path, info[0]))
            info = info[1:]
        for line in info:
            print("  " + line)
    if errored:
        return 2
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
