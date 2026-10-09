# Known divergences from Nix 2.34

iets aims to give you exactly what Nix 2.34.8 gives. This page lists every
place where it knowingly does not. Each entry says what differs, why, and
how you can see it for yourself. The classic names add a few more, listed
in `docs/reference/classic.md`.

You can check most entries yourself. `checks.conformance` runs the cases of
nix-conformance against the answers Nix 2.34.8 gave. Among them is one case
for each entry of the regex and TOML corpora (`lang/corpora/`): 272 regex
pattern and input pairs through both `builtins.match` and `builtins.split`,
46 patterns both must reject, 77 TOML documents and 46 rejected ones.
`fuzz/conformance-known` lists the cases iets fails on purpose, each under
its section of this page.

## Regular expressions (`builtins.match`, `builtins.split`)

Nix compiles patterns with libstdc++ `std::regex` in `extended` (POSIX ERE)
mode. iets has its own engine, written from the behaviour Nix shows and
the POSIX spec. No divergence is known: the matches, the capture groups, the
accept/reject set and both error messages agree, including the
`memory limit exceeded by regular expression` limit (`a{99993}` compiles,
`a{99994}` does not).

## `builtins.fromTOML`

Only the error messages differ. Nix reads TOML 1.0 (toml11 with
`toml::spec::v(1, 0, 0)`). iets uses a parser for TOML 1.1, and first
refuses what only 1.1 allows: a newline or a trailing comma inside an
inline table, the `\e` and `\xHH` escapes, and a time with no seconds.
Under `parse-toml-timestamps`, datetimes print as Nix prints them (`T`
separator, 3, 6 or 9 fractional digits).

## `builtins.fromJSON`

Errors carry a message shaped like nlohmann's (`parse error at line L,
column C: ...`) but the `last read` detail is not reproduced.

Nesting deeper than 100,000 levels is refused, because the parser
recurses and a worker stack is 256 MiB. Nix has no limit.

## The `__json` derivation attribute

iets parses the text with the `fromJSON` parser and writes it again, so
the `.drv` holds the same normal form as Nix. That parser refuses a whole
number above 9223372036854775807, which Nix keeps as an unsigned value and
writes back unchanged. Only a hand-written `__json` can hold one:
`builtins.toJSON` cannot produce it.

## Bytes that are not UTF-8 in a derivation

A Nix string is bytes. Nix writes a derivation's strings into the `.drv`
as they are. In iets, an `env` value and an argument keep their bytes,
and the `.drv` path is the one Nix gives. Three places still differ:

- The name of an `env` entry, `builder` and `system` must be UTF-8. Nix
  takes any bytes there; iets's `derivationStrict` stops with `string is
  not valid UTF-8`. iets holds these three as text.
- `nix derivation show` writes each such byte as U+FFFD. Nix fails with
  `[json.exception.type_error.316]`.
- iets's daemon client reads the strings of a reply as UTF-8 and writes
  each byte that is not as U+FFFD. Two output names that differ only in
  such bytes are then one name. A daemon sends only valid output names
  (`[A-Za-z0-9+._?=-]`), so this happens only with a broken daemon, as in
  the wire fuzz target (`fuzz/divergences.toml`, `output-name-not-utf8`).

Observe it with `builtins.substring 0 1 "é"`, which is one byte that is not
UTF-8: as `builder = "/bin/sh" + …` the evaluation fails, as `v = …` it
gives `/nix/store/vhkdfhp7f0qfk8j0kx9b54hpn6cyg8m1-x.drv` like Nix.
`lang/eval-okay-derivation-non-utf8.nix` of nix-conformance makes one
derivation for each field; Nix 2.34.8 gives five drvPaths, iets the first
two and then the error. Such derivations exist in real stores: `drv/` of
nix-conformance holds six. `fuzz/conformance-known` lists the case.

## Reading a `.drv` file

iets reads a `.drv` as Nix 2.34.8 does, with these exceptions. The
`drv-read` and `drv-gen` fuzz targets find each one; `fuzz/divergences.toml`
names them.

- **Bytes that are not UTF-8** in an output name, in `system`, in
  `builder` or in the name of an `env` entry: iets stops with `non-UTF-8
  string in derivation`, Nix reads the file. This is the same limit as
  in the section above. Three of the six real files in `drv/` of
  nix-conformance (`u4`, `u5`, `u6`) hit it.
  (`drv-string-not-utf8`)
- **`__json`:** Nix reads the value as JSON and writes it again, with
  the keys sorted; a value that is not JSON stops it with `cannot process
  __json attribute`. iets keeps the text as it is. A `.drv` that Nix
  wrote holds the sorted form already, so only a `.drv` written by hand
  shows this. (`drv-json-not-checked`, `drv-json-kept-as-text`)
- **A `blake3` hash:** Nix needs the experimental feature
  `blake3-hashes`; in iets every feature is on (**Experimental-feature
  builtins** below). (`drv-blake3-without-feature`)
- **A store path with no dash after the hash**, such as
  `/nix/store/<hash>Xname`: Nix's `StorePath` checks the length, the hash
  characters and the name, but not the byte between them (`path.cc`).
  iets refuses the path with `missing dash`. (`store-path-without-dash`)
- **A store path that is not in canonical form**, such as
  `/nix/store/<hash>-name/` or `/nix/store//<hash>-name`: Nix's
  `parseStorePath` makes it canonical first (`canonPath`,
  `store-dir-config.cc`), so it reads the path. iets refuses it with `is
  not in the Nix store`. (`store-path-not-canonical`)
- **Two outputs with one name:** Nix keeps the first
  (`drv.outputs.emplace`, `derivations.cc`), iets keeps the last. This is
  an iets bug, not a choice. `drv-gen` gives each output its own name.
  Until the bug is fixed, the entry
  `drv-duplicate-output` hides every difference in the outputs.

## `builtins.genericClosure`

The result list and its order match. When two keys cannot be compared,
the error can name another pair, with other trace lines, because iets
keeps the keys in its own structure, not in a `std::map`.

## `builtins.break`

Nix returns a copy of the unforced argument thunk; inside a list,
`nix-instantiate --eval --strict` then prints that element as `<CODE>`
(`[ (builtins.break { a = 2; }) ]` gives `[ <CODE> ]`). iets forces the
argument and prints the value. Selecting from the result behaves the same
in both.

## A local binding of an operator's builtin

`parser.y` desugars `a - b` to `__sub a b`, `a * b` to `__mul a b`, `a / b`
to `__div a b`, `-a` to `__sub 0 a` and the four comparisons to
`__lessThan`. A lexical binding of one of those names therefore changes the
operator in Nix: `let __sub = a: b: 42; in 5 - 1` gives 42. iets compiles
the operators to their own operations and gives 4.

A `with` does not reach them in Nix either, because the four names are in
the base environment. Only a `let`, a `rec` set or a lambda parameter can
shadow them, and no file of the pinned nixpkgs binds any of the four. To
match Nix, iets would have to look up the scope for every arithmetic and
comparison operator when it compiles, and it does not.

## Nesting depth of a file

The parser recurses. Nix recurses too and reports
`stack overflow (possible infinite recursion)` from its SIGSEGV handler
when the C stack runs out. iets counts the nesting instead and gives the
same message above 50,000 levels, so no input makes it crash. Nix accepts
more: 400,000 nested lists parse there and stop only in the printer.

## A NUL byte outside a string

Nix's lexer gives each byte that no rule takes as a token of its own
(lexer.l, the last `{ANY}` rule), and the byte 0 is the end-of-input
token of the bison parser. So a NUL byte outside a string or a comment
ends the file for Nix: `1<NUL> + garbage` parses as `1`, and `{ a<NUL>`
is an unexpected end of file one byte after the NUL. iets refuses the
NUL with `unexpected invalid token` at the byte itself. Matching Nix
would make iets read part of a file in silence. Inside a string both
give the same `cannot be represented as Nix string` error, and in a
comment both ignore it.

`fuzz/divergences.toml` holds this case as `nul-outside-a-string`.

## Which of several clashes an error names

A file with two or more duplicate formal arguments, or two or more
attributes that clash when Nix merges nested sets, is refused by both,
with the same message. The name in the message can differ. Nix sorts the
formals by symbol to find a duplicate (`validateFormals`,
parser-state.hh) and merges a set's attributes in symbol order
(`addAttr`, the same file). A symbol's place in that order is the time
the evaluator first saw the name, and the names of the builtins come
first: in `{ lib, fetchurl, lib, fetchurl }: 1` Nix names `fetchurl`.
iets names a clash by its own symbol order. Matching Nix would mean
creating iets's symbols in Nix's order. With one clash both name the
same attribute at the same place.

`fuzz/divergences.toml` holds this case as `which-duplicate-formal`
and `which-duplicate-attribute`.

## Which attribute's error comes first

When two attributes of a set both fail, `==`, `builtins.deepSeq` and
`--strict` report the one they force first. When one attribute differs
and another fails, `==` gives `false` or the error by the same order.
Both evaluators go through a set in symbol order, as in the entry above:
a name's place is the time the evaluator first saw it.

Nix 2.34.8 creates 52 symbols before any other (`StaticEvalSymbols`,
eval.hh), among them `""`, `key`, `system`, `builder`, `args`, `meta`,
`self` and `description`. iets creates the same 52 first, in the same
order, so both report `key` here:

```nix
{ zz = throw "zz"; key = throw "key"; } == { zz = 1; key = 1; }
```

Three names still go the other way: iets creates `success`, `revCount`
and `version` when it starts, and Nix only when it first needs them. With
`success` in place of `key`, Nix reports `zz` and iets `success`. After
the 52, each evaluator also creates the names of its builtins in its own
order. When neither evaluator knew any of the names in advance, both
report the same attribute. `builtins.toJSON` and `builtins.attrValues`
go through the names in sorted order, so they agree.

## A `%` in an `or` argument

Nix warns when `or` is an argument (`f "x" or`: `or` will become an
identifier). The warning quotes the source and passes the text to
`warn` as a format string (`ExprCall::warnIfCursedOr`, nixexpr.cc). A
`%` in the quoted text then fails in boost: `f "%" or` gives
`error: boost::bad_format_string: format-string is ill-formed`, and
`f "%d" or` gives `boost::too_few_args`. With more `%` signs a boost
assertion fails and Nix aborts. This is a bug of Nix 2.34.8.
iets parses both, as Nix does without the `%`.

`fuzz/divergences.toml` holds this case as `percent-in-a-cursed-or` and
`percent-in-a-cursed-or-abort`.

## Unforced values in lazy positions

In a list element, an attribute value, a `let` binding or a function
argument, Nix makes a thunk of every expression except a literal and an
already evaluated variable (`Expr::maybeThunk`). iets keeps a value there
instead of a thunk when it costs nothing to have it: a lambda, a folded
`builtins.x`, an attribute of an attrset that is already forced, and `+`
or `-` on ints that are already forced. The results are the same; only a
printer that stops at thunks can see it. `nix-instantiate --eval` without
`--strict` prints `[ builtins.nixPath (1 + 1) (x: x) ]` as
`[ <CODE> <CODE> <CODE> ]`, iets as `[ [ ] 2 <LAMBDA> ]`. These are
speed shortcuts, kept on purpose; `--strict`, `--json` and `nix eval`
print the same.

`==` can see them too. In Nix, two elements that come from one slot are
equal without a look inside, and a function equals only the function in
its own slot. The two operands of `==` are fresh copies, so that shortcut
never applies to them. iets uses a faster rule at every level: two values
that are the same after forcing are equal. It makes one exception: two
functions are equal only when they were the same before forcing. So
`[ (id id) ] == [ (id id) ]` and `with foo; [ bar ] == [ bar ]` are
false, as in Nix.

What stays different:

- `let x = { a = throw "a"; }; in x == x` is true, where Nix throws.
- A function reached through one of the shortcuts above still equals
  itself: `[ builtins.add ] == [ builtins.add ]`,
  `{ inherit (builtins) import; } == { inherit (builtins) import; }` and
  `builtins.elem builtins.map [ builtins.map ]`.
- Two thunks that give the same function, and that are both forced in
  place, count as one slot.

The answer is the same at every worker count. Nix's exact rules would cost
about 1% of the instructions of a NixOS evaluation, and make no difference
on nixpkgs.
The eval fuzz target can switch on Nix's exact rules, to tell this
divergence apart from other differences (`eq-rule`,
`docs/reference/fuzz.md`).

## `«repeated»` for a list

Sets match Nix exactly. A list can print twice where Nix prints
`«repeated»`: `let l = [ 1 2 ]; in { a = l; b = l; }` gives
`{ a = [ 1 2 ]; b = [ 1 2 ]; }` in iets and `{ a = [ 1 2 ]; b = «repeated»;
}` in Nix.

Nix spots a repeated list by the `Value` that holds it
(`print-ambiguous.cc`), because a small list lives inside its `Value`.
iets has no such `Value`, so it uses the slot. Using the list itself would
only turn the difference around, because Nix prints `let x = [ x ]; in x`
as `[ [ «repeated» ] ]`, which needs two slots for one list.

## The digits of a float in `toJSON`

`builtins.toJSON` writes a float with the fewest digits that read back to
the same double. Nix writes it with nlohmann's JSON library, whose Grisu2
algorithm gives that shortest form for most doubles but not for all:
`builtins.toJSON 4.200000000001e17` is `"4.2000000000009997e+17"` in Nix
and `"4.200000000001e+17"` in iets. Both read back to the same double, but
the bytes differ, so a JSON file that holds such a float has another hash.
To match Nix, iets would need a port of Grisu2.

`fuzz/divergences.toml` holds this case as `json-float-digits`.

## The limits of a run

Both evaluators stop on a program that asks for too much, with the same
text and exit code but at another point.

`error: stack overflow (possible infinite recursion)`, exit 1. Nix's stack
is the thread's, so `ulimit -s` sets the depth; iets gives the demand
path 4 GiB and takes a much deeper recursion. A file of 400,000 `1 + 1`
terms is Nix's error and iets's answer.

`error: out of memory`, exit 1. Nix prints one or more
`GC Warning: Out of Memory!` lines first, and iets adds its usual
`while calling the …` trace.

`error: too many symbols`, exit 1. Above 2^30 attribute names in one run
iets stops; Nix has no limit of its own.

## Experimental-feature builtins

Nix leaves `fetchTree` out of `builtins` unless `fetch-tree` or `flakes`
is on, `fetchClosure` unless `fetch-closure` is, and `outputOf` unless
`dynamic-derivations` is (`experimental-features` in nix.conf or
`--extra-experimental-features`). Under one of Nix's own names iets does
the same, so `builtins.attrNames builtins` is the same list on both sides
whatever the features are. Its own commands have
every feature, so `builtins ? fetchTree` is true there where nix with no
feature on says false, and code that branches on it (flake-compat) takes
the `fetchTree` branch. `builtins.fetchClosure` is unsupported either way,
so classic mode falls back when it is called.

`builtins.outputOf` works. The one thing a run cannot do is read back a
producer that it did not make and that is not in the store. Nix writes
each `.drv` as it evaluates, so it always finds one. iets writes nothing.
It keeps a producer this run made in memory when its output is text
addressed, which every dynamic derivation is. Any other producer is read
from `/nix/store`, and the call fails if it is not there.

Because iets answers from the derivation it holds in memory, `outputOf`
works even when nothing is written. `nix-eval-jobs --no-instantiate` over
a jobset that uses it succeeds in iets. Nix, without the producer's `.drv`
in the store, reports `path '…-cargo-vendor.drv.drv' is not a valid store
path` for each such attribute: 33 of 199 jobs on repkgs.

`builtins.getContext` of a string whose context is a dynamic derivation
output is an error. Nix resolves the derived path first, which means
building the producer, and evaluation does not build.

## Flake references (`parseFlakeRef`, `flakeRefToString`, the command line)

iets parses and renders the common schemes like Nix 2.34.8 (flakeref.cc,
url.cc and the libfetchers schemes): indirect ids,
`flake:`, `github:`, `gitlab:`, `path:` and absolute paths, `git:` and
`git+…`, `tarball+…`, `file+…` and plain `http(s)`/`file` URLs, with the
query parameters each scheme keeps. For valid input the output is Nix's.

iets accepts some flake refs Nix rejects. The checks whose only effect
is to refuse invalid input are left out, so these parse or render where
Nix fails:

- a character boost.url does not allow where it stands (`github:a/b/x y`,
  `https://x.org/é`), and a `%` followed by `+` and a hex digit;
- a branch or tag name libgit2 refuses (`a..b`, `-x`), a `rev` next to a
  `ref`, the same parameter twice in the path and the query, an indirect
  id or a `host` outside Nix's patterns, a third indirect component that
  is not a commit hash;
- an attribute or a query parameter the scheme does not allow
  (`flakeRefToString { type = "github"; owner = "a"; repo = "b"; foo = 1; }`
  gives `github:a/b`; `path:/a?x=1` keeps `x`), a `revCount` or
  `lastModified` that is no number (dropped), an attribute of the wrong
  type (ignored; `path` prints it), a `rev` or `narHash` Nix cannot
  parse in `parseFlakeRef` (kept as given), and a `narHash` that is not
  SHA-256 SRI in `flakeRefToString` (printed as given);
- a `file://` URL with a host, a path component with an encoded `/`, and
  a `path` whose URL starts with `///` (Nix aborts on it);
- a string that is not UTF-8 or decodes to a NUL byte (read lossily).

Where iets still errors, the wording is its own. Classic mode falls back
to Nix for each of these:

- `sourcehut:`, `hg+…` and any other scheme or type;
- a URL authority with a character that is not unreserved (an IP literal
  `[::1]`, a `%XX` escape, a sub-delim), and a port that is not 1-65535;
- a git `url` in `flakeRefToString` that is not a URL (scp-style
  `git@host:x`, a plain path).

`flakeRefToString` on a git input leaves out `keytype`, `publicKey` and
`publicKeys`, which Nix renders only with the `verified-fetches`
experimental feature on.

## `--reference-lock-file`

Nix locks the flake again with the given file as the starting point, so it
can lock a missing or changed input in memory. iets takes the file as the
lock, which must be complete for the inputs `flake.nix` declares, like the
flake's own `flake.lock`. Neither writes the file. The path belongs to the
tracing-cache key (`docs/reference/classic.md`).

## Tarball inputs

The tree of an archive input is the git sink's
(`docs/reference/flakes.md`), member for member: a `..` in a name, a
member below a symlink and an unknown type flag give what Nix gives, not
an error. Nix's other archive path, `nix store prefetch-file --unpack`,
refuses those three, so `iets prefetch --unpack` takes archives that
command refuses. One tree, one rule, and it is the rule a `narHash` in a
lock file was computed with.

Two limits have no counterpart in Nix, because Nix has no setting for
either. `fetch-max-size` bounds the spool and everything a sparse member
expands to together, so an archive of ten kilobytes cannot write two
gigabytes. A stream with over eight layers of compression is refused;
libarchive stacks its filters without a bound.

## `revCount` of a source tree

The value of `revCount` is that of Nix, and the attribute is there when
Nix gives one: a clean tree with its full history. Two things differ.

- **A read gives a warning.** `revCount` is deprecated in iets. The lint
  `rev-count` (`docs/reference/lints.md`) reports a read of
  `self.revCount` on stderr, at the level `warn` by default. The
  `revCount` of a locked input is silent unless `[lint.inputs.<node>]`
  gives the lint a level. Nix gives no warning. stdout, the `.drv` paths
  and the exit code do not change.
- **The count is computed when the evaluation reads it.** Nix 2.34.8
  counts the revisions of a git working tree before the evaluation
  (git.cc:getAccessorFromWorkdir). iets counts when the evaluation reads
  the attribute, as Nix 2.35 does (fetchTree.cc:emitLazyAttrThunk). So an
  error of the count (a repository that lacks a commit) comes at the read,
  not at the start. An evaluation that does not read the attribute has no
  such error.

Observe it: in a git repository, evaluate `toString self.revCount` from a
flake with `iets eval` and with `nix eval`. stdout is the same, and only
iets prints `warning: [rev-count] 'self.revCount' is deprecated: ...`.
`--lint rev-count=off` removes the line. `scripts/flaketest.nu` compares
the value, `self ? revCount` and the attribute names with Nix on a clean
tree, a dirty tree and a shallow clone.

## Pure evaluation of a project with no flake

A native command evaluates purely by default, where `nix-instantiate`,
`nix-build` and `nix-shell` do not, and a pure run reads below its project:
the working tree the entry file is in, else the nearest `flake.nix`
directory, else the outermost `iets.toml` one, else the entry's own
directory (`docs/explanation/flakes-and-sources.md`). Nix has no such root. The
nearest thing it has is `restrict-eval` with `allowed-uris`, which iets does
not implement.

Three things follow, and `nix` needs `--impure` to be compared against any
of them:

| | Nix, pure | iets, pure |
|:---|:---|:---|
| `builtins.readFile ./x` with no flake | forbidden | the file, when `./x` is in the project |
| `toString ./x` under the root flake or project | a store path | a store path under a flake, the checkout path under a project |
| `"${./x}"` under the root flake or project | a path inside the tree copy | the same under a flake; one copy of that path alone under a project |

The last two are what keeps a position, an error message and a
`toString` naming the file the user edits. A `.drv` that holds such a path
is the case that does not survive another checkout location, so
`impure-path-in-drv` and `absolute-path-in-drv` are errors by default
(`docs/reference/lints.md`).

The user setting `pure = false` restores Nix's defaults for the native
commands. A project `iets.toml` may not set it
(`docs/reference/settings.md`).

## Daemon protocol versions

Nix 2.34.8's client talks to a daemon of protocol 1.18 or newer
(`WorkerProto::minimum`, worker-protocol.cc). iets needs 1.35 and refuses
an older daemon at the handshake with
`the Nix daemon speaks protocol 1.N; this client needs 1.35 or newer`.
Older daemons lack the trust flag and some reply shapes this client
relies on. The `wire` fuzz target (`docs/reference/fuzz.md`) therefore makes
daemons of 1.35 and newer only.

## Padding in a NAR from the daemon

For `NarFromPath`, Nix 2.34.8's client copies the NAR with `copyNAR`
(src/libutil/archive.cc). It parses into a `NullFileSystemObjectSink`,
which skips the contents of a file with their padding and does not check
that the padding is zero. iets reads the NAR in full and refuses
`bad NAR: non-zero padding`, as Nix does when it unpacks the same NAR
(`restorePath`). The wire fuzz target lists the case as
`nar-content-padding`.

## A store path without a dash

Nix 2.34.8 takes the name of a store path from the bytes after the 32
characters of the hash part, and does not check that the byte between them
is `-` (path.cc:StorePath::StorePath). So `/nix/store/<hash>6n-` is a
valid store path to Nix, with the name `n-`. iets refuses it with
`missing dash`. The wire fuzz target lists the case as
`store-path-without-a-dash`.

## Classic mode

`nix-instantiate`, `nix-build` and `nix-eval-jobs` under Nix's own names
diverge in a few more places (store `auto`, `-Q`, `--show-trace`,
`--no-out-link`, `getDerivations` recursion, `--add-root` clobbering).
They are listed in `docs/reference/classic.md`.

## The tracing cache under the classic names

`nix eval`, `nix build`, `nix derivation show`, `nix develop`,
`nix print-dev-env`, `nix search` and `nix fmt` answer from iets's tracing
cache (`docs/explanation/caching.md`, `docs/reference/classic.md`). Nix has
an evaluation cache of its own for flakes, so both skip evaluations, but
not the same ones: Nix's is keyed on the locked fingerprint and is off for a
dirty tree, `-f` and `--expr`; iets's is keyed on the command and validated
by what the run read. stdout and the exit code of a hit are the bytes and
the code of the run that wrote the entry, which `scripts/classic-e2e.nu`
compares with Nix 2.34.8.

Two things differ on stderr.

- **An invalidation is silent.** The native commands say
  `re-evaluating: PATH changed` when an entry no longer holds. Nix says
  nothing when its cache misses, and the classic names print what Nix prints,
  so under them the line does not reach stderr, in text or under
  `--log-format internal-json`. The record (level 3, code
  `cache-invalidated`) still goes to the `json-log-path` file, which is where
  to look when a classic command evaluated and you want to know why.
- **A hit prints no `trace:` and no `evaluation warning:` line.**
  `builtins.trace` and `builtins.warn` write while the evaluation runs, and a
  hit does not run one. Nix behaves the same way when its cache answers
  (`nix build`, `nix develop`). It differs for `nix eval`, which Nix never
  answers from its cache, so there every run prints them and only iets's
  first run does.

Observe it: run `nix eval .#x` twice on a flake whose `x` is
`builtins.trace "evaluated" 1`. The first run prints `trace: evaluated` and
`1`, the second prints `1`. Edit `flake.nix` and the trace line is back, with
no `re-evaluating:` line before it; `--option json-log-path FILE` puts the
record in `FILE`.

## Flags and settings with no effect

iets does not accept a flag or a setting in silence. Each one that has no
effect falls into one of four classes:

1. **It holds by construction.** Nix's behaviour is already what the flag
   asks for. No message.
2. **iets implements it**, or sends it to the daemon as a Nix client does.
3. **A classic name falls back** to the real nix
   (`docs/reference/classic.md`).
4. **One warning per key and process**, with the reason. Nix prints nothing
   here, so each warning is a divergence on stderr. stdout and the exit
   code do not change. The record is level 1 with the code `ignored-flag`
   or `ignored-setting` (`docs/reference/errors.md`). It appears under
   `--log-format internal-json` and in the `json-log-path` file. An
   `eval-jobs` worker does not repeat the warning of its parent. The warning
   comes from the command line and the settings, not from the evaluation,
   so a run that the tracing cache answers gives it too, under the classic
   names as well: it is not the `re-evaluating:` line that they keep off
   stderr. A warning
   can come before a fallback: the run had read the flag before it found
   the reason to fall back.

The flags of class 4:

| Flag | Where | The reason in the warning |
|:---|:---|:---|
| `-v`, `--verbose` | `nix-instantiate`, `nix-build`, `nix-shell`; `-vv` and more fall back | iets has one log level, the one Nix starts at |
| `--verbose`, `--debug` | `eval-jobs` | the same |
| `--accept-flake-config` | `nix` | iets never applies a flake's `nixConfig` (see below) |
| `--eval-store auto`, `daemon`, `""` | `nix`; any other value falls back | iets evaluates in memory and needs no evaluation store |
| `--readonly-mode` | `iets build`, `eval-jobs`, `print-dev-env` | the command writes to the store; `eval` and `eval-jobs --no-instantiate` do not |

The flags of class 1, and why they hold:

| Flag | Why |
|:---|:---|
| `--indirect`, `--add-drv-link` | Nix does nothing with them either (`nix-build.cc`: "obsolete"; `nix-instantiate.cc`: an empty statement). |
| `--no-gc-warning` on `nix-build`, `nix-shell` | Only `nix-instantiate` prints that warning. `nix-instantiate` implements the flag. |
| `--no-location` | It changes `--xml` output only, and `--xml` falls back. |
| `--readonly-mode` on `iets eval` | `iets eval` writes to no store. |
| `-j`, `--cores`, `-k`, `-K` on `nix eval`, `nix-instantiate`, `nix derivation show`, `eval-jobs` | These commands build nothing: iets never builds during evaluation. The building commands send the four values to the daemon. |
| `--experimental-features`, `--extra-experimental-features` | Every feature is always on (**Experimental-feature builtins** above). `parse-toml-timestamps` is the exception: `iets eval` applies it, and a classic `nix` falls back. |
| `--show-trace`, `--log-format bar` | iets always prints the full trace, and renders builds its own way; `docs/reference/classic.md` lists both. |

Two flags moved from "ignored" to class 2 or 3:

- `nix develop -j N`, `--cores N`, `-k`: the values did not reach the
  daemon that builds the shell. They do now.
- `nix-instantiate --store URL` wrote to the local store when `URL` named
  another daemon. It falls back now; `--eval` does not need a store and
  stays.

### Settings

`docs/reference/settings.md` has the rules. In short:

- A name that Nix 2.34.8 does not know gives Nix's own text, `warning:
  unknown setting 'NAME'`, with the code `unknown-setting`. This is not a
  divergence.
- A setting that acts in the store goes to the daemon, as a Nix client
  sends it: from the user files, `$NIX_CONFIG` and `--option`, not from the
  system `nix.conf`, which the daemon reads itself.
- A setting that acts in the Nix client and that iets has nothing for
  counts when its value is not Nix's default: `restrict-eval`,
  `allowed-uris`, `eval-system`, `max-call-depth`, `allow-dirty`,
  `trace-verbose`, `abort-on-warn` and the rest of class 4 under "What is
  applied" in `docs/reference/settings.md`. `nix-instantiate`,
  `nix-build`, `nix-shell` and `nix` fall back, so the real nix applies
  it. iets's own commands and `nix-eval-jobs` cannot, and give one
  `ignored-setting` warning. `accept-flake-config` and `log-lines` give
  the warning everywhere.
- iets's own commands (`iets eval`, `build`, `eval-jobs`,
  `print-dev-env`, `diff`) send a daemon no settings. A store setting given
  to them with `--option` is one `ignored-setting` warning. A `grpc://`
  builder has no settings either, so a classic name gives the same
  warning for it.

### A flake's `nixConfig`

Nix applies the `nixConfig` attribute of the top-level flake: always for a
short list of harmless settings (`bash-prompt` and its two relatives,
`flake-registry`, `commit-lock-file-summary`), for the others after a
question, a saved answer in `~/.local/share/nix/trusted-settings.json`, or
`--accept-flake-config`. Without any of these it prints `warning: ignoring
untrusted flake configuration setting 'NAME'`.

iets never reads `nixConfig`. This is the safe side of the security
decision that `--accept-flake-config` makes: no flake can change a setting.
But a flake that needs its `nixConfig` behaves differently:

- `extra-substituters` and `extra-trusted-public-keys` are not used, so
  iets builds what Nix would download.
- `allow-import-from-derivation`, `sandbox` and the other settings keep the
  value of `nix.conf`.
- `bash-prompt` does not change the prompt of `nix develop`.
- iets does not print Nix's `ignoring untrusted flake configuration
  setting` warning.

`--accept-flake-config` and `accept-flake-config = true` each give one
warning that says so. Put the settings in `nix.conf` to get them.

Observe it: `nix eval --accept-flake-config .#x` on a flake with
`nixConfig.extra-substituters`. stdout and the exit code are Nix's, and
stderr has the `ignored-flag` warning. `scripts/classic-e2e.nu` has the
case.

## GC roots the shells and the builds keep

`nix-shell`, `nix develop` and `nix fmt` keep a GC root of the closure they
entered or built, and `nix-build --no-out-link` (`nix build --no-link`)
keeps one of every output it was told not to link. Nix keeps none of these:
after its run the output is collectable, which is why the next entry
rebuilds it and why nix-direnv and flake-fmt exist. The roots live in
`~/.cache/iets/roots`, with a 30-day TTL and `iets cache gc`
(`docs/explanation/caching.md`); `shell-gc-root` and `build-gc-root` turn
them off (`docs/reference/settings.md`).

`--no-out-link` is the deliberate one: the flag means "no symlink here",
and iets reads it as "no symlink here", not as "let this output go". That
is the workflow the flag is for — build something, use the path it printed,
keep no `result` in the tree — and in Nix the output can be collected while
you are still using it. The cost is disk: a `--no-out-link` build now pins
its output until the TTL sweeps the link.

Observe it: `nix-build --no-out-link -E '<drv>'` prints the same path and
the same exit code as Nix, with no symlink on either side, and then
`nix-store --query --roots <path>` names a link under
`~/.cache/iets/roots` for iets's run and nothing for Nix's. stdout,
stderr and the exit code are unchanged — a root is a side effect on the
daemon — which is why `scripts/classic-e2e.nu` can compare both runs byte
for byte and check the root in the same case.

## Errors, exit codes and `--log-format internal-json`

`docs/reference/errors.md` has the codes and the record format. Where it differs from
Nix 2.34:

- **Build exit codes.** `iets build` exits 100, 101, 102 or 104 on a
  failed build (`failingExitStatus`) through a daemon, where Nix exits 1.
  Nix computes these codes only when it builds in its own store. The
  classic names exit 1, as Nix does as a daemon client.
- **Usage errors of native commands** exit 2. Nix exits 1; under a
  classic name iets does too.
- **`msg`** holds iets's plain text: no ANSI colours, no source
  excerpt. `raw_msg`, `line`, `column` and `file` are the fields to
  compare; they differ from Nix where the text differs. An error that
  Nix reports without a position and with the position in its trace,
  such as `throw`, has the position here.
- **`code` and `hint`** are extra keys; Nix has neither.
- **A failed build's record** has the position of the derivation; Nix's
  has none.
- **eval-jobs** puts out a record for each failed job; nix-eval-jobs
  prints those errors as plain text even under `internal-json`.
- **Activity records** (`start`, `stop`, `result`, docs/reference/errors.md) have
  Nix's shape, and nix-output-monitor reads them. The daemon's
  activities during a build are the same as in Nix: realise, builds,
  copy paths, one build activity per derivation, and the same result
  types. `scripts/classic-e2e.nu` compares that sequence with Nix 2.34.8
  for `nix-build` and `nix build`. What differs:
  - Ids are new ids of the iets process; Nix passes the daemon's ids on.
  - Nix sends `QueryMissing` before a build. So Nix has a `querying info
    about missing paths` activity and the substituter queries first,
    then `these N derivations will be built:`. iets does not send it:
    the queries come inside the build op, and no plan text is printed.
    `iets build --dry-run` prints the plan.
  - Nix writes the `.drv` files during evaluation, with no activity.
    iets uploads them before the build, under `copying N paths` and
    `copying path` activities, as `nix copy` does. Nix's copy activities
    also report the bytes of each path; iets's report the paths only.
  - Under `internal-json` the build renderer's lines (start, phase, log
    block, verdict, heartbeat) are not put out; the records carry them.
  - With a `grpc://` builder iets makes the records itself. There is
    one build activity per derivation from the start of its scheduling,
    and a derivation the cache already has also gets one. Its upload is
    one `copying N paths` activity, with no `copying path` under it.
  - The evaluation is an `evaluating …` activity of type 0
    (`actUnknown`). Nix has none.
  - A flake input iets downloads itself has a `downloading '<url>'`
    activity (`actFileTransfer`), with the compressed bytes as its
    progress. A `git` or `hg` input that the nix on `PATH` fetches has
    a `fetching input '<name>' with nix` activity (`actFetchTree`), and
    that nix's own records come under it.
  - The `--timings` and `IETS_STATS` lines stay plain text.
- **`iets build --dry-run --json`** prints the plan (`willBuild`,
  `willSubstitute`, `unknown`, `downloadSize`, `narSize`). `nix build
  --dry-run --json` prints the installables' derived paths. The text on
  stderr is the same as Nix's.
- **`json-log-path`** applies to every command. Nix applies it to the
  `nix` command only: the classic names return before `applyJSONLogger`
  (`src/nix/main.cc`).
- **A failed write on stdout** gives `error: write of N bytes: REASON`
  and exit 1, as it does in Nix, but `N` is the bytes of the one write
  that failed. iets writes a value and its newline together, so its
  count can be one more than Nix's. `nix-instantiate` and `nix-build`
  exit 0 on a failed write in both, because `std::cout` never reports.
