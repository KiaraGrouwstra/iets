//! Tree-walking evaluator over the resolved IR.

pub mod coerce;
pub mod error;

#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::collections::HashMap;

pub use error::{Error, ErrorKind, EvalResult, Pos};

use crate::compile::ir::{AttrElemIr, AttrsIr, BindIr, BindScope, Ir, IrRef, LambdaIr, ParamIr};
use crate::compile::{Compiler, Globals};
use crate::runtime::*;
use crate::runtime::arena::{STAT_NAMES, Stat};
use crate::syntax::ast::FileId;
use crate::syntax::symbol::{FxHashMap, Interner, Sym};

pub const MAX_CALL_DEPTH: u32 = 10000;

/// Virtual path prefix for the embedded corepkgs (`<nix/fetchurl.nix>`).
pub struct CorePkgs;
impl CorePkgs {
    pub const PREFIX: &[u8] = b"/__corepkgs__/";
}

pub struct FileInfo {
    pub path: Vec<u8>,
    pub src: &'static [u8],
    pub base: u32,
    line_starts: Vec<u32>,
}

impl FileInfo {
    fn new(path: Vec<u8>, src: &'static [u8], base: u32) -> FileInfo {
        let mut line_starts = vec![0u32];
        for (i, &b) in src.iter().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        FileInfo {
            path,
            src,
            base,
            line_starts,
        }
    }

    /// 1-based line and column of a byte offset.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        (line as u32 + 1, offset - self.line_starts[line] + 1)
    }
}

/// Interned symbols the evaluator needs by name.
pub struct WellKnown {
    pub out_path: Sym,
    pub drv_path: Sym,
    pub type_: Sym,
    pub derivation: Sym,
    pub functor: Sym,
    pub to_string: Sym,
    pub name: Sym,
    pub value: Sym,
    pub file: Sym,
    pub line: Sym,
    pub column: Sym,
    pub success: Sym,
    pub right: Sym,
    pub wrong: Sym,
    pub version: Sym,
    pub path: Sym,
    pub prefix: Sym,
}

pub struct Evaluator {
    pub arena: Arena,
    pub interner: Interner,
    pub files: Vec<FileInfo>,
    next_base: u32,
    file_ids: HashMap<Vec<u8>, FileId>,
    pub globals: Globals,
    pub root_env: &'static EnvObj,
    import_cache: HashMap<Vec<u8>, Word>,
    /// `(prefix, path)` entries, `-I` first then `NIX_PATH`.
    pub search_path: Vec<(Vec<u8>, Vec<u8>)>,
    pub call_depth: u32,
    pub try_level: u32,
    pub s: WellKnown,
    pub builtins_word: Word,
    /// Store path prefix, for `builtins.storeDir` and store path checks.
    pub store_dir: Vec<u8>,
    pub current_system: &'static str,
    pub nr_thunks: u64,
    pub nr_function_calls: u64,
    pub t_compile: std::time::Duration,
    /// One shared context-free string per symbol, made on first use.
    sym_strings: Vec<Word>,
    /// Source path -> virtual store path, like Nix's `srcToStore`.
    pub src_to_store: HashMap<Vec<u8>, &'static [u8]>,
    pub compile_settings: crate::compile::CompileSettings,
    pub drv: crate::builtins::derivation::DrvState,
}

macro_rules! bail {
    ($kind:expr, $pos:expr, $($arg:tt)*) => {
        return Err($crate::eval::Error::boxed($kind, format!($($arg)*), $pos))
    };
}
pub(crate) use bail;

impl Evaluator {
    pub fn new() -> Evaluator {
        let mut arena = Arena::new();
        let mut interner = Interner::new();
        let root_env = arena.alloc_env(None, 0);
        let s = WellKnown {
            out_path: interner.intern(b"outPath"),
            drv_path: interner.intern(b"drvPath"),
            type_: interner.intern(b"type"),
            derivation: interner.intern(b"derivation"),
            functor: interner.intern(b"__functor"),
            to_string: interner.intern(b"__toString"),
            name: interner.intern(b"name"),
            value: interner.intern(b"value"),
            file: interner.intern(b"file"),
            line: interner.intern(b"line"),
            column: interner.intern(b"column"),
            success: interner.intern(b"success"),
            right: interner.intern(b"right"),
            wrong: interner.intern(b"wrong"),
            version: interner.intern(b"version"),
            path: interner.intern(b"path"),
            prefix: interner.intern(b"prefix"),
        };
        let mut ev = Evaluator {
            arena,
            interner,
            files: Vec::new(),
            next_base: 0,
            file_ids: HashMap::new(),
            globals: Globals {
                table: FxHashMap::default(),
                builtins: None,
            },
            root_env,
            import_cache: HashMap::new(),
            search_path: Vec::new(),
            call_depth: 0,
            try_level: 0,
            s,
            builtins_word: Word::NULL,
            store_dir: b"/nix/store".to_vec(),
            current_system: "x86_64-linux",
            nr_thunks: 0,
            nr_function_calls: 0,
            t_compile: std::time::Duration::ZERO,
            sym_strings: Vec::new(),
            src_to_store: HashMap::new(),
            compile_settings: Default::default(),
            drv: Default::default(),
        };
        crate::builtins::install(&mut ev);
        ev
    }

    // ---- files ------------------------------------------------------------

    pub fn add_file(&mut self, path: &[u8], src: Vec<u8>) -> FileId {
        if let Some(id) = self.file_ids.get(path) {
            return *id;
        }
        let id = FileId(self.files.len() as u32);
        let src: &'static [u8] = Box::leak(src.into_boxed_slice());
        let base = self.next_base;
        self.next_base = base
            .checked_add(src.len() as u32 + 1)
            .expect("sources exceed 4 GiB");
        self.files.push(FileInfo::new(path.to_vec(), src, base));
        self.file_ids.insert(path.to_vec(), id);
        id
    }

    pub fn file(&self, id: FileId) -> Option<&FileInfo> {
        self.files.get(id.0 as usize)
    }

    /// The file holding `pos` and the offset inside it.
    pub fn file_of(&self, pos: Pos) -> Option<(&FileInfo, u32)> {
        if pos.is_none() {
            return None;
        }
        let i = self.files.partition_point(|f| f.base <= pos.0);
        let f = self.files.get(i.checked_sub(1)?)?;
        Some((f, pos.0 - f.base))
    }

    pub fn describe_pos(&self, pos: Pos) -> String {
        match self.file_of(pos) {
            Some((f, off)) => {
                let (l, c) = f.line_col(off);
                format!("{}:{}:{}", String::from_utf8_lossy(&f.path), l, c)
            }
            _ => "«none»".to_string(),
        }
    }

    /// Parse and compile a source text. `path` is the file name used for
    /// positions; `base_dir` resolves relative path literals.
    pub fn compile_source(
        &mut self,
        path: &[u8],
        src: Vec<u8>,
        base_dir: &[u8],
        extra_scope: Option<&[Sym]>,
    ) -> EvalResult<IrRef> {
        let file = self.add_file(path, src);
        let text = self.files[file.0 as usize].src;
        let base = self.files[file.0 as usize].base;
        let ast = crate::syntax::parse(text, file, &mut self.interner).map_err(|e| {
            Error::boxed(
                ErrorKind::Parse,
                e.msg,
                Pos(base + e.span.start),
            )
        })?;
        let t0 = std::time::Instant::now();
        let before = self.arena.bytes_allocated;
        let mut c = Compiler::new(
            &ast,
            &mut self.arena,
            &mut self.interner,
            &self.globals,
            base,
            base_dir,
            self.compile_settings,
        );
        let r = c.compile_root(extra_scope);
        self.arena.stats.note(Stat::Ir, self.arena.bytes_allocated - before);
        self.t_compile += t0.elapsed();
        r
    }

    /// Evaluate a string expression (`-E`).
    pub fn eval_string(&mut self, src: &str, base_dir: &[u8]) -> EvalResult<Word> {
        let ir = self.compile_source(
            b"\xc2\xabstring\xc2\xbb",
            src.as_bytes().to_vec(),
            base_dir,
            None,
        )?;
        let root = self.root_env;
        self.eval(root, ir)
    }

    /// `import`: parse, compile, evaluate and cache a file.
    pub fn eval_file(&mut self, path: &[u8], pos: Pos) -> EvalResult<Word> {
        let resolved = self.resolve_import_path(path, pos)?;
        if let Some(w) = self.import_cache.get(&resolved) {
            return Ok(*w);
        }
        let src = self.read_source(&resolved, pos)?;
        let dir = dir_of(&resolved).to_vec();
        let ir = self.compile_source(&resolved, src, &dir, None)?;
        let root = self.root_env;
        let w = self.eval(root, ir)?;
        self.import_cache.insert(resolved, w);
        Ok(w)
    }

    /// Like `eval_file` but with `scope` attrs bound above the globals; not
    /// cached.
    pub fn scoped_import(
        &mut self,
        scope: &'static AttrsObj,
        path: &[u8],
        pos: Pos,
    ) -> EvalResult<Word> {
        let resolved = self.resolve_import_path(path, pos)?;
        let src = self.read_source(&resolved, pos)?;
        let names: Vec<Sym> = scope.attrs().iter().map(|a| a.sym).collect();
        let dir = dir_of(&resolved).to_vec();
        let ir = self.compile_source(&resolved, src, &dir, Some(&names))?;
        let env = self.arena.alloc_env(Some(self.root_env), names.len());
        for (slot, a) in env.slots().iter().zip(scope.attrs()) {
            slot.set(a.cell.get());
        }
        self.eval(env, ir)
    }

    fn resolve_import_path(&mut self, path: &[u8], pos: Pos) -> EvalResult<Vec<u8>> {
        let mut p = path.to_vec();
        if p.starts_with(CorePkgs::PREFIX) {
            return Ok(p);
        }
        let meta = std::fs::metadata(std::path::Path::new(std::str::from_utf8(&p).unwrap_or("")));
        match meta {
            Ok(m) if m.is_dir() => {
                p.extend_from_slice(b"/default.nix");
            }
            Ok(_) => {}
            Err(e) => bail!(
                ErrorKind::Io,
                pos,
                "getting status of '{}': {}",
                String::from_utf8_lossy(&p),
                e
            ),
        }
        Ok(p)
    }

    /// `<name>` lookup.
    pub fn find_file(
        &mut self,
        entries: &[(Vec<u8>, Vec<u8>)],
        name: &[u8],
        pos: Pos,
    ) -> EvalResult<Vec<u8>> {
        let (head, rest) = match name.iter().position(|&b| b == b'/') {
            Some(i) => (&name[..i], &name[i..]),
            None => (name, &b""[..]),
        };
        for (prefix, path) in entries {
            let candidate = if prefix.is_empty() {
                let mut c = path.clone();
                c.push(b'/');
                c.extend_from_slice(name);
                c
            } else if prefix.as_slice() == head {
                let mut c = path.clone();
                c.extend_from_slice(rest);
                c
            } else {
                continue;
            };
            let cp = crate::compile::canon_path(&candidate);
            if std::path::Path::new(std::str::from_utf8(&cp).unwrap_or("")).exists() {
                return Ok(cp);
            }
        }
        if head == b"nix" && crate::builtins::corepkgs(&rest[1..]).is_some() {
            let mut p = CorePkgs::PREFIX.to_vec();
            p.extend_from_slice(&rest[1..]);
            return Ok(p);
        }
        bail!(
            ErrorKind::Throw,
            pos,
            "file '{}' was not found in the Nix search path (add it using $NIX_PATH or -I)",
            String::from_utf8_lossy(name)
        )
    }

    fn read_source(&self, resolved: &[u8], pos: Pos) -> EvalResult<Vec<u8>> {
        if let Some(rest) = resolved.strip_prefix(CorePkgs::PREFIX)
            && let Some(src) = crate::builtins::corepkgs(rest)
        {
            return Ok(src.to_vec());
        }
        match std::fs::read(std::path::Path::new(
            std::str::from_utf8(resolved).unwrap_or(""),
        )) {
            Ok(s) => Ok(s),
            Err(e) => bail!(
                ErrorKind::Io,
                pos,
                "opening file '{}': {}",
                String::from_utf8_lossy(resolved),
                e
            ),
        }
    }

    // ---- forcing ----------------------------------------------------------

    /// Force a word to weak head normal form.
    #[inline]
    pub fn force(&mut self, w: Word) -> EvalResult<Word> {
        if w.is_thunk() {
            self.force_thunk(w)
        } else {
            Ok(w)
        }
    }

    /// Force the word in a cell and write the result back.
    #[inline]
    pub fn force_cell(&mut self, cell: &Cell) -> EvalResult<Word> {
        let w = cell.get();
        if !w.is_thunk() {
            return Ok(w);
        }
        let v = self.force_thunk(w)?;
        cell.set(v);
        Ok(v)
    }

    fn force_thunk(&mut self, w: Word) -> EvalResult<Word> {
        let result = w.thunk_result_cell();
        let r = result.get();
        if r != Word::EMPTY {
            if r == Word::BLACKHOLE {
                bail!(
                    ErrorKind::InfiniteRecursion,
                    self.thunk_pos(w),
                    "infinite recursion encountered"
                );
            }
            return Ok(r);
        }
        result.set(Word::BLACKHOLE);
        let v = match w.thunk_kind() {
            Kind::Thunk => {
                let t = w.as_thunk();
                self.eval(t.env, t.ir)
            }
            _ => {
                let t = w.as_thunk_app();
                self.apply(t.func, &[t.arg], Pos::NONE)
            }
        };
        match v {
            Ok(v) => {
                debug_assert!(!v.is_thunk());
                result.set(v);
                Ok(v)
            }
            Err(e) => {
                result.set(Word::EMPTY);
                Err(e)
            }
        }
    }

    fn thunk_pos(&self, w: Word) -> Pos {
        match w.thunk_kind() {
            Kind::Thunk => ir_pos(w.as_thunk().ir),
            _ => Pos::NONE,
        }
    }

    /// Wrap `ir` for lazy evaluation in `env`, sharing existing values and
    /// thunks for cheap expressions.
    #[inline]
    pub fn maybe_thunk(&mut self, env: &'static EnvObj, ir: IrRef) -> Word {
        match ir {
            Ir::Const(w) => *w,
            Ir::Local { depth, idx } => {
                let mut e = env;
                for _ in 0..*depth {
                    e = e.up().expect("compile-time depth is valid");
                }
                let w = e.slots()[*idx as usize].get();
                // a slot of the env under construction (`let x = x;`): thunk it
                // so forcing reports infinite recursion instead of reading EMPTY
                if w == Word::EMPTY {
                    self.nr_thunks += 1;
                    return self.arena.mk_thunk(env, ir);
                }
                w
            }
            Ir::Lambda(l) => self.arena.mk_lambda(env, l),
            // `n - 1` on an already-forced int cannot fail except on
            // overflow, so compute it now instead of allocating a thunk
            Ir::Sub(l, r, _) => match (self.forced_int(env, l), self.forced_int(env, r)) {
                (Some(a), Some(b)) => match a.checked_sub(b) {
                    Some(v) if Word::fits_imm(v) => Word::imm_int(v),
                    _ => self.mk_thunk_counted(env, ir),
                },
                _ => self.mk_thunk_counted(env, ir),
            },
            Ir::Concat {
                force_string: false,
                parts,
                ..
            } if parts.len() == 2 => match (
                self.forced_int(env, parts[0]),
                self.forced_int(env, parts[1]),
            ) {
                (Some(a), Some(b)) => match a.checked_add(b) {
                    Some(v) if Word::fits_imm(v) => Word::imm_int(v),
                    _ => self.mk_thunk_counted(env, ir),
                },
                _ => self.mk_thunk_counted(env, ir),
            },
            _ => self.mk_thunk_counted(env, ir),
        }
    }

    #[inline]
    fn mk_thunk_counted(&mut self, env: &'static EnvObj, ir: IrRef) -> Word {
        self.nr_thunks += 1;
        self.arena.mk_thunk(env, ir)
    }

    /// The value of a cheap operand if it is already a forced integer.
    #[inline]
    fn forced_int(&self, env: &'static EnvObj, ir: IrRef) -> Option<i64> {
        let w = match ir {
            Ir::Const(w) => *w,
            Ir::Local { depth, idx } => {
                let mut e = env;
                for _ in 0..*depth {
                    e = e.up()?;
                }
                e.slots()[*idx as usize].get()
            }
            _ => return None,
        };
        if w.is_imm_int() {
            Some(w.imm_int_value())
        } else {
            None
        }
    }

    /// Recursively force lists and attrs (`--strict`).
    pub fn force_deep(&mut self, w: Word) -> EvalResult<Word> {
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let w = self.force(w)?;
        self.force_deep_inner(w, &mut seen)?;
        Ok(w)
    }

    fn force_deep_inner(
        &mut self,
        w: Word,
        seen: &mut std::collections::HashSet<u64>,
    ) -> EvalResult<()> {
        match w.view() {
            Value::Attrs(a) => {
                if !seen.insert(w.0) {
                    return Ok(());
                }
                for attr in a.attrs() {
                    let v = self.force_cell(&attr.cell).map_err(|mut e| {
                        e.add_trace(
                            attr.pos,
                            format!(
                                "while evaluating the attribute '{}'",
                                self.interner.name_str(attr.sym)
                            ),
                        );
                        e
                    })?;
                    self.force_deep_inner(v, seen)?;
                }
            }
            Value::List(l) => {
                if !seen.insert(w.0) {
                    return Ok(());
                }
                for c in l.cells() {
                    let v = self.force_cell(c)?;
                    self.force_deep_inner(v, seen)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ---- typed forcing helpers ------------------------------------------

    pub fn type_error<T>(&self, expected: &str, got: Word, pos: Pos, ctx: &str) -> EvalResult<T> {
        let mut e = Error::new(
            ErrorKind::Type,
            format!(
                "expected {} but found {}: {}",
                expected,
                got.value_type().name(),
                crate::print::show_value_short(self, got)
            ),
            pos,
        );
        if !ctx.is_empty() {
            e.add_trace(pos, ctx);
        }
        Err(Box::new(e))
    }

    #[inline]
    pub fn force_attrs(&mut self, w: Word, pos: Pos, ctx: &str) -> EvalResult<&'static AttrsObj> {
        let w = self.force(w)?;
        match w.view() {
            Value::Attrs(a) => Ok(a),
            _ => self.type_error("a set", w, pos, ctx),
        }
    }

    #[inline]
    pub fn force_list(&mut self, w: Word, pos: Pos, ctx: &str) -> EvalResult<&'static ListObj> {
        let w = self.force(w)?;
        match w.view() {
            Value::List(l) => Ok(l),
            _ => self.type_error("a list", w, pos, ctx),
        }
    }

    #[inline]
    pub fn force_int(&mut self, w: Word, pos: Pos, ctx: &str) -> EvalResult<i64> {
        let w = self.force(w)?;
        match w.view() {
            Value::Int(i) => Ok(i),
            _ => self.type_error("an integer", w, pos, ctx),
        }
    }

    pub fn force_float(&mut self, w: Word, pos: Pos, ctx: &str) -> EvalResult<f64> {
        let w = self.force(w)?;
        match w.view() {
            Value::Int(i) => Ok(i as f64),
            Value::Float(f) => Ok(f),
            _ => self.type_error("a float", w, pos, ctx),
        }
    }

    #[inline]
    pub fn force_bool(&mut self, w: Word, pos: Pos, ctx: &str) -> EvalResult<bool> {
        let w = self.force(w)?;
        match w.view() {
            Value::Bool(b) => Ok(b),
            _ => self.type_error("a Boolean", w, pos, ctx),
        }
    }

    /// Force a string, returning the object (with context).
    #[inline]
    pub fn force_string(&mut self, w: Word, pos: Pos, ctx: &str) -> EvalResult<&'static StringObj> {
        let w = self.force(w)?;
        match w.view() {
            Value::String(s) => Ok(s),
            _ => self.type_error("a string", w, pos, ctx),
        }
    }

    /// Force a string that must not carry context.
    pub fn force_string_no_ctx(
        &mut self,
        w: Word,
        pos: Pos,
        ctx: &str,
    ) -> EvalResult<&'static [u8]> {
        let s = self.force_string(w, pos, ctx)?;
        if !s.ctx.is_empty() {
            let mut e = Error::new(
                ErrorKind::Eval,
                format!(
                    "the string '{}' is not allowed to refer to a store path (such as '{}')",
                    String::from_utf8_lossy(s.bytes),
                    crate::print::ctx_display(&s.ctx[0])
                ),
                pos,
            );
            if !ctx.is_empty() {
                e.add_trace(pos, ctx);
            }
            return Err(Box::new(e));
        }
        Ok(s.bytes)
    }

    pub fn force_function(&mut self, w: Word, pos: Pos, ctx: &str) -> EvalResult<Word> {
        let w = self.force(w)?;
        match w.view() {
            Value::Lambda(_) | Value::PrimOp(_) | Value::PrimOpApp(_) => Ok(w),
            Value::Attrs(a) if a.get(self.s.functor).is_some() => Ok(w),
            _ => self.type_error("a function", w, pos, ctx),
        }
    }

    pub fn is_derivation(&mut self, w: Word) -> bool {
        match w.view() {
            Value::Attrs(a) => match a.get(self.s.type_) {
                Some(attr) => match self.force_cell(&attr.cell) {
                    Ok(t) => t
                        .as_string()
                        .map(|s| s.bytes == b"derivation")
                        .unwrap_or(false),
                    Err(_) => false,
                },
                None => false,
            },
            _ => false,
        }
    }

    // ---- construction helpers --------------------------------------------

    pub fn mk_string(&mut self, bytes: &[u8]) -> Word {
        self.arena.mk_string(bytes)
    }

    pub fn mk_string_ctx(&mut self, bytes: &[u8], ctx: &[Ctx]) -> Word {
        self.arena.mk_string_ctx(bytes, ctx)
    }

    pub fn mk_int(&mut self, i: i64) -> Word {
        self.arena.mk_int(i)
    }

    pub fn mk_float(&mut self, f: f64) -> Word {
        self.arena.mk_float(f)
    }

    pub fn mk_list(&mut self, items: &[Word]) -> Word {
        self.arena.mk_list(items)
    }

    pub fn mk_attrs(&mut self, entries: &mut [(Sym, Pos, Word)], pos: Pos) -> EvalResult<Word> {
        match self.arena.mk_attrs(entries) {
            Ok(w) => Ok(w),
            Err(dup) => bail!(
                ErrorKind::Eval,
                pos,
                "attribute '{}' already defined",
                self.interner.name_str(dup)
            ),
        }
    }

    pub fn empty_attrs(&mut self) -> Word {
        self.arena.mk_attrs_sorted(&[])
    }

    pub fn sym(&mut self, name: &[u8]) -> Sym {
        self.interner.intern(name)
    }

    /// The string value of a symbol's name. attrNames and mapAttrs hand
    /// out the same object for the same name instead of copying it.
    pub fn sym_string(&mut self, sym: Sym) -> Word {
        let i = sym.0 as usize;
        if i >= self.sym_strings.len() {
            self.sym_strings.resize(self.interner.len(), Word::EMPTY);
        }
        let w = self.sym_strings[i];
        if w != Word::EMPTY {
            return w;
        }
        let w = self.arena.mk_string(self.interner.name(sym));
        self.sym_strings[i] = w;
        w
    }

    /// `{ file, line, column }` for `__curPos` and `unsafeGetAttrPos`.
    pub fn mk_pos_attrs(&mut self, pos: Pos) -> Word {
        let Some((f, off)) = self.file_of(pos) else {
            return Word::NULL;
        };
        if f.path.starts_with(b"\xc2\xab") {
            return Word::NULL;
        }
        let (line, col) = f.line_col(off);
        let path = f.path.clone();
        let file = self.mk_string(&path);
        let line = self.mk_int(line as i64);
        let col = self.mk_int(col as i64);
        let mut entries = vec![
            (self.s.file, Pos::NONE, file),
            (self.s.line, Pos::NONE, line),
            (self.s.column, Pos::NONE, col),
        ];
        self.arena.mk_attrs(&mut entries).expect("distinct keys")
    }

    // ---- function application --------------------------------------------

    #[inline]
    fn enter_call(&mut self, pos: Pos) -> EvalResult<()> {
        self.call_depth += 1;
        if self.call_depth > MAX_CALL_DEPTH {
            self.call_depth -= 1;
            bail!(
                ErrorKind::StackOverflow,
                pos,
                "stack overflow; max-call-depth exceeded"
            );
        }
        Ok(())
    }

    /// Apply `func` to `args` left to right. Returns a forced value.
    pub fn apply(&mut self, func: Word, args: &[Word], pos: Pos) -> EvalResult<Word> {
        self.enter_call(pos)?;
        let r = self.apply_inner(func, args, pos);
        self.call_depth -= 1;
        r
    }

    fn apply_inner(&mut self, func: Word, args: &[Word], pos: Pos) -> EvalResult<Word> {
        let mut cur = self.force(func)?;
        let mut args = args;
        while !args.is_empty() {
            match cur.view() {
                Value::Lambda(l) => {
                    self.nr_function_calls += 1;
                    cur = self.call_lambda(l, args[0], pos)?;
                    args = &args[1..];
                }
                Value::PrimOp(op) => {
                    let arity = op.arity as usize;
                    if args.len() < arity {
                        return Ok(self.arena.mk_primop_app(op, args));
                    }
                    cur = (op.f)(self, &args[..arity], pos).map_err(|mut e| {
                        e.add_trace(pos, format!("while calling the '{}' builtin", op.name));
                        e
                    })?;
                    args = &args[arity..];
                }
                Value::PrimOpApp(app) => {
                    let op = app.op;
                    let arity = op.arity as usize;
                    let need = arity - app.args.len();
                    if args.len() < need {
                        let mut all = app.args.to_vec();
                        all.extend_from_slice(args);
                        return Ok(self.arena.mk_primop_app(op, &all));
                    }
                    let mut all: [Word; 8] = [Word::EMPTY; 8];
                    all[..app.args.len()].copy_from_slice(app.args);
                    all[app.args.len()..arity].copy_from_slice(&args[..need]);
                    cur = (op.f)(self, &all[..arity], pos).map_err(|mut e| {
                        e.add_trace(pos, format!("while calling the '{}' builtin", op.name));
                        e
                    })?;
                    args = &args[need..];
                }
                Value::Attrs(a) => {
                    let Some(functor) = a.get(self.s.functor) else {
                        bail!(
                            ErrorKind::Type,
                            pos,
                            "attempt to call something which is not a function but a set: {}",
                            crate::print::show_value_short(self, cur)
                        );
                    };
                    let f = self.force_cell(&functor.cell)?;
                    cur = self.apply(f, &[cur], pos)?;
                }
                _ => bail!(
                    ErrorKind::Type,
                    pos,
                    "attempt to call something which is not a function but {}: {}",
                    cur.value_type().name(),
                    crate::print::show_value_short(self, cur)
                ),
            }
        }
        Ok(cur)
    }

    fn call_lambda(&mut self, l: &'static LambdaObj, arg: Word, pos: Pos) -> EvalResult<Word> {
        let lam: &'static LambdaIr = l.lam;
        let env2 = self.arena.alloc_env(Some(l.env), lam.nslots());
        let slots = env2.slots();
        match &lam.param {
            ParamIr::Ident => slots[0].set(arg),
            ParamIr::Formals {
                has_bind,
                formals,
                ellipsis,
            } => {
                let attrs = self
                    .force_attrs(
                        arg,
                        lam.pos,
                        "while evaluating the value passed for the lambda argument",
                    )
                    .map_err(|mut e| {
                        if !pos.is_none() {
                            e.add_trace(pos, "from call site");
                        }
                        e
                    })?;
                let mut i = 0;
                if *has_bind {
                    slots[0].set(arg);
                    i = 1;
                }
                let mut used = 0;
                for f in formals.iter() {
                    match attrs.get(f.sym) {
                        Some(a) => {
                            used += 1;
                            slots[i].set(a.cell.get());
                        }
                        None => match f.default {
                            Some(d) => {
                                let w = self.maybe_thunk(env2, d);
                                slots[i].set(w);
                            }
                            None => {
                                let mut e = Error::new(
                                    ErrorKind::Type,
                                    format!(
                                        "function '{}' called without required argument '{}'",
                                        self.lambda_name(lam),
                                        self.interner.name_str(f.sym)
                                    ),
                                    lam.pos,
                                );
                                e.add_trace(pos, "from call site");
                                return Err(Box::new(e));
                            }
                        },
                    }
                    i += 1;
                }
                if !*ellipsis && used != attrs.len() {
                    for a in attrs.attrs() {
                        if formals.binary_search_by(|f| f.sym.cmp(&a.sym)).is_err() {
                            let mut e = Error::new(
                                ErrorKind::Type,
                                format!(
                                    "function '{}' called with unexpected argument '{}'",
                                    self.lambda_name(lam),
                                    self.interner.name_str(a.sym)
                                ),
                                lam.pos,
                            );
                            e.add_trace(pos, "from call site");
                            return Err(Box::new(e));
                        }
                    }
                }
            }
        }
        self.eval(env2, lam.body).map_err(|mut e| {
            e.add_trace(lam.pos, format!("while calling {}", self.lambda_desc(lam)));
            if !pos.is_none() {
                e.add_trace(pos, "from call site");
            }
            e
        })
    }

    fn lambda_name(&self, lam: &LambdaIr) -> String {
        match lam.name {
            Some(n) => self.interner.name_str(n).to_string(),
            None => "anonymous lambda".to_string(),
        }
    }

    fn lambda_desc(&self, lam: &LambdaIr) -> String {
        match lam.name {
            Some(n) => format!("'{}'", self.interner.name_str(n)),
            None => "anonymous lambda".to_string(),
        }
    }

    /// Convenience: call with one argument.
    #[inline]
    pub fn call1(&mut self, f: Word, a: Word, pos: Pos) -> EvalResult<Word> {
        self.apply(f, &[a], pos)
    }

    #[inline]
    pub fn call2(&mut self, f: Word, a: Word, b: Word, pos: Pos) -> EvalResult<Word> {
        self.apply(f, &[a, b], pos)
    }

    // ---- evaluation -------------------------------------------------------

    /// Evaluate `ir` in `env` to weak head normal form.
    pub fn eval(&mut self, env: &'static EnvObj, ir: IrRef) -> EvalResult<Word> {
        match ir {
            Ir::Const(w) => Ok(*w),
            Ir::Local { depth, idx } => {
                let mut e = env;
                for _ in 0..*depth {
                    e = e.up().expect("compile-time depth is valid");
                }
                self.force_cell(&e.slots()[*idx as usize])
            }
            Ir::WithVar { sym, withs, pos } => self.eval_with_var(env, *sym, withs, *pos),
            Ir::CurPos(pos) => Ok(self.mk_pos_attrs(*pos)),
            Ir::Concat {
                force_string,
                parts,
                pos,
            } => self.eval_concat(env, *force_string, parts, *pos),
            Ir::SearchPath { name, pos } => {
                let n = self.eval(env, name)?;
                let n = self.force_string_no_ctx(n, *pos, "while evaluating a path segment")?;
                let entries = self.search_path.clone();
                let p = self.find_file(&entries, n, *pos)?;
                Ok(self.arena.mk_path(&p))
            }
            Ir::Select {
                subject,
                path,
                default,
                pos,
            } => self.eval_select(env, subject, path, *default, *pos),
            Ir::HasAttr { subject, path } => {
                let mut v = self.eval(env, subject)?;
                for elem in path.iter() {
                    let name = self.attr_name(env, elem)?;
                    let attrs = self.force(v)?;
                    match attrs.as_attrs().and_then(|a| a.get(name)) {
                        Some(a) => v = a.cell.get(),
                        None => return Ok(Word::FALSE),
                    }
                }
                Ok(Word::TRUE)
            }
            Ir::Attrs(a) => self.eval_attrs(env, a),
            Ir::List(items) => {
                let l = self.arena.alloc_list(items.len());
                for (c, item) in l.cells().iter().zip(items.iter()) {
                    let w = self.maybe_thunk(env, item);
                    c.set(w);
                }
                Ok(Word::from_ptr(l))
            }
            Ir::Lambda(l) => Ok(self.arena.mk_lambda(env, l)),
            Ir::Apply { func, args, pos } => {
                let f = self.eval(env, func)?;
                let mut buf: [Word; 8] = [Word::EMPTY; 8];
                if args.len() <= 8 {
                    for (i, a) in args.iter().enumerate() {
                        buf[i] = self.maybe_thunk(env, a);
                    }
                    self.apply(f, &buf[..args.len()], *pos)
                } else {
                    let v: Vec<Word> = args.iter().map(|a| self.maybe_thunk(env, a)).collect();
                    self.apply(f, &v, *pos)
                }
            }
            Ir::Let { binds, from, body } => {
                let env2 = self.alloc_rec_env(env, binds, from);
                self.eval(env2, body)
            }
            Ir::With { scope, body, .. } => {
                let env2 = self.arena.alloc_env(Some(env), 1);
                let w = self.maybe_thunk(env, scope);
                env2.slots()[0].set(w);
                self.eval(env2, body)
            }
            Ir::If {
                cond,
                then,
                else_,
                pos,
            } => {
                let c = self.eval_bool(env, cond, *pos, "while evaluating a branch condition")?;
                self.eval(env, if c { then } else { else_ })
            }
            Ir::Assert { cond, body, pos } => {
                if !self.eval_bool(env, cond, *pos, "in the condition of the assert statement")? {
                    bail!(ErrorKind::Assert, *pos, "assertion failed");
                }
                self.eval(env, body)
            }
            Ir::Not(e, pos) => {
                let b = self.eval_bool(env, e, *pos, "in the argument of the not operator")?;
                Ok(Word::from_bool(!b))
            }
            Ir::Neg(e, pos) => {
                let v = self.eval(env, e)?;
                let zero = Word::imm_int(0);
                self.arith(zero, v, ArithOp::Sub, *pos)
            }
            Ir::Eq(l, r, pos) => {
                let a = self.eval(env, l)?;
                let b = self.eval(env, r)?;
                Ok(Word::from_bool(self.eq_values_top(a, b, *pos)?))
            }
            Ir::Ne(l, r, pos) => {
                let a = self.eval(env, l)?;
                let b = self.eval(env, r)?;
                Ok(Word::from_bool(!self.eq_values_top(a, b, *pos)?))
            }
            Ir::Lt(l, r, pos) => {
                let a = self.eval(env, l)?;
                let b = self.eval(env, r)?;
                Ok(Word::from_bool(self.less_than(a, b, *pos)?))
            }
            Ir::Gt(l, r, pos) => {
                let a = self.eval(env, l)?;
                let b = self.eval(env, r)?;
                Ok(Word::from_bool(self.less_than(b, a, *pos)?))
            }
            Ir::Le(l, r, pos) => {
                let a = self.eval(env, l)?;
                let b = self.eval(env, r)?;
                Ok(Word::from_bool(!self.less_than(b, a, *pos)?))
            }
            Ir::Ge(l, r, pos) => {
                let a = self.eval(env, l)?;
                let b = self.eval(env, r)?;
                Ok(Word::from_bool(!self.less_than(a, b, *pos)?))
            }
            Ir::And(l, r, pos) => {
                if !self.eval_bool(env, l, *pos, "in the left operand of the AND (&&) operator")? {
                    return Ok(Word::FALSE);
                }
                let b = self.eval_bool(
                    env,
                    r,
                    *pos,
                    "in the right operand of the AND (&&) operator",
                )?;
                Ok(Word::from_bool(b))
            }
            Ir::Or(l, r, pos) => {
                if self.eval_bool(env, l, *pos, "in the left operand of the OR (||) operator")? {
                    return Ok(Word::TRUE);
                }
                let b =
                    self.eval_bool(env, r, *pos, "in the right operand of the OR (||) operator")?;
                Ok(Word::from_bool(b))
            }
            Ir::Impl(l, r, pos) => {
                if !self.eval_bool(
                    env,
                    l,
                    *pos,
                    "in the left operand of the IMPL (->) operator",
                )? {
                    return Ok(Word::TRUE);
                }
                let b = self.eval_bool(
                    env,
                    r,
                    *pos,
                    "in the right operand of the IMPL (->) operator",
                )?;
                Ok(Word::from_bool(b))
            }
            Ir::Update(l, r, pos) => {
                let a = self.eval(env, l)?;
                let a =
                    self.force_attrs(a, *pos, "in the left operand of the update (//) operator")?;
                let b = self.eval(env, r)?;
                let b =
                    self.force_attrs(b, *pos, "in the right operand of the update (//) operator")?;
                Ok(self.update_attrs(a, b))
            }
            Ir::ConcatLists(l, r, pos) => {
                let a = self.eval(env, l)?;
                let b = self.eval(env, r)?;
                let a = self.force_list(
                    a,
                    *pos,
                    "in the left operand of the list concatenation operator",
                )?;
                let b = self.force_list(
                    b,
                    *pos,
                    "in the right operand of the list concatenation operator",
                )?;
                Ok(self.concat_lists(&[a, b]))
            }
            Ir::Sub(l, r, pos) => {
                let a = self.eval(env, l)?;
                let b = self.eval(env, r)?;
                self.arith(a, b, ArithOp::Sub, *pos)
            }
            Ir::Mul(l, r, pos) => {
                let a = self.eval(env, l)?;
                let b = self.eval(env, r)?;
                self.arith(a, b, ArithOp::Mul, *pos)
            }
            Ir::Div(l, r, pos) => {
                let a = self.eval(env, l)?;
                let b = self.eval(env, r)?;
                self.arith(a, b, ArithOp::Div, *pos)
            }
        }
    }

    #[inline]
    fn eval_bool(
        &mut self,
        env: &'static EnvObj,
        ir: IrRef,
        pos: Pos,
        ctx: &str,
    ) -> EvalResult<bool> {
        let v = self.eval(env, ir)?;
        match v {
            Word::TRUE => Ok(true),
            Word::FALSE => Ok(false),
            _ => self.type_error("a Boolean", v, pos, ctx),
        }
    }

    fn eval_with_var(
        &mut self,
        env: &'static EnvObj,
        sym: Sym,
        withs: &[u32],
        pos: Pos,
    ) -> EvalResult<Word> {
        let mut e = env;
        let mut walked = 0u32;
        for &d in withs {
            for _ in walked..d {
                e = e.up().expect("compile-time depth is valid");
            }
            walked = d;
            let attrs = self.force_attrs(
                e.slots()[0].get(),
                pos,
                "while evaluating the first subexpression of a with expression",
            )?;
            e.slots()[0].set(Word::from_ptr(attrs));
            if let Some(a) = attrs.get(sym) {
                return self.force_cell(&a.cell);
            }
        }
        if let Some(w) = self.globals.table.get(&sym) {
            return Ok(*w);
        }
        bail!(
            ErrorKind::UndefinedVar,
            pos,
            "undefined variable '{}'",
            self.interner.name_str(sym)
        )
    }

    fn attr_name(&mut self, env: &'static EnvObj, elem: &AttrElemIr) -> EvalResult<Sym> {
        match elem {
            AttrElemIr::Static(s) => Ok(*s),
            AttrElemIr::Dynamic(e) => {
                let pos = ir_pos(e);
                let v = self.eval(env, e)?;
                let name =
                    self.force_string_no_ctx(v, pos, "while evaluating an attribute name")?;
                Ok(self.interner.intern(name))
            }
        }
    }

    fn eval_select(
        &mut self,
        env: &'static EnvObj,
        subject: IrRef,
        path: &[AttrElemIr],
        default: Option<IrRef>,
        pos: Pos,
    ) -> EvalResult<Word> {
        let mut v = self.eval(env, subject)?;
        let mut pos2 = Pos::NONE;
        for elem in path {
            let name = self.attr_name(env, elem)?;
            match default {
                Some(d) => match v.as_attrs().and_then(|a| a.get(name)) {
                    Some(a) => {
                        pos2 = a.pos;
                        v = self.force_cell(&a.cell).map_err(|mut e| {
                            e.add_trace(
                                pos2,
                                format!(
                                    "while evaluating the attribute '{}'",
                                    self.show_attr_path(env, path)
                                ),
                            );
                            e
                        })?;
                    }
                    None => return self.eval(env, d),
                },
                None => {
                    let attrs = self.force_attrs(v, pos, "while selecting an attribute")?;
                    match attrs.get(name) {
                        Some(a) => {
                            pos2 = a.pos;
                            v = self.force_cell(&a.cell).map_err(|mut e| {
                                e.add_trace(
                                    pos2,
                                    format!(
                                        "while evaluating the attribute '{}'",
                                        self.show_attr_path(env, path)
                                    ),
                                );
                                e
                            })?;
                        }
                        None => bail!(
                            ErrorKind::MissingAttr,
                            pos,
                            "attribute '{}' missing",
                            self.interner.name_str(name)
                        ),
                    }
                }
            }
        }
        let _ = pos2;
        Ok(v)
    }

    fn show_attr_path(&mut self, env: &'static EnvObj, path: &[AttrElemIr]) -> String {
        let mut out = String::new();
        for (i, e) in path.iter().enumerate() {
            if i > 0 {
                out.push('.');
            }
            match e {
                AttrElemIr::Static(s) => out.push_str(self.interner.name_str(*s)),
                AttrElemIr::Dynamic(d) => match self.eval(env, d) {
                    Ok(v) if v.as_string().is_some() => {
                        out.push_str(&String::from_utf8_lossy(v.as_string().unwrap().bytes))
                    }
                    _ => out.push_str("\"${...}\""),
                },
            }
        }
        out
    }

    /// Env for `let` and `rec { }`: slots for binds then `from` thunks.
    fn alloc_rec_env(
        &mut self,
        env: &'static EnvObj,
        binds: &'static [BindIr],
        from: &'static [IrRef],
    ) -> &'static EnvObj {
        let env2 = self.arena.alloc_env(Some(env), binds.len() + from.len());
        let slots = env2.slots();
        for (i, b) in binds.iter().enumerate() {
            let w = match b.scope {
                BindScope::Outer => self.maybe_thunk(env, b.value),
                _ => self.maybe_thunk(env2, b.value),
            };
            slots[i].set(w);
        }
        for (j, f) in from.iter().enumerate() {
            let w = self.maybe_thunk(env2, f);
            slots[binds.len() + j].set(w);
        }
        env2
    }

    fn eval_attrs(&mut self, env: &'static EnvObj, a: &'static AttrsIr) -> EvalResult<Word> {
        if a.dynamic.is_empty() && a.overrides.is_none() {
            return Ok(self.eval_static_attrs(env, a));
        }
        let mut entries: Vec<(Sym, Pos, Word)> =
            Vec::with_capacity(a.binds.len() + a.dynamic.len());
        let dyn_env: &'static EnvObj;
        let mut needs_sort = false;
        if a.rec {
            let env2 = self.alloc_rec_env(env, a.binds, a.from);
            dyn_env = env2;
            for (i, b) in a.binds.iter().enumerate() {
                entries.push((b.sym, b.pos, env2.slots()[i].get()));
            }
            if let Some(oi) = a.overrides {
                let ow = env2.slots()[oi as usize].get();
                let overrides =
                    self.force_attrs(ow, a.pos, "while evaluating the `__overrides` attribute")?;
                for o in overrides.attrs() {
                    match a.binds.binary_search_by(|b| b.sym.cmp(&o.sym)) {
                        Ok(j) => {
                            entries[j].2 = o.cell.get();
                            env2.slots()[j].set(o.cell.get());
                        }
                        Err(_) => entries.push((o.sym, o.pos, o.cell.get())),
                    }
                }
                needs_sort = true;
            }
        } else {
            dyn_env = env;
            let inherit_env = self.inherit_env(env, a.from);
            for b in a.binds.iter() {
                let w = match b.scope {
                    BindScope::From => self.maybe_thunk(inherit_env.unwrap(), b.value),
                    _ => self.maybe_thunk(env, b.value),
                };
                entries.push((b.sym, b.pos, w));
            }
        }
        for d in a.dynamic.iter() {
            let nv = self.eval(dyn_env, d.name)?;
            if nv == Word::NULL {
                continue;
            }
            let name = self.force_string_no_ctx(
                nv,
                d.pos,
                "while evaluating the name of a dynamic attribute",
            )?;
            let sym = self.interner.intern(name);
            if entries.iter().any(|e| e.0 == sym) {
                bail!(
                    ErrorKind::Eval,
                    d.pos,
                    "dynamic attribute '{}' already defined",
                    self.interner.name_str(sym)
                );
            }
            let w = self.maybe_thunk(dyn_env, d.value);
            entries.push((sym, d.pos, w));
            needs_sort = true;
        }
        if needs_sort {
            self.mk_attrs(&mut entries, a.pos)
        } else {
            Ok(self.arena.mk_attrs_sorted(&entries))
        }
    }

    /// The common case: static names only, so the binds are already sorted
    /// and the set is filled in place without a temporary vector.
    fn eval_static_attrs(&mut self, env: &'static EnvObj, a: &'static AttrsIr) -> Word {
        let out = self.arena.alloc_attrs(a.binds.len());
        if a.rec {
            let env2 = self.alloc_rec_env(env, a.binds, a.from);
            for (i, b) in a.binds.iter().enumerate() {
                out.init(i, b.sym, b.pos, env2.slots()[i].get());
            }
        } else {
            let inherit_env = self.inherit_env(env, a.from);
            for (i, b) in a.binds.iter().enumerate() {
                let w = match b.scope {
                    BindScope::From => self.maybe_thunk(inherit_env.unwrap(), b.value),
                    _ => self.maybe_thunk(env, b.value),
                };
                out.init(i, b.sym, b.pos, w);
            }
        }
        Word::from_ptr(out as *const AttrsObj)
    }

    /// Env holding the `inherit (e)` source thunks of a non-rec set.
    fn inherit_env(
        &mut self,
        env: &'static EnvObj,
        from: &'static [IrRef],
    ) -> Option<&'static EnvObj> {
        if from.is_empty() {
            return None;
        }
        let ie = self.arena.alloc_env(Some(env), from.len());
        for (j, f) in from.iter().enumerate() {
            let w = self.maybe_thunk(env, f);
            ie.slots()[j].set(w);
        }
        Some(ie)
    }

    /// Kept out of `eval` so its buffers do not widen every eval frame.
    #[inline(never)]
    fn eval_concat(
        &mut self,
        env: &'static EnvObj,
        force_string: bool,
        parts: &[IrRef],
        pos: Pos,
    ) -> EvalResult<Word> {
        let mut first = !force_string;
        let mut first_type = ValueType::String;
        let mut n: i64 = 0;
        let mut nf: f64 = 0.0;
        // Interpolations have few parts and coerce to borrowed slices: keep
        // them on the stack and spill to a Vec only past the buffer.
        let mut buf: [&'static [u8]; 16] = [&[]; 16];
        let mut nbuf = 0usize;
        let mut spill: Vec<Cow<'static, [u8]>> = Vec::new();
        let mut ctx: Vec<Ctx> = Vec::new();
        for part in parts {
            let v = self.eval(env, part)?;
            let ppos = ir_pos(part);
            if first {
                first_type = v.value_type();
            }
            match first_type {
                ValueType::Int => match v.view() {
                    Value::Int(i) => match n.checked_add(i) {
                        Some(x) => n = x,
                        None => bail!(
                            ErrorKind::Eval,
                            ppos,
                            "integer overflow in adding {} + {}",
                            n,
                            i
                        ),
                    },
                    Value::Float(f) => {
                        first_type = ValueType::Float;
                        nf = n as f64 + f;
                    }
                    _ => bail!(
                        ErrorKind::Eval,
                        ppos,
                        "cannot add {} to an integer",
                        v.value_type().name()
                    ),
                },
                ValueType::Float => match v.view() {
                    Value::Int(i) => nf += i as f64,
                    Value::Float(f) => nf += f,
                    _ => bail!(
                        ErrorKind::Eval,
                        ppos,
                        "cannot add {} to a float",
                        v.value_type().name()
                    ),
                },
                _ => {
                    let s = self.coerce_to_string(
                        v,
                        &mut ctx,
                        ppos,
                        "while evaluating a path segment",
                        false,
                        first_type == ValueType::String,
                        !first,
                    )?;
                    match s {
                        Cow::Borrowed(b) if spill.is_empty() && nbuf < buf.len() => {
                            buf[nbuf] = b;
                            nbuf += 1;
                        }
                        s => {
                            if spill.is_empty() {
                                spill.extend(buf[..nbuf].iter().map(|b| Cow::Borrowed(*b)));
                                nbuf = 0;
                            }
                            spill.push(s);
                        }
                    }
                }
            }
            first = false;
        }
        let spilled: Vec<&[u8]> = spill.iter().map(|s| s.as_ref()).collect();
        let pieces: &[&[u8]] = if spill.is_empty() {
            &buf[..nbuf]
        } else {
            &spilled
        };
        match first_type {
            ValueType::Int => Ok(self.mk_int(n)),
            ValueType::Float => Ok(self.mk_float(nf)),
            ValueType::Path => {
                if !ctx.is_empty() {
                    bail!(
                        ErrorKind::Eval,
                        pos,
                        "a string that refers to a store path cannot be appended to a path"
                    );
                }
                let joined = pieces.concat();
                Ok(self.arena.mk_path(&crate::compile::canon_path(&joined)))
            }
            _ => {
                self.arena
                    .stats
                    .note(Stat::Bytes, pieces.iter().map(|p| p.len()).sum());
                self.arena.stats.note(Stat::Ctx, ctx.len() * size_of::<Ctx>());
                let bytes = self.arena.alloc_concat(pieces);
                let ctx = ctx_to_arena(&mut self.arena, ctx);
                Ok(self.arena.mk_string_static(bytes, ctx))
            }
        }
    }

    // ---- attrs and lists ---------------------------------------------------

    /// `a // b`. Two linear merges: one to size the result, one to fill it.
    pub fn update_attrs(&mut self, a: &'static AttrsObj, b: &'static AttrsObj) -> Word {
        if a.is_empty() {
            return Word::from_ptr(b);
        }
        if b.is_empty() {
            return Word::from_ptr(a);
        }
        let (aa, bb) = (a.attrs(), b.attrs());
        let (mut i, mut j, mut common) = (0, 0, 0);
        while i < aa.len() && j < bb.len() {
            match aa[i].sym.cmp(&bb[j].sym) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    common += 1;
                    i += 1;
                    j += 1;
                }
            }
        }
        let out = self.arena.alloc_attrs(aa.len() + bb.len() - common);
        let (mut i, mut j, mut k) = (0, 0, 0);
        while i < aa.len() && j < bb.len() {
            match aa[i].sym.cmp(&bb[j].sym) {
                std::cmp::Ordering::Less => {
                    out.init_from(k, &aa[i]);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    out.init_from(k, &bb[j]);
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    out.init_from(k, &bb[j]);
                    i += 1;
                    j += 1;
                }
            }
            k += 1;
        }
        for x in &aa[i..] {
            out.init_from(k, x);
            k += 1;
        }
        for x in &bb[j..] {
            out.init_from(k, x);
            k += 1;
        }
        Word::from_ptr(out as *const AttrsObj)
    }

    pub fn concat_lists(&mut self, lists: &[&'static ListObj]) -> Word {
        let total: usize = lists.iter().map(|l| l.len as usize).sum();
        if let Some(l) = lists.iter().find(|l| l.len as usize == total) {
            return Word::from_ptr(*l);
        }
        let out = self.arena.alloc_list(total);
        let cells = out.cells();
        let mut k = 0;
        for l in lists {
            for c in l.cells() {
                cells[k].set(c.get());
                k += 1;
            }
        }
        Word::from_ptr(out)
    }

    // ---- arithmetic -----------------------------------------------------------

    pub fn arith(&mut self, a: Word, b: Word, op: ArithOp, pos: Pos) -> EvalResult<Word> {
        let a = self.force(a)?;
        let b = self.force(b)?;
        let is_float = matches!(a.view(), Value::Float(_)) || matches!(b.view(), Value::Float(_));
        if is_float {
            let x = self.force_float(a, pos, op.ctx(true))?;
            let y = self.force_float(b, pos, op.ctx(false))?;
            if op == ArithOp::Div && y == 0.0 {
                bail!(ErrorKind::Eval, pos, "division by zero");
            }
            return Ok(self.mk_float(match op {
                ArithOp::Add => x + y,
                ArithOp::Sub => x - y,
                ArithOp::Mul => x * y,
                ArithOp::Div => x / y,
            }));
        }
        let x = self.force_int(a, pos, op.ctx(true))?;
        let y = self.force_int(b, pos, op.ctx(false))?;
        let r = match op {
            ArithOp::Add => x.checked_add(y),
            ArithOp::Sub => x.checked_sub(y),
            ArithOp::Mul => x.checked_mul(y),
            ArithOp::Div => {
                if y == 0 {
                    bail!(ErrorKind::Eval, pos, "division by zero");
                }
                x.checked_div(y)
            }
        };
        match r {
            Some(v) => Ok(self.mk_int(v)),
            None => bail!(
                ErrorKind::Eval,
                pos,
                "integer overflow in {} {} {} {}",
                op.verb(),
                x,
                op.symbol(),
                y
            ),
        }
    }
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl ArithOp {
    fn verb(self) -> &'static str {
        match self {
            ArithOp::Add => "adding",
            ArithOp::Sub => "subtracting",
            ArithOp::Mul => "multiplying",
            ArithOp::Div => "dividing",
        }
    }
    fn symbol(self) -> &'static str {
        match self {
            ArithOp::Add => "+",
            ArithOp::Sub => "-",
            ArithOp::Mul => "*",
            ArithOp::Div => "/",
        }
    }
    fn ctx(self, first: bool) -> &'static str {
        match (self, first) {
            (ArithOp::Add, true) => "while evaluating the first argument of the addition",
            (ArithOp::Add, false) => "while evaluating the second argument of the addition",
            (ArithOp::Sub, true) => "while evaluating the first argument of the subtraction",
            (ArithOp::Sub, false) => "while evaluating the second argument of the subtraction",
            (ArithOp::Mul, true) => "while evaluating the first of the multiplication",
            (ArithOp::Mul, false) => "while evaluating the second argument of the multiplication",
            (ArithOp::Div, true) => "while evaluating the first operand of the division",
            (ArithOp::Div, false) => "while evaluating the second operand of the division",
        }
    }
}

/// Best-effort position of an IR node, for error messages.
pub fn ir_pos(ir: &Ir) -> Pos {
    match ir {
        Ir::WithVar { pos, .. }
        | Ir::CurPos(pos)
        | Ir::Concat { pos, .. }
        | Ir::SearchPath { pos, .. }
        | Ir::Select { pos, .. }
        | Ir::Apply { pos, .. }
        | Ir::With { pos, .. }
        | Ir::If { pos, .. }
        | Ir::Assert { pos, .. }
        | Ir::Not(_, pos)
        | Ir::Neg(_, pos)
        | Ir::Eq(_, _, pos)
        | Ir::Ne(_, _, pos)
        | Ir::Lt(_, _, pos)
        | Ir::Gt(_, _, pos)
        | Ir::Le(_, _, pos)
        | Ir::Ge(_, _, pos)
        | Ir::And(_, _, pos)
        | Ir::Or(_, _, pos)
        | Ir::Impl(_, _, pos)
        | Ir::Update(_, _, pos)
        | Ir::ConcatLists(_, _, pos)
        | Ir::Sub(_, _, pos)
        | Ir::Mul(_, _, pos)
        | Ir::Div(_, _, pos) => *pos,
        Ir::Attrs(a) => a.pos,
        Ir::Lambda(l) => l.pos,
        Ir::Let { body, .. } => ir_pos(body),
        Ir::Const(_) | Ir::Local { .. } | Ir::HasAttr { .. } | Ir::List(_) => Pos::NONE,
    }
}

pub fn dir_of(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(0) => b"/",
        Some(i) => &path[..i],
        None => b".",
    }
}

impl Evaluator {
    /// `--arg`/`--argstr` handling: call a formals lambda with the matching
    /// subset of `args`; anything else is returned unchanged.
    pub fn auto_call(&mut self, fun: Word, args: Word) -> EvalResult<Word> {
        let fun = self.force(fun)?;
        let Value::Lambda(l) = fun.view() else {
            return Ok(fun);
        };
        let ParamIr::Formals { formals, .. } = &l.lam.param else {
            return Ok(fun);
        };
        let args = self.force_attrs(args, Pos::NONE, "")?;
        let mut entries: Vec<(Sym, Pos, Word)> = Vec::new();
        for f in formals.iter() {
            match args.get(f.sym) {
                Some(a) => entries.push((f.sym, a.pos, a.cell.get())),
                None if f.default.is_some() => {}
                None => bail!(
                    ErrorKind::Eval,
                    l.lam.pos,
                    "cannot evaluate a function that has an argument without a value ('{}')",
                    self.interner.name_str(f.sym)
                ),
            }
        }
        let a = self.arena.mk_attrs_sorted(&entries);
        self.apply(fun, &[a], Pos::NONE)
    }
}

impl Evaluator {
    /// `IETSP_STATS=1`: allocation and phase counters on stderr.
    pub fn print_stats(&self) {
        let st = &self.arena.stats;
        eprintln!(
            "files={} src_bytes={} thunks={} calls={} drvs={} arena_bytes={} compile={:?}",
            self.files.len(),
            self.files.iter().map(|f| f.src.len()).sum::<usize>(),
            self.nr_thunks,
            self.nr_function_calls,
            self.drv.nr_derivations,
            self.arena.bytes_allocated,
            self.t_compile
        );
        for (i, name) in STAT_NAMES.iter().enumerate() {
            eprintln!(
                "{name:>10} count={:>9} bytes={:>11} ({:.1} MiB)",
                st.counts[i],
                st.bytes[i],
                st.bytes[i] as f64 / (1 << 20) as f64
            );
        }
    }
}
