# Known divergences from Nix 2.34

Every place where ietsp knowingly differs from Nix 2.34.8, and how to
observe it.

Each entry says what differs, why, and how to observe it. The classic
names add a few more, listed in `docs/reference/classic.md`.

`scripts/parity.nu` runs the differential corpora under `tests/parity/`
(272 regex pattern/input pairs through both `builtins.match` and
`builtins.split`, 46 patterns both sides must reject, 77 TOML documents
and 46 rejected ones) against the real `nix-instantiate` and requires
byte-equal `--strict --json` output. It needs a nix binary, so it is not
a flake check; run it by hand in the dev shell. `NIX=` and `IETSP=`
override either side.

## Regular expressions (`builtins.match`, `builtins.split`)

Nix compiles patterns with libstdc++ `std::regex` in `extended` (POSIX ERE)
mode. ietsp has its own engine (`src/builtins/text/regex.rs`), written from
the behaviour Nix shows and the POSIX spec. Its header lists the observed
rules. No divergence is known: the matches, the capture groups, the
accept/reject set and both error messages agree, including the
`memory limit exceeded by regular expression` limit (`a{99993}` compiles,
`a{99994}` does not).

## `builtins.fromTOML`

Nix uses toml11 with `toml::spec::v(1, 0, 0)`; ietsp uses the `toml`
crate, which implements TOML 1.1 and has no switch for the older spec. A
scanning pass (`reject_1_1` in `src/builtins/text/fromtoml.rs`) refuses
the 1.1-only syntax first: a newline or a trailing comma inside an inline
table, and the `\e` and `\xHH` escapes. Omitted seconds are caught on the
parsed value, which the crate reports as `second: None`. Only the error
messages differ. Datetime rendering under `parse-toml-timestamps` follows
Nix (`T` separator, 3/6/9 fractional digits).

## `builtins.fromJSON`

Errors carry a message shaped like nlohmann's (`parse error at line L,
column C: ...`) but the `last read` detail is not reproduced.

## `builtins.genericClosure`

The key set is a sorted vector with binary search instead of a
`std::map`, so which pair of elements triggers a `cannot compare` error
(and the error's trace lines) can differ. The result list and its order
match.

## `builtins.break`

Nix returns a copy of the unforced argument thunk; inside a list,
`nix-instantiate --eval --strict` then prints that element as `<CODE>`
(`[ (builtins.break { a = 2; }) ]` gives `[ <CODE> ]`). ietsp forces the
argument and prints the value. Selecting from the result behaves the same
in both.

## Unforced values in lazy positions

In a list element, an attribute value, a `let` binding or a function
argument, Nix makes a thunk of every expression except a literal and an
already evaluated variable (`Expr::maybeThunk`). ietsp keeps a value there
instead of a thunk when it costs nothing to have it: a lambda, a folded
`builtins.x`, an attribute of an attrset that is already forced, and `+`
or `-` on ints that are already forced (`Evaluator::maybe_thunk`,
`Compiler::fold_builtins_select`). The results are the same; only a
printer that stops at thunks can see it. `nix-instantiate --eval` without
`--strict` prints `[ builtins.nixPath (1 + 1) (x: x) ]` as
`[ <CODE> <CODE> <CODE> ]`, ietsp as `[ [ ] 2 <LAMBDA> ]`. These are
speed shortcuts, kept on purpose; `--strict`, `--json` and `nix eval`
print the same.

`==` can see them too. Nix's `eqValues` forces both sides, then takes two
elements from one slot as equal without looking inside, and a function
equals only its own slot; the two operands of `==` are fresh copies, so
that shortcut never applies to them. ietsp keeps its faster rule, equal
forced words are equal, at every level, and makes one exception: a
function is equal only when the words before forcing are the same, so
`[ (id id) ] == [ (id id) ]` and `with foo; [ bar ] == [ bar ]` are false,
as in Nix. What stays different: `let x = { a = throw "a"; }; in x == x`
is true where Nix throws, and a function reached through a shortcut above
still equals itself (`[ builtins.add ] == [ builtins.add ]`, `{ inherit
(builtins) import; } == { inherit (builtins) import; }`, `builtins.elem
builtins.map [ builtins.map ]`). Forcing a cell writes its value over the
thunk, so two thunks that give the same function and are both forced in
place look like one slot. Nix's exact rules cost about 1% of the
instructions of a NixOS evaluation, for no difference in nixpkgs.

## Experimental-feature builtins

Nix leaves `fetchTree` out of `builtins` unless `fetch-tree` or `flakes`
is on, `fetchClosure` unless `fetch-closure` is, and `outputOf` unless
`dynamic-derivations` is (`experimental-features` in nix.conf or
`--extra-experimental-features`). ietsp always has all three, so
`builtins ? fetchTree` is true under ietsp where nix, with no feature
on, says false; code that branches on it (flake-compat) takes the
`fetchTree` branch. `builtins.fetchClosure` and `builtins.outputOf` are
unsupported either way, so classic mode falls back when they are called.

## Flake references (`parseFlakeRef`, `flakeRefToString`, the command line)

`src/flake/flakeref.rs` parses and renders the common schemes like Nix
2.34.8 (flakeref.cc, url.cc and the libfetchers schemes): indirect ids,
`flake:`, `github:`, `gitlab:`, `path:` and absolute paths, `git:` and
`git+…`, `tarball+…`, `file+…` and plain `http(s)`/`file` URLs, with the
query parameters each scheme keeps. For valid input the output is Nix's.

ietsp accepts some flake refs Nix rejects. The checks whose only effect
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

Where ietsp still errors, the wording is its own. These are
`ErrorKind::Unsupported`, so classic mode falls back:

- `sourcehut:`, `hg+…` and any other scheme or type;
- a URL authority with a character that is not unreserved (an IP literal
  `[::1]`, a `%XX` escape, a sub-delim), and a port that is not 1-65535;
- a git `url` in `flakeRefToString` that is not a URL (scp-style
  `git@host:x`, a plain path).

`flakeRefToString` on a git input leaves out `keytype`, `publicKey` and
`publicKeys`, which Nix renders only with the `verified-fetches`
experimental feature on.

## Classic mode

`nix-instantiate`, `nix-build` and `nix-eval-jobs` under Nix's own names
diverge in a few more places (store `auto`, `-Q`, `--show-trace`,
`--no-out-link`, `getDerivations` recursion, `--add-root` clobbering).
They are listed in `docs/reference/classic.md`.

## GC roots the shells and the builds keep

`nix-shell`, `nix develop` and `nix fmt` keep a GC root of the closure they
entered or built, and `nix-build --no-out-link` (`nix build --no-link`)
keeps one of every output it was told not to link. Nix keeps none of these:
after its run the output is collectable, which is why the next entry
rebuilds it and why nix-direnv and flake-fmt exist. The roots live in
`~/.cache/ietsp/roots`, with a 30-day TTL and `ietsp cache gc`
(`docs/explanation/caching.md`); `shell-gc-root` and `build-gc-root` turn
them off (`docs/reference/settings.md`).

`--no-out-link` is the deliberate one: the flag means "no symlink here",
and ietsp reads it as "no symlink here", not as "let this output go". That
is the workflow the flag is for — build something, use the path it printed,
keep no `result` in the tree — and in Nix the output can be collected while
you are still using it. The cost is disk: a `--no-out-link` build now pins
its output until the TTL sweeps the link.

Observe it: `nix-build --no-out-link -E '<drv>'` prints the same path and
the same exit code as Nix, with no symlink on either side, and then
`nix-store --query --roots <path>` names a link under
`~/.cache/ietsp/roots` for ietsp's run and nothing for Nix's. stdout,
stderr and the exit code are unchanged — a root is a side effect on the
daemon — which is why `scripts/classic-e2e.nu` can compare both runs byte
for byte and check the root in the same case.

## Errors, exit codes and `--log-format internal-json`

`docs/reference/errors.md` has the codes and the record format. Where it differs from
Nix 2.34:

- **Build exit codes.** `ietsp build` exits 100, 101, 102 or 104 on a
  failed build (`failingExitStatus`) through a daemon, where Nix exits 1.
  Nix computes these codes only when it builds in its own store. The
  classic names exit 1, as Nix does as a daemon client.
- **Usage errors of native commands** exit 2. Nix exits 1; under a
  classic name ietsp does too.
- **`msg`** holds ietsp's plain text: no ANSI colours, no source
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
  - Ids are new ids of the ietsp process; Nix passes the daemon's ids on.
  - Nix sends `QueryMissing` before a build. So Nix has a `querying info
    about missing paths` activity and the substituter queries first,
    then `these N derivations will be built:`. ietsp does not send it:
    the queries come inside the build op, and no plan text is printed.
    `ietsp build --dry-run` prints the plan.
  - Nix writes the `.drv` files during evaluation, with no activity.
    ietsp uploads them before the build, under `copying N paths` and
    `copying path` activities, as `nix copy` does. Nix's copy activities
    also report the bytes of each path; ietsp's report the paths only.
  - Under `internal-json` the build renderer's lines (start, phase, log
    block, verdict, heartbeat) are not put out; the records carry them.
  - With a `grpc://` builder ietsp makes the records itself. There is
    one build activity per derivation from the start of its scheduling,
    and a derivation the cache already has also gets one. Its uploads
    have no copy activities.
  - The `--timings` and `IETSP_STATS` lines stay plain text.
- **`ietsp build --dry-run --json`** prints the plan (`willBuild`,
  `willSubstitute`, `unknown`, `downloadSize`, `narSize`). `nix build
  --dry-run --json` prints the installables' derived paths. The text on
  stderr is the same as Nix's.
- **`json-log-path`** applies to every command. Nix applies it to the
  `nix` command only: the classic names return before `applyJSONLogger`
  (`src/nix/main.cc`).
