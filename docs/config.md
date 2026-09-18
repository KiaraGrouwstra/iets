# nix.conf

`src/config.rs` reads Nix's settings files. It copies Nix 2.34.8:
`src/libstore/globals.cc:loadConfFile` for discovery,
`src/libutil/configuration.cc:parseConfigFiles` for the line syntax.

## Files and order

`Config::load(overrides)` applies these in order. Later wins.

1. `$NIX_CONF_DIR/nix.conf`, default `/etc/nix/nix.conf`.
2. The user files, lowest precedence first. `loadConfFile` walks the list
   backwards (`globals.cc:143`), so the **first** entry of the list below has
   the highest precedence:
   - `$NIX_USER_CONF_FILES`, colon-separated, replaces the whole list; else
   - `$NIX_CONFIG_HOME/nix.conf` or `$XDG_CONFIG_HOME/nix/nix.conf`
     (default `~/.config/nix/nix.conf`), then
   - `<dir>/nix/nix.conf` for each `$XDG_CONFIG_DIRS` entry
     (default `/etc/xdg`).
3. `$NIX_CONFIG`, treated as one more file's text.
4. The `--option name value` pairs from the command line.

A file that is missing or unreadable is skipped, not an error.

## Line syntax

- `#` starts a comment; the rest of the line is dropped.
- `name = value`. The value is the rest of the line, with runs of whitespace
  collapsed to one space.
- `include path` and `!include path`, relative to the including file. A missing
  `include` is an error (`file '...' included from '...' not found`); a missing
  `!include` is silent.
- `extra-<name> = value` appends to a list setting. For a setting that is not a
  list, the pair is kept under its literal name, as Nix keeps it in
  `unknownSettings`.
- An unknown setting is kept and readable with `Config::str`. Nix warns about
  it; this reader is silent.

## Values

Raw strings, with typed accessors: `str`, `int`, `bool` (`true`/`yes`/`1` and
`false`/`no`/`0`), `list` (whitespace-separated) and `paths` (the `builders`
form: `;`-separated, `@file` reads that machines file).

Typed getters carry Nix's defaults: `store` (`auto`), `system` (the build
target), `max_jobs` (`auto` = CPU count), `cores` (`0` = all), `keep_going`,
`keep_failed`, `substituters`, `trusted_public_keys`, `builders`,
`experimental_features`, `nix_path` and `sandbox`.

`$NIX_PATH` replaces the `nix-path` setting, as `eval-gc.cc:129` does.

## What is honoured today

`worker::Options` carries `max-jobs`, `cores`, `keep-going`, `keep-failed` and
`substitute` into the daemon's `SetOptions`
(`remote-store.cc:setOptions`). Build `Options` with
`Options::try_from(&config)`.

Library call sites still pass `Options::default()`, which sends the values this
client has always sent. The CLI does not build a `Config` yet: that wiring
lands with classic mode.

## Parsed but not used yet

`store`, `system`, `substituters`, `trusted-public-keys`, `builders`,
`experimental-features`, `nix-path`, `sandbox` and every unknown setting are
read and exposed, but nothing in ietsp consults them yet.
