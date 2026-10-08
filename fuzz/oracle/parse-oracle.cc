// Parses Nix text with libnixexpr and prints the tree in the form of
// fuzz/src/sx.rs. Frames on stdin and stdout, see fuzz/src/oracle.rs.

#include <nix/expr/eval.hh>
#include <nix/expr/eval-gc.hh>
#include <nix/expr/eval-settings.hh>
#include <nix/expr/nixexpr.hh>
#include <nix/fetchers/fetch-settings.hh>
#include <nix/store/store-open.hh>
#include <nix/store/globals.hh>
#include <nix/util/config-global.hh>
#include <nix/util/current-process.hh>
#include <nix/util/experimental-features.hh>
#include <nix/util/memory-source-accessor.hh>
#include <nix/util/terminal.hh>
#include <nix/util/util.hh>

#include <bit>
#include <cstdio>
#include <sstream>
#include <string>
#include <unistd.h>

using namespace nix;

namespace {

const SymbolTable * syms;

void quote(std::ostream & o, std::string_view s)
{
    o << '"';
    for (unsigned char c : s) {
        if (c == '"' || c == '\\')
            o << '\\' << c;
        else if (c < 0x20 || c >= 0x7f) {
            char buf[5];
            snprintf(buf, sizeof buf, "\\x%02x", c);
            o << buf;
        } else
            o << c;
    }
    o << '"';
}

void sym(std::ostream & o, Symbol s)
{
    quote(o, std::string_view((*syms)[s]));
}

void show(std::ostream & o, const Expr * e);

void attrPath(std::ostream & o, std::span<const AttrName> path)
{
    o << "(attrpath";
    for (auto & a : path) {
        o << ' ';
        if (a.symbol)
            sym(o, a.symbol);
        else {
            o << "(dyn ";
            show(o, a.expr);
            o << ')';
        }
    }
    o << ')';
}

// an inherited attribute names its source, so the displacements of the
// `inherit (e)` clauses do not show
void attrs(std::ostream & o, const ExprAttrs * a)
{
    o << (a->recursive ? "(attrs rec" : "(attrs nonrec");
    for (auto & [s, d] : *a->attrs) {
        o << " (bind ";
        sym(o, s);
        o << ' ';
        switch (d.kind) {
        case ExprAttrs::AttrDef::Kind::Plain:
            show(o, d.e);
            break;
        case ExprAttrs::AttrDef::Kind::Inherited:
            o << "(inherit)";
            break;
        case ExprAttrs::AttrDef::Kind::InheritedFrom: {
            auto & sel = dynamic_cast<const ExprSelect &>(*d.e);
            auto & from = dynamic_cast<const ExprInheritFrom &>(*sel.e);
            o << "(inherit-from ";
            show(o, (*a->inheritFromExprs)[from.displ]);
            o << ')';
            break;
        }
        }
        o << ')';
    }
    for (auto & d : *a->dynamicAttrs) {
        o << " (dyn-bind ";
        show(o, d.nameExpr);
        o << ' ';
        show(o, d.valueExpr);
        o << ')';
    }
    o << ')';
}

void node(std::ostream & o, const char * tag, std::initializer_list<const Expr *> es)
{
    o << '(' << tag;
    for (auto e : es) {
        o << ' ';
        show(o, e);
    }
    o << ')';
}

void show(std::ostream & o, const Expr * e)
{
    if (auto x = dynamic_cast<const ExprInt *>(e))
        o << "(int " << x->v.integer().value << ')';
    else if (auto x = dynamic_cast<const ExprFloat *>(e)) {
        char buf[32];
        snprintf(buf, sizeof buf, "%016llx", (unsigned long long) std::bit_cast<uint64_t>(x->v.fpoint()));
        o << "(float " << buf << ')';
    } else if (auto x = dynamic_cast<const ExprString *>(e)) {
        o << "(str ";
        quote(o, x->v.string_view());
        o << ')';
    } else if (auto x = dynamic_cast<const ExprPath *>(e)) {
        o << "(path ";
        quote(o, x->v.pathStrView());
        o << ')';
    } else if (auto x = dynamic_cast<const ExprVar *>(e)) {
        o << "(var ";
        sym(o, x->name);
        o << ')';
    } else if (dynamic_cast<const ExprPos *>(e))
        o << "(cur-pos)";
    else if (auto x = dynamic_cast<const ExprSelect *>(e)) {
        o << "(select ";
        show(o, x->e);
        o << ' ';
        attrPath(o, x->getAttrPath());
        if (x->def) {
            o << ' ';
            show(o, x->def);
        }
        o << ')';
    } else if (auto x = dynamic_cast<const ExprOpHasAttr *>(e)) {
        o << "(has ";
        show(o, x->e);
        o << ' ';
        attrPath(o, x->attrPath);
        o << ')';
    } else if (auto x = dynamic_cast<const ExprAttrs *>(e))
        attrs(o, x);
    else if (auto x = dynamic_cast<const ExprList *>(e)) {
        o << "(list";
        for (auto i : x->elems) {
            o << ' ';
            show(o, i);
        }
        o << ')';
    } else if (auto x = dynamic_cast<const ExprLambda *>(e)) {
        o << "(lambda ";
        if (x->arg)
            sym(o, x->arg);
        else
            o << '_';
        if (auto f = x->getFormals()) {
            o << " (formals";
            for (auto & i : f->formals) {
                o << " (formal ";
                sym(o, i.name);
                if (i.def) {
                    o << ' ';
                    show(o, i.def);
                }
                o << ')';
            }
            if (f->ellipsis)
                o << " ...";
            o << ')';
        } else
            o << " _";
        o << ' ';
        show(o, x->body);
        o << ')';
    } else if (auto x = dynamic_cast<const ExprCall *>(e)) {
        o << "(call ";
        show(o, x->fun);
        for (auto a : *x->args) {
            o << ' ';
            show(o, a);
        }
        o << ')';
    } else if (auto x = dynamic_cast<const ExprLet *>(e)) {
        o << "(let ";
        attrs(o, x->attrs);
        o << ' ';
        show(o, x->body);
        o << ')';
    } else if (auto x = dynamic_cast<const ExprWith *>(e))
        node(o, "with", {x->attrs, x->body});
    else if (auto x = dynamic_cast<const ExprIf *>(e))
        node(o, "if", {x->cond, x->then, x->else_});
    else if (auto x = dynamic_cast<const ExprAssert *>(e))
        node(o, "assert", {x->cond, x->body});
    else if (auto x = dynamic_cast<const ExprOpNot *>(e))
        node(o, "!", {x->e});
    else if (auto x = dynamic_cast<const ExprOpEq *>(e))
        node(o, "==", {x->e1, x->e2});
    else if (auto x = dynamic_cast<const ExprOpNEq *>(e))
        node(o, "!=", {x->e1, x->e2});
    else if (auto x = dynamic_cast<const ExprOpAnd *>(e))
        node(o, "&&", {x->e1, x->e2});
    else if (auto x = dynamic_cast<const ExprOpOr *>(e))
        node(o, "||", {x->e1, x->e2});
    else if (auto x = dynamic_cast<const ExprOpImpl *>(e))
        node(o, "->", {x->e1, x->e2});
    else if (auto x = dynamic_cast<const ExprOpUpdate *>(e))
        node(o, "//", {x->e1, x->e2});
    else if (auto x = dynamic_cast<const ExprOpConcatLists *>(e))
        node(o, "++", {x->e1, x->e2});
    else if (auto x = dynamic_cast<const ExprConcatStrings *>(e)) {
        o << (x->forceString ? "(interp" : "(+");
        for (auto & [_, part] : x->es) {
            o << ' ';
            show(o, part);
        }
        o << ')';
    } else
        o << "(unknown)";
}

bool readAll(int fd, void * buf, size_t n)
{
    auto p = (char *) buf;
    while (n) {
        auto r = read(fd, p, n);
        if (r <= 0)
            return false;
        p += r;
        n -= r;
    }
    return true;
}

void writeAll(int fd, const void * buf, size_t n)
{
    auto p = (const char *) buf;
    while (n) {
        auto r = write(fd, p, n);
        if (r <= 0)
            _exit(1);
        p += r;
        n -= r;
    }
}

void reply(uint8_t kind, uint32_t line, uint32_t col, const std::string & text)
{
    uint32_t len = text.size();
    writeAll(1, &kind, 1);
    writeAll(1, &line, 4);
    writeAll(1, &col, 4);
    writeAll(1, &len, 4);
    writeAll(1, text.data(), len);
}

void serve()
{
    static bool readOnly = true;
    initLibUtil();
    initLibStore(false);
    initGC();
    experimentalFeatureSettings.set("experimental-features", "pipe-operators");
    settings.readOnlyMode = true;
    auto store = openStore("dummy://");
    fetchers::Settings fetchSettings;
    EvalSettings evalSettings(readOnly);
    evalSettings.pureEval = false;

    std::shared_ptr<EvalState> state;
    std::shared_ptr<StaticEnv> env;
    auto files = make_ref<MemorySourceAccessor>();
    size_t served = 0;
    std::string input;
    while (true) {
        // a fresh state now and then: the expression arena only grows
        if (!state || served++ % 4096 == 0) {
            state = std::make_shared<EvalState>(LookupPath{}, store, fetchSettings, evalSettings);
            // the oracle compares syntax, not scope: a `with` at the top
            // makes every free variable legal, as iets's parser has it
            auto top = new ExprWith(noPos, new ExprAttrs(), new ExprAttrs());
            env = std::make_shared<StaticEnv>(top, state->staticBaseEnv);
            syms = &state->symbols;
        }
        uint32_t len;
        if (!readAll(0, &len, 4))
            return;
        input.resize(len);
        if (!readAll(0, input.data(), len))
            return;
        try {
            // a file origin: for a string Nix finds lines in the text up to
            // its first NUL only (position.cc, Pos::getSource), a file it reads whole
            auto file = files->addFile(CanonPath("/fuzz/input.nix"), std::string(input));
            auto e = state->parseExprFromFile(file, env);
            std::ostringstream o;
            show(o, e);
            reply(0, 0, 0, o.str());
        } catch (Error & err) {
            uint32_t line = 0, col = 0;
            if (auto pos = err.info().pos) {
                line = pos->line;
                col = pos->column;
            }
            reply(1, line, col, filterANSIEscapes(err.info().msg.str(), true));
        }
    }
}

} // namespace

int main()
{
    setenv("HOME", "/home/fuzz", 1);
    // the parser recurses: the stack nix's own main asks for (src/nix/main.cc)
    setStackSize(60 * 1024 * 1024);
    serve();
    return 0;
}
