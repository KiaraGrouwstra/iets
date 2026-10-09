# Builtins

iets has every builtin that Nix 2.34.8 has. This page tells you how well
each one works, and where it differs from Nix.

Each row has one of three statuses:

- `works`: you get what Nix gives you, apart from what the notes say.
- `partial`: it works for the inputs in the notes, and fails for the rest.
- `fails`: it always stops with an error.

A few rules hold for every builtin:

- **No import from derivation.** If a path is the output of a derivation,
  reading it fails, because evaluation never builds anything. You can still
  `import` a `.drv` file.
- **Classic mode falls back.** When iets does not support a call, a classic
  name such as `nix-instantiate` or `nix` runs the real Nix for you
  (`docs/reference/classic.md`).
- **Pure evaluation reads your project.** A pure run of a native command can
  read files below the project root. Nix does not allow that
  (`docs/reference/divergences.md`).

When a note mentions a difference, `docs/reference/divergences.md` has the
details. To add a builtin, see `docs/how-to/add-a-builtin.md`.

## Status

| name | status | notes |
|---|---|---|
| `abort` | works | |
| `add` | works | |
| `addDrvOutputDependencies` | works | |
| `addErrorContext` | works | |
| `all` | works | |
| `any` | works | |
| `appendContext` | works | |
| `attrNames` | works | |
| `attrValues` | works | |
| `baseNameOf` | works | |
| `bitAnd` | works | |
| `bitOr` | works | |
| `bitXor` | works | |
| `break` | works | Forces its argument. Nix returns the argument unforced. |
| `builtins` | works | |
| `catAttrs` | works | |
| `ceil` | works | |
| `compareVersions` | works | |
| `concatLists` | works | |
| `concatMap` | works | |
| `concatStringsSep` | works | |
| `convertHash` | works | |
| `currentSystem` | works | Not there in pure evaluation, as in Nix. |
| `currentTime` | works | Not there in pure evaluation, as in Nix. |
| `deepSeq` | works | |
| `derivation` | works | See `derivationStrict`. |
| `derivationStrict` | works | An `env` name, `builder` and `system` must be UTF-8. Nix accepts any bytes. |
| `dirOf` | works | |
| `div` | works | A `let` binding of `__div` does not change `/`. In Nix it does. |
| `elem` | works | |
| `elemAt` | works | |
| `false` | works | |
| `fetchClosure` | fails | Under a classic name, there only with the `fetch-closure` feature, as in Nix. |
| `fetchGit` | fails | |
| `fetchMercurial` | fails | |
| `fetchTarball` | partial | Needs a `sha256`. |
| `fetchTree` | partial | Type `path` works for an absolute path. Types `github`, `gitlab`, `sourcehut`, `git`, `tarball` and `hg` need a `narHash`. Other types fail. Under a classic name, there only with the `fetch-tree` or `flakes` feature, as in Nix. |
| `fetchurl` | fails | |
| `filter` | works | |
| `filterSource` | works | |
| `findFile` | works | |
| `flakeRefToString` | works | Accepts some references that Nix refuses. A `git` input that is not a URL fails. |
| `floor` | works | |
| `foldl'` | works | |
| `fromJSON` | works | Error text differs. Refuses nesting deeper than 100,000 levels. |
| `fromTOML` | works | TOML 1.0, as in Nix. Datetimes need the `parse-toml-timestamps` feature. Error text differs. |
| `functionArgs` | works | |
| `genericClosure` | works | When two keys cannot be compared, the error can name another pair. |
| `genList` | works | |
| `getAttr` | works | |
| `getContext` | works | Fails on the output of a dynamic derivation. Nix builds the producer first. |
| `getEnv` | works | |
| `getFlake` | partial | Impure evaluation only. Needs a local flake with a complete `flake.lock`. |
| `groupBy` | works | |
| `hasAttr` | works | |
| `hasContext` | works | |
| `hashFile` | works | |
| `hashString` | works | |
| `head` | works | |
| `import` | works | Also imports a `.drv` file. |
| `intersectAttrs` | works | |
| `isAttrs` | works | |
| `isBool` | works | |
| `isFloat` | works | |
| `isFunction` | works | |
| `isInt` | works | |
| `isList` | works | |
| `isNull` | works | |
| `isPath` | works | |
| `isString` | works | |
| `langVersion` | works | `6`. |
| `length` | works | |
| `lessThan` | works | A `let` binding of `__lessThan` does not change `<`. In Nix it does. |
| `listToAttrs` | works | |
| `map` | works | |
| `mapAttrs` | works | |
| `match` | works | |
| `mul` | works | A `let` binding of `__mul` does not change `*`. In Nix it does. |
| `nixPath` | works | |
| `nixVersion` | works | `"2.34.8"`. |
| `null` | works | |
| `outputOf` | works | A producer that this run did not make must be in `/nix/store`. Under a classic name, there only with the `dynamic-derivations` feature, as in Nix. |
| `parseDrvName` | works | |
| `parseFlakeRef` | works | Accepts some references that Nix refuses. `sourcehut:`, `hg+...` and other schemes fail. |
| `partition` | works | |
| `path` | works | |
| `pathExists` | works | |
| `placeholder` | works | |
| `readDir` | works | |
| `readFile` | works | |
| `readFileType` | works | |
| `removeAttrs` | works | |
| `replaceStrings` | works | |
| `scopedImport` | works | |
| `seq` | works | |
| `sort` | works | |
| `split` | works | |
| `splitVersion` | works | |
| `storeDir` | works | |
| `storePath` | works | |
| `stringLength` | works | |
| `sub` | works | A `let` binding of `__sub` does not change `-`. In Nix it does. |
| `substring` | works | |
| `tail` | works | |
| `throw` | works | |
| `toFile` | works | |
| `toJSON` | works | Writes a float with the fewest digits. Nix sometimes writes more. |
| `toPath` | works | |
| `toString` | works | |
| `toXML` | works | |
| `trace` | works | |
| `traceVerbose` | works | |
| `true` | works | |
| `tryEval` | works | |
| `typeOf` | works | |
| `unsafeDiscardOutputDependency` | works | |
| `unsafeDiscardStringContext` | works | |
| `unsafeGetAttrPos` | works | |
| `warn` | works | |
| `zipAttrsWith` | works | |
