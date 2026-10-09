# Getting started with iets

This tutorial takes about ten minutes. You will:

- build iets;
- evaluate an expression, then a file that reads nixpkgs;
- build a small derivation;
- watch the cache make a second run instant;
- see iets answer to a classic Nix command name.

You need a checkout of this repository, a working `nix` with flakes, and a
running `nix-daemon`.

Type each command exactly as written. Run them in order, in one terminal.

## 1. Build iets

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
iets version
```

You see a version and the commit it was built from. Your commit differs:

```
iets 0.1.0 (037e49d)
```

## 2. Evaluate an expression

```sh
iets eval --strict -E '1 + 1'
```

```
2
```

`-E` gives an expression on the command line. `--strict` evaluates the
whole value instead of only its top level.

## 3. Get the nixpkgs this repository pins

The next steps read nixpkgs. Ask iets for the exact tree this repository's
`flake.lock` names, so your results match the ones printed here:

```sh
nixpkgs=$(iets eval --raw --impure -E "(builtins.getFlake \"path:$PWD\").inputs.nixpkgs.outPath")
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
mkdir -p ~/iets-tour && cd ~/iets-tour
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
iets eval --strict --json -I nixpkgs=$nixpkgs hello.nix
```

```
{"drv":"/nix/store/2bkip97lh6y3dws9inyhjssyayh9vfhq-hello-2.12.3.drv","name":"hello-2.12.3"}
```

Nix 2.34.8 gives the same derivation path for the same tree. iets always
aims for that: same input, same `.drv`.

Select one attribute with `-A`:

```sh
iets eval --raw -I nixpkgs=$nixpkgs hello.nix -A name
```

```
hello-2.12.3
```

`-A` also works straight on the nixpkgs tree. You do not need to wrap it in
a file:

```sh
iets eval --raw $nixpkgs -A hello.name
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
  args = [ "-c" "echo hello from iets > $out" ];
}
EOF
```

Build it on your local daemon:

```sh
iets build --builder daemon greeting.nix
```

```
▶ building greeting (/home/you/iets-tour/greeting.nix:2:3)
✔  greeting (/home/you/iets-tour/greeting.nix:2:3) (0s)
/nix/store/vwp4kv8mzqx77xsf8x4dff531r5bcxkz-greeting
```

iets evaluated the derivation in memory, sent it to the daemon, and asked
the daemon to build it. iets itself wrote nothing to `/nix/store`. Run it
again and you see only the output path, because the daemon already has it.

Look at the two progress lines. They show the file and line that defined
the derivation, not only a store path. Now read the output:

```sh
cat /nix/store/vwp4kv8mzqx77xsf8x4dff531r5bcxkz-greeting
```

```
hello from iets
```

## 6. Watch the cache

`--timings` reports what each phase cost and whether the run came from the
cache. Use a cache directory of your own so this step starts cold:

```sh
iets eval --timings --cache-dir ~/iets-tour/cache \
  --strict --json -I nixpkgs=$nixpkgs hello.nix
```

```
cache: miss (no entry)
{"drv":"/nix/store/2bkip97lh6y3dws9inyhjssyayh9vfhq-hello-2.12.3.drv","name":"hello-2.12.3"}
eval: 0.499s
cache: wrote 1218 entries, 0.001s
```

The run took half a second. It recorded 1,218 things it looked at: every
file it read, every directory it listed, every path it checked.

Run exactly the same command again:

```sh
iets eval --timings --cache-dir ~/iets-tour/cache \
  --strict --json -I nixpkgs=$nixpkgs hello.nix
```

```
cache: hit, 1218 entries validated, 0 rehashed, load 0.000s, validate 0.005s
{"drv":"/nix/store/2bkip97lh6y3dws9inyhjssyayh9vfhq-hello-2.12.3.drv","name":"hello-2.12.3"}
```

There is no `eval:` line, because nothing was evaluated. iets checked the
1,218 entries in 5 milliseconds, saw that nothing had changed, and gave you
the saved answer.

Now change the file and run the same command a third time:

```sh
touch hello.nix
iets eval --timings --cache-dir ~/iets-tour/cache \
  --strict --json -I nixpkgs=$nixpkgs hello.nix
```

```
cache: hit, 1218 entries validated, 1 rehashed, load 0.002s, validate 0.012s
{"drv":"/nix/store/2bkip97lh6y3dws9inyhjssyayh9vfhq-hello-2.12.3.drv","name":"hello-2.12.3"}
```

`1 rehashed`: the modification time of `hello.nix` changed, so iets hashed
the file again, found the same contents, and still hit. Change what the
file says and the next run misses.

## 7. Use a classic Nix command name

The same binary answers to Nix's own command names. Ask it to instantiate
your derivation:

```sh
iets nix-instantiate greeting.nix
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

Same warning, same path, byte for byte. Under those names, iets handles
what it can and runs the real `nix` for the rest.

## What you have seen

You evaluated an expression and a file. You built a derivation through a
daemon. You saw a second run answered from the cache in milliseconds, and a
classic Nix name give Nix's own output.

Where to go next:

- [How to build on a remote builder](../how-to/build-remotely.md)
- [How to replace nix-diff, nvd, nurl and the rest](../how-to/replace-nix-tools.md)
- [How iets evaluates](../explanation/evaluation.md)
- [Every command and flag](../reference/commands.md)
