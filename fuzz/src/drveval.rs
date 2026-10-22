//! The drv-eval target. The input chooses up to three `derivation` calls,
//! and a later one can name an earlier one in its attributes. The
//! expression is the list of each derivation's drvPath and outPaths. iets
//! and Nix 2.34.8 (`eval-oracle`, with a store in read-only mode that
//! nothing is written to) must give the same paths, or both must fail.
//! The comparison is the eval target's (`eval.rs`); only the expressions
//! differ.
//!
//! The shapes follow real derivations: those of nix-conformance `drv/`,
//! which hold the nix-gc-broken seeds, and those of nixpkgs: fixed outputs
//! in each hash format, several outputs, structured attributes, and
//! references to other derivations in strings.

use std::collections::BTreeSet;

use libfuzzer_sys::arbitrary::Unstructured;

const SHOW: &str =
    "show = d: [ d.drvPath ] ++ map (o: d.${o}.outPath) (d.outputs or [ \"out\" ]);\n";

/// The hashes of the empty string: (algorithm, base16, nix32, base64).
const HASHES: &[(&str, &str, &str, &str)] = &[
    (
        "md5",
        "d41d8cd98f00b204e9800998ecf8427e",
        "3y8bwfr609h3lh9ch0izcqq7fl",
        "1B2M2Y8AsgTpgAmY7PhCfg==",
    ),
    (
        "sha1",
        "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        "143xibwh31h9bvxzalr0sjvbbvpa6ffs",
        "2jmj7l5rSw0yVb/vlWAYkK/YBwk=",
    ),
    (
        "sha256",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73",
        "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
    ),
    (
        "sha512",
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
        "0zdl9zrg8r3i9c1g90lgg9ip5ijzv3yhz91i0zzn3r8ap9ws784gkp9dk9j3aglhgf1amqb0pj21mh7h1nxcl18akqvvf7ggqsy30yg",
        "z4PhNX7vuL3xVChQ1m2AB9Yg5AULVxXcg/SpIdNs6c5H0NE8XYXysP+DGNKHfuwvY7kxvUdBeoGlODJ6+SfaPg==",
    ),
];

const NAMES: &[&str] = &[
    "hello-2.12.3",
    "a",
    "glibc-2.42",
    "source",
    "python3.13-requests-2.32.3",
    "foo+bar_baz?=1",
];

const OUTPUTS: &[&str] = &["out", "dev", "bin", "lib", "doc", "man", "debug"];

const ENV_NAMES: &[&str] = &[
    "a",
    "b",
    "src",
    "buildInputs",
    "nativeBuildInputs",
    "FOO_BAR",
    "version",
    "pname",
    "meta",
    "a b",
    "",
    "out",
    "dev",
    "outPath",
    "type",
    "allowedReferences",
    "allowedRequisites",
    "disallowedReferences",
    "preferLocalBuild",
    "allowSubstitutes",
    "requiredSystemFeatures",
    "impureEnvVars",
    "exportReferencesGraph",
];

/// The expression `input` chooses; see the module comment.
pub fn generate(u: &mut Unstructured) -> String {
    let mut g = Gen {
        u,
        outputs: Vec::new(),
        bad: false,
    };
    // an error anywhere fails the whole list: one expression in four only
    // may hold one on purpose, so most compare paths, not errors
    g.bad = g.odds(4);
    let n = 1 + g.small(2) as usize;
    let mut binds = String::from(SHOW);
    for i in 0..n {
        let d = g.drv(i);
        binds.push_str(&format!("d{i} = {d};\n"));
    }
    let shown: Vec<String> = (0..n).map(|i| format!("(show d{i})")).collect();
    format!("let\n{binds}in [ {} ]", shown.join(" "))
}

struct Gen<'a, 'b> {
    u: &'a mut Unstructured<'b>,
    /// The outputs of each derivation made so far.
    outputs: Vec<Vec<&'static str>>,
    /// Whether this expression may hold a wrong value on purpose.
    bad: bool,
}

impl Gen<'_, '_> {
    fn pick(&mut self, n: usize) -> usize {
        if n <= 1 {
            return 0;
        }
        self.u.choose_index(n).unwrap_or(0)
    }

    fn one<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.pick(xs.len())]
    }

    fn odds(&mut self, n: u8) -> bool {
        // with no bytes left, `ratio` says yes to every rare choice
        !self.u.is_empty() && self.u.ratio(1u8, n).unwrap_or(false)
    }

    /// A wrong value, one time in `n`, in an expression that may hold one.
    fn wrong(&mut self, n: u8) -> bool {
        self.bad && self.odds(n)
    }

    fn small(&mut self, max: u8) -> u8 {
        self.u.int_in_range(0..=max).unwrap_or(0)
    }

    fn drv(&mut self, i: usize) -> String {
        let mut attrs: Vec<(String, String)> = Vec::new();
        let structured = self.odds(5);
        if !self.wrong(16) {
            let name = self.name();
            attrs.push((quote("name"), name));
        }
        if !self.wrong(16) {
            let s = self.system();
            attrs.push((quote("system"), s));
        }
        if !self.wrong(16) {
            let b = self.builder(i);
            attrs.push((quote("builder"), b));
        }
        if self.odds(2) {
            let n = self.small(3);
            let args: Vec<String> = (0..n).map(|_| self.value(i, 1, false)).collect();
            attrs.push((quote("args"), format!("[ {} ]", args.join(" "))));
        }
        let fixed = self.odds(5);
        // several outputs with a fixed output is an error
        let outputs = if self.odds(3) && (!fixed || self.wrong(4)) {
            let outs = self.outputs_list();
            attrs.push((quote("outputs"), outs.0));
            outs.1
        } else {
            vec!["out"]
        };
        if fixed {
            self.fixed(&mut attrs);
        }
        if structured {
            attrs.push((quote("__structuredAttrs"), "true".into()));
        }
        if self.odds(8) {
            attrs.push((quote("__ignoreNulls"), "true".into()));
        }
        if self.odds(10) {
            let f = format!("[ \"{}\" ]", self.one(&["a", "b", "src", "nope"]));
            attrs.push((quote("passAsFile"), f));
        }
        if self.odds(24) {
            // ca-derivations is on in eval-oracle; impure-derivations and
            // git-hashing are not, and iets has neither (both refuse)
            let k = self.one(&["__contentAddressed", "__impure", "__noChroot"]);
            if k != "__impure" || self.bad {
                attrs.push((quote(k), "true".into()));
            }
        }
        for _ in 0..self.small(5) {
            // a name that is not UTF-8: Nix takes it, iets refuses it
            // (docs/reference/divergences.md; nix-gc-broken's u4)
            let k = if self.odds(255) {
                "${builtins.substring 0 1 \"é\"}".to_string()
            } else {
                quote(self.one(ENV_NAMES))
            };
            let v = self.value(i, 2, structured);
            attrs.push((k, v));
        }
        let mut seen = BTreeSet::new();
        attrs.retain(|(k, _)| seen.insert(k.clone()));
        let n = attrs.len();
        for k in (1..n).rev() {
            let j = self.pick(k + 1);
            attrs.swap(k, j);
        }
        self.outputs.push(outputs);
        let body: Vec<String> = attrs.iter().map(|(k, v)| format!("{k} = {v};")).collect();
        format!("derivation {{ {} }}", body.join(" "))
    }

    fn name(&mut self) -> String {
        if self.wrong(3) {
            return match self.small(7) {
                0 => quote(""),
                1 => quote(&"a".repeat(212)),
                2 => quote(".hidden"),
                3 => quote(self.one(&["a b", "a/b", "é", "a!"])),
                4 => "(builtins.substring 0 1 \"é\")".into(),
                5 => quote("x.drv"),
                // the longest name, too long with an output's suffix
                6 => quote(&"a".repeat(211)),
                _ => "1".into(),
            };
        }
        quote(self.one(NAMES))
    }

    fn system(&mut self) -> String {
        if self.wrong(3) {
            return self.one(&["builtins.currentSystem", "1", "\"\""]).into();
        }
        match self.small(127) {
            // Nix takes it, iets refuses it (docs/reference/divergences.md)
            127 => "(builtins.substring 0 1 \"é\")".into(),
            _ => quote(self.one(&["x86_64-linux", "aarch64-darwin", "x", "builtin"])),
        }
    }

    fn builder(&mut self, i: usize) -> String {
        if self.wrong(4) {
            return quote("");
        }
        match self.small(127) {
            // as for `system`
            127 => "(builtins.substring 0 1 \"é\")".into(),
            123..=126 => quote("builtin:fetchurl"),
            115..=122 if i > 0 => format!("\"${{d{}}}/bin/sh\"", self.pick(i)),
            _ => quote("/bin/sh"),
        }
    }

    /// The `outputs` attribute, and the names in it.
    fn outputs_list(&mut self) -> (String, Vec<&'static str>) {
        if self.wrong(4) {
            let bad = self.one(&[
                "[ ]",
                "[ \"\" ]",
                "[ \"a b\" ]",
                "[ \"drv\" ]",
                "\"out\"",
                "[ 1 ]",
                "[ \"out\" \"out\" ]",
            ]);
            return (bad.into(), vec!["out"]);
        }
        let mut outs: Vec<&'static str> = Vec::new();
        for _ in 0..1 + self.small(3) {
            let o = self.one(OUTPUTS);
            if !outs.contains(&o) {
                outs.push(o);
            }
        }
        let list: Vec<String> = outs.iter().map(|o| quote(o)).collect();
        (format!("[ {} ]", list.join(" ")), outs)
    }

    /// `outputHash` and, now and then, `outputHashAlgo` and `outputHashMode`.
    fn fixed(&mut self, attrs: &mut Vec<(String, String)>) {
        let (algo, b16, n32, b64) = self.one(HASHES);
        // base16, nix32 and base64 name no algorithm: outputHashAlgo must
        let bare = self.odds(3);
        let hash = if self.wrong(3) {
            match self.small(2) {
                0 => b16[..b16.len() - 1].to_string(),
                1 => String::new(),
                _ => format!("{algo}-{}", &b64[1..]),
            }
        } else if bare {
            self.one(&[b16, n32, b64]).to_string()
        } else {
            match self.small(2) {
                0 => format!("{algo}:{b16}"),
                1 => format!("{algo}:{n32}"),
                _ => format!("{algo}-{b64}"),
            }
        };
        attrs.push((quote("outputHash"), quote(&hash)));
        if bare || self.odds(2) {
            let a = if self.wrong(3) {
                self.one(&["", "blake3", "sha384", "SHA256"])
            } else if !bare && self.odds(4) {
                // another algorithm than the hash's: Nix takes the hash's
                self.one(HASHES).0
            } else {
                algo
            };
            attrs.push((quote("outputHashAlgo"), quote(a)));
        }
        if self.odds(2) {
            let m = if self.wrong(3) {
                self.one(&["git", "bogus", "text"])
            } else {
                self.one(&["flat", "recursive", "nar"])
            };
            attrs.push((quote("outputHashMode"), quote(m)));
        }
    }

    /// A value of an attribute or an argument. Without structured
    /// attributes it is coerced to a string as `derivationStrict` does
    /// (ints, floats, bools, null and lists too); with them, it becomes
    /// JSON.
    fn value(&mut self, i: usize, depth: u8, structured: bool) -> String {
        match self.small(15) {
            0 => self
                .one(&[
                    "0",
                    "42",
                    "(-1)",
                    "9223372036854775807",
                    "(-9223372036854775807 - 1)",
                ])
                .into(),
            1 => self
                .one(&["1.5", "0.1", "3.0", "1.0e21", "(-0.0)", "123456.789"])
                .into(),
            2 => self.one(&["true", "false"]).into(),
            3 => "null".into(),
            4 if depth > 0 => {
                let n = self.small(3);
                let xs: Vec<String> = (0..n)
                    .map(|_| self.value(i, depth - 1, structured))
                    .collect();
                format!("[ {} ]", xs.join(" "))
            }
            5 if depth > 0 && structured => {
                let n = self.small(2);
                let xs: Vec<String> = (0..n)
                    .map(|k| {
                        let v = self.value(i, depth - 1, true);
                        format!("{} = {v};", ["a", "b"][k as usize])
                    })
                    .collect();
                format!("{{ {} }}", xs.join(" "))
            }
            6 if self.bad || structured => "{ }".into(),
            6 => self
                .one(&["{ outPath = \"/x\"; }", "{ __toString = self: \"t\"; }"])
                .into(),
            7 => "(builtins.toFile \"f\" \"contents\")".into(),
            8..=10 if i > 0 => self.reference(i),
            // Nix takes it in a value, iets too; with structured
            // attributes, JSON refuses it on both sides
            11 => "(builtins.substring 0 1 \"é\")".into(),
            12 => quote(self.one(&["a\nb\tc", "\"quoted\" \\ $x ${", "é ü 日本", "${x}"])),
            _ => quote(self.one(&[
                "",
                "a",
                "hello",
                "-O2 -g",
                "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-x",
            ])),
        }
    }

    /// A string or value that names derivation `d<k>`, `k < i`, with its
    /// context.
    fn reference(&mut self, i: usize) -> String {
        let k = self.pick(i);
        let outs = self.outputs[k].clone();
        let o = self.one(&outs);
        match self.small(8) {
            0 => format!("\"${{d{k}}}\""),
            1 => format!("\"${{d{k}.{o}}}/lib\""),
            2 => format!("d{k}.drvPath"),
            3 => format!("(builtins.unsafeDiscardStringContext \"${{d{k}}}\")"),
            4 => format!("(builtins.unsafeDiscardOutputDependency d{k}.drvPath)"),
            5 => format!("[ d{k} \"a\" ]"),
            6 => format!("(builtins.toString d{k}.{o})"),
            _ => format!("d{k}"),
        }
    }
}

/// A Nix string literal of `s`.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
