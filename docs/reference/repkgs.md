# The dev shell from repkgs

On x86_64-linux `devShells.default` is made of [repkgs](https://github.com/Mic92/repkgs)
packages, and nothing in it is nixpkgs'. Nothing iets ships comes from repkgs either:
`packages.default` and every check stay on nixpkgs. The shell is the one place where the change
is visible.

repkgs is a package set on stock Nix. One LLVM targets every platform, the build phases are
nushell modules, and a package's locked dependencies arrive through a dynamic derivation. iets
already evaluates the whole set with the drvPaths real nix gives, so the shell is also a standing
test of that.

## What the flake does

```nix
r = import "${repkgs}/nix/set.nix" { system = "x86_64-linux"; };
r.mkShell {
  name = "iets-shell";
  packages = with r.pkgs; [ rust hyperfine nix perf jq nushell direnv ];
}
```

- `r.mkShell` is repkgs' own shell builder, not nixpkgs' `mkShell`. It writes PATH,
  `PKG_CONFIG_PATH` and `CMAKE_PREFIX_PATH` from the package list and adds the set's cc wrapper,
  which is what `cargo build` links with. `nix develop` prepends PATH to the caller's, so the
  shell adds tools rather than replacing the terminal's.
- `nix/set.nix` rather than the flake's own outputs: `mkShell` lives there, and so does the set's
  own `nix`. `repkgs.packages.<system>.nix` is the one built from nixpkgs, which exists to
  bootstrap a daemon that can build the set at all.
- `r.pkgs.rust` is cargo, rustc, clippy and rustfmt in one output.

aarch64-linux keeps the nixpkgs shell: repkgs uploads its seed nar for x86_64-linux only.

## What repkgs needs from the daemon

```nix
nix.settings = {
  experimental-features = [ "nix-command" "ca-derivations" "dynamic-derivations" "recursive-nix" ];
  system-features = [ "builder-rpc-v0" "big-parallel" "kvm" "nixos-test" "benchmark" ];
};
nix.package = inputs.repkgs.packages.${pkgs.system}.nix;
```

`builder-rpc-v0` is the part to check first. The dependency fetchers run under it, so without it
every cargo, go or npm package of the set fails with *missing system features* before anything
compiles. The feature comes from [NixOS/nix#16459](https://github.com/NixOS/nix/pull/16459),
which stock nix does not have; `nix-build nix/nix` in repkgs builds the nix that does. A client
cannot supply the feature: the daemon's own `system-features` decides.

The first entry into the shell is a bootstrap from the seed: llvm twice, then rust from source.
Budget hours on a quiet machine, not minutes.

## What was added to repkgs for this

Four tools and the shell builder itself did not exist in the set. All of it is on the
`iets-packages` branch of `github.com/zimbatm/repkgs`:

| what | why |
|---|---|
| `nix/mk-shell.nix` | `mkShell` and `mkShellNoCC`, so a shell needs no nixpkgs |
| hyperfine (cargo) | the benchmark driver `scripts/` and `bench/` use |
| direnv (go) | `.envrc` and `nix/direnvrc` |
| elfutils (autotools) | libelf and libdw, which perf needs for symbols and DWARF |
| perf (make) | `perf stat -e instructions:u`, the measurement iets trusts |

`mkShell` is shaped by one constraint: `nix develop` never builds the shell. It swaps the
builder's arguments for its own `get-env.sh`, builds that, and reads the environment back. So
the builder must be called `bash` — `develop.cc` refuses any other name, and iets mirrors it in
`src/devenv/mod.rs` — which means the nu builder every repkgs package uses cannot be entered.
There is also no setup script and no stdenv, so the shell writes its variables out rather than
deriving them from `buildInputs`.

perf needed two things. `Makefile.config` rebuilds `CC` as `$(CLANG) $(CLANG_FLAGS)
-fintegrated-as` the moment it finds that CC is clang, and `$(CLANG)` defaults to a bare `clang`
on PATH, which carries none of the toolchain wrapper's flags — not even the sysroot, so nothing
finds `stdio.h`. And perf compiles its exec path in, so it installs through `DESTDIR` with
`prefix=/`: a repkgs output may not name its own install prefix.

## What is missing

- **perf's external helpers.** `stat`, `record`, `report` and every other builtin subcommand
  work. The scripts under `libexec/perf-core` (`perf archive`, `perf iostat`) need
  `PERF_EXEC_PATH` pointing at them, because subcmd resolves a relative exec path against the
  compiled-in prefix and has no runtime-prefix mechanism. `perf trace` and the tracepoint
  events need libtraceevent, which the set does not package.
- **aarch64-linux**, as above.
