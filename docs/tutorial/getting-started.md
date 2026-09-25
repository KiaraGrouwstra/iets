# Getting started with ietsp

This tutorial takes about ten minutes. You will build ietsp, evaluate an
expression, evaluate a file that reads nixpkgs, build a small derivation,
watch the cache make a second run instant, and see ietsp answer to a
classic Nix command name.

You need a checkout of this repository, a working `nix` with flakes, and a
running `nix-daemon`.

Every command below is meant to be typed exactly as it is written. Run them
in order, in one terminal.

## 1. Build ietsp

From the repository root:

```sh
nix develop -c cargo build --release
```

The first build takes a few minutes. When it finishes, put the binary on
your path for this shell:

```sh
export PATH="$PWD/target/release:$PATH"
```

Check it:

```sh
ietsp version
```

You see a version and the commit it was built from. Your commit differs:

```
ietsp 0.1.0 (037e49d)
```

## 2. Evaluate an expression

```sh
ietsp eval --strict -E '1 + 1'
```

```
2
```

`-E` gives an expression on the command line. `--strict` evaluates the
whole value instead of only its top level.

## 3. Get the nixpkgs this repository pins

The next steps read nixpkgs. Ask ietsp for the exact tree this repository's
`flake.lock` names, so your results match the ones printed here:

```sh
nixpkgs=$(ietsp eval --raw --impure -E "(builtins.getFlake \"path:$PWD\").inputs.nixpkgs.outPath")
echo "$nixpkgs"
```

```
/nix/store/hhfnpma32czw4h3bqpag8dciax0bcmar-source
```

Your hash differs if the lock file has moved on. That is fine; the rest of
the tutorial uses the variable.

## 4. Evaluate a file

Make a working directory and write a file in it:

```sh
mkdir -p ~/ietsp-tour && cd ~/ietsp-tour
```

```sh
cat > hello.nix <<'EOF'
let
  pkgs = import <nixpkgs> { };
in
{
  inherit (pkgs.hello) name;
  drv = pkgs.hello.drvPath;
}
EOF
```

Evaluate it. `-I` says where `<nixpkgs>` is:

```sh
ietsp eval --strict --json -I nixpkgs=$nixpkgs hello.nix
```

```
{"drv":"/nix/store/2bkip97lh6y3dws9inyhjssyayh9vfhq-hello-2.12.3.drv","name":"hello-2.12.3"}
```

That derivation path is the same one Nix 2.34.8 computes for the same tree.
Parity on derivation paths is the invariant ietsp is built around.

Select one attribute with `-A`:

```sh
ietsp eval --raw -I nixpkgs=$nixpkgs hello.nix -A name
```

```
hello-2.12.3
```

## 5. Build a derivation

Write a derivation that makes one file:

```sh
cat > greeting.nix <<'EOF'
derivation {
  name = "greeting";
  system = builtins.currentSystem;
  builder = "/bin/sh";
  args = [ "-c" "echo hello from ietsp > $out" ];
}
EOF
```

Build it on your local daemon:

```sh
ietsp build --builder daemon greeting.nix
```

```
▶ building greeting (/home/you/ietsp-tour/greeting.nix:2:3)
✔  greeting (/home/you/ietsp-tour/greeting.nix:2:3) (0s)
/nix/store/vwp4kv8mzqx77xsf8x4dff531r5bcxkz-greeting
```

ietsp evaluated the derivation in memory, uploaded it to the daemon, and
asked the daemon to build it. It wrote nothing to `/nix/store` itself. Run
it again and only the output path is printed: the daemon already has it.

Note the two progress lines. They name the attribute and the file and line
that defined the derivation, not only a store path. Read the output:

```sh
cat /nix/store/vwp4kv8mzqx77xsf8x4dff531r5bcxkz-greeting
```

```
hello from ietsp
```

## 6. Watch the cache

`--timings` reports what each phase cost and whether the run came from the
cache. Use a cache directory of your own so this step starts cold:

```sh
ietsp eval --timings --cache-dir ~/ietsp-tour/cache \
  --strict --json -I nixpkgs=$nixpkgs hello.nix
```

```
cache: miss (no entry)
{"drv":"/nix/store/2bkip97lh6y3dws9inyhjssyayh9vfhq-hello-2.12.3.drv","name":"hello-2.12.3"}
eval: 0.499s
cache: wrote 1218 entries, 0.001s
```

The run took half a second and recorded 1,218 observations of the outside
world — every file it read, every directory it listed, every path it
checked.

Run exactly the same command again:

```sh
ietsp eval --timings --cache-dir ~/ietsp-tour/cache \
  --strict --json -I nixpkgs=$nixpkgs hello.nix
```

```
cache: hit, 1218 entries validated, 0 rehashed, load 0.000s, validate 0.005s
{"drv":"/nix/store/2bkip97lh6y3dws9inyhjssyayh9vfhq-hello-2.12.3.drv","name":"hello-2.12.3"}
```

There is no `eval:` line, because nothing was evaluated. ietsp checked the
1,218 observations in 5 milliseconds, found that nothing had changed, and
replayed the answer.

Now change the file and run the same command a third time:

```sh
touch hello.nix
ietsp eval --timings --cache-dir ~/ietsp-tour/cache \
  --strict --json -I nixpkgs=$nixpkgs hello.nix
```

```
cache: hit, 1218 entries validated, 1 rehashed, load 0.002s, validate 0.012s
{"drv":"/nix/store/2bkip97lh6y3dws9inyhjssyayh9vfhq-hello-2.12.3.drv","name":"hello-2.12.3"}
```

`1 rehashed`: the modification time of `hello.nix` changed, so ietsp hashed
the file again, found the same contents, and still hit. Change what the
file says and the next run misses.

## 7. Use a classic Nix command name

The same binary answers to Nix's own command names. Ask it to instantiate
your derivation:

```sh
ietsp nix-instantiate greeting.nix
```

```
warning: you did not specify '--add-root'; the result might be removed by the garbage collector
/nix/store/zr5qd58hj1cxwncj6z0ra1qlp09g8h07-greeting.drv
```

Now ask the real Nix the same thing:

```sh
nix-instantiate greeting.nix
```

```
warning: you did not specify '--add-root'; the result might be removed by the garbage collector
/nix/store/zr5qd58hj1cxwncj6z0ra1qlp09g8h07-greeting.drv
```

Same warning, same path, byte for byte. That is what "drop-in" means here.
Under those names ietsp handles what it implements and runs the real `nix`
for the rest.

## What you have seen

You evaluated an expression and a file, built a derivation through a
daemon, saw a second identical run answered from cache in milliseconds, and
saw a classic Nix name give Nix's own output.

Where to go next:

- [How to build on a remote builder](../how-to/build-remotely.md)
- [How to replace nix-diff, nvd, nurl and the rest](../how-to/replace-nix-tools.md)
- [How ietsp evaluates](../explanation/evaluation.md)
- [Every command and flag](../reference/commands.md)
