# Settings files

ietsp reads two settings files. `nix.conf` is Nix's, shared with the real nix
behind the classic fallback; `ietsp.conf` is ietsp's own, and no nix ever sees
it. Both use the same line syntax.

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
`false`/`no`/`0`) and `list` (whitespace-separated). Typed getters carry Nix's
defaults: `store` (`auto`), `system` (the build target), `max_jobs` (`auto` =
CPU count), `cores` (`0` = all), `keep_going` and `keep_failed`.

`$NIX_PATH` replaces the `nix-path` setting, as `eval-gc.cc:129` does.

## What is honoured

`worker::Options` carries `max-jobs`, `cores`, `keep-going`, `keep-failed` and
`substitute` into the daemon's `SetOptions` (`remote-store.cc:setOptions`),
built with `Options::try_from(&config)`. `crate::cli::classic` uses it, and
takes `store` as the last fallback for `builder` below and `system` for
`builtins.currentSystem` (`docs/classic.md`). The other subcommands read
`system` and `store` only and send `Options::default()`. The flake input
fetcher reads `access-tokens`, `connect-timeout` (default 15 s) and
`stalled-download-timeout` (default 300 s); 0 is no limit, as in
`filetransfer.hh`. Every other setting is read and kept, and nothing in ietsp
consults it.

# ietsp.conf

`src/settings.rs` reads ietsp's own settings. The line syntax is nix.conf's,
parsed by the same code: `name = value`, `include`/`!include`, `#` comments and
`extra-<name>` appending to a list setting.

Nothing but ietsp reads this file, so a host can send ietsp's builds somewhere
without touching `nix.conf`, which the real nix behind the classic fallback
also reads.

## Files and order

Later wins.

1. `$IETSP_CONF_DIR/ietsp.conf`, default `/etc/ietsp/ietsp.conf`.
2. `$XDG_CONFIG_HOME/ietsp/ietsp.conf`, default `~/.config/ietsp/ietsp.conf`.
3. `$IETSP_CONFIG`, treated as one more file's text.
4. The `--ietsp-option name value` pairs from the command line.

A file that is missing or unreadable is skipped. An **unknown key is an
error** naming the file and the line: this file is ietsp's, so a name it does
not know is a mistake, not a setting for somebody else.

## Keys

Every key in the **File** column of `docs/settings.md`, with the type and
default given there, plus three keys that have no environment variable:

| Key | Type | Default |
|:---|:---|:---|
| `builder` | URL: `daemon`, `unix://PATH`, `ssh-ng://[user@]host`, `grpc://host:port` | the nix.conf `store` setting (itself `auto`) |
| `source-map` | list of `STOREPATH=LOCALDIR` | empty |
| `fetch-max-size` | bytes | 17179869184 (16 GiB) |

`extra-source-map` appends; no other key is a list. `fetch-max-size` caps the
decompressed bytes of one flake input download; a stream that decompresses
over 1000 times its size past its first 16 MiB is refused too.

ietsp has no local store of its own, so `auto` and an empty value both mean the
local daemon.

`builder` is where derivations are realised. Every command takes it:
`ietsp build --builder`, `ietsp print-dev-env --builder` and
`ietsp fast-build --store` win over it, and so does a classic `--store` or
`--option store`. `nix-build`, `nix build`, `nix-shell` and `nix develop` have
no flag of their own and use it as it stands. Registration is not affected:
drvs and sources for `nix-instantiate --add-root`, its GC roots and the
`nix-build`/`nix build` out-links always go to the local daemon, and a remote
`builder`'s outputs are copied back first (`docs/classic.md`).

## The NixOS module

`ietsp.settings` is an attribute set of key to string, bool, int or list of
strings, rendered to `/etc/ietsp/ietsp.conf`:

```nix
{
  imports = [ ietsp.nixosModules.default ];
  ietsp.settings.builder = "ssh-ng://bld1";
}
```
