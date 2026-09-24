// A consumer of Nix's C API, compiled against Nix's own headers and linked
// against ietsp's shared library. It evaluates an expression, walks an
// attribute set, reads scalars and strings, calls functions, builds values,
// and checks the error paths. docs/c-api.md.

#include <nix_api_util.h>
#include <nix_api_expr.h>
#include <nix_api_value.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;

#define CHECK(cond)                                                     \
    do {                                                                \
        if (cond) {                                                     \
            printf("ok   %s\n", #cond);                                 \
        } else {                                                        \
            printf("FAIL %s (%s:%d)\n", #cond, __FILE__, __LINE__);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

static void copy_str(const char * start, unsigned int n, void * user_data)
{
    char * out = (char *) user_data;
    memcpy(out, start, n);
    out[n] = '\0';
}

static const char * EXPR =
    "rec {\n"
    "  name = \"demo\";\n"
    "  version = 3;\n"
    "  ratio = 0.5;\n"
    "  enabled = true;\n"
    "  nothing = null;\n"
    "  dir = /tmp;\n"
    "  tags = [ \"one\" \"two\" \"three\" ];\n"
    "  greet = who: \"hello ${who}\";\n"
    "  add = a: b: a + b;\n"
    "  nested = { inherit name; deep = [ (1 + 1) ]; };\n"
    "}\n";

int main(void)
{
    nix_c_context * ctx = nix_c_context_create();
    CHECK(ctx != NULL);
    CHECK(nix_libexpr_init(ctx) == NIX_OK);
    CHECK(strlen(nix_version_get()) > 0);

    Store * store = nix_store_open(ctx, "dummy://", NULL);
    CHECK(store != NULL);
    EvalState * state = nix_state_create(ctx, NULL, store);
    CHECK(state != NULL);

    // --- evaluate and inspect the root -----------------------------------
    nix_value * root = nix_alloc_value(ctx, state);
    CHECK(nix_expr_eval_from_string(ctx, state, EXPR, "/", root) == NIX_OK);
    CHECK(nix_get_type(ctx, root) == NIX_TYPE_ATTRS);
    CHECK(nix_get_attrs_size(ctx, root) == 10);

    char * tn = (char *) nix_get_typename(ctx, root);
    CHECK(tn != NULL && strcmp(tn, "a set") == 0);
    free(tn);

    // --- walk the attribute set by index ---------------------------------
    unsigned int n = nix_get_attrs_size(ctx, root);
    int saw_name = 0, saw_greet = 0;
    for (unsigned int i = 0; i < n; i++) {
        const char * an = nix_get_attr_name_byidx(ctx, root, state, i);
        CHECK(an != NULL);
        if (an && strcmp(an, "name") == 0)
            saw_name = 1;
        if (an && strcmp(an, "greet") == 0)
            saw_greet = 1;
    }
    CHECK(saw_name && saw_greet);
    CHECK(nix_get_attr_name_byidx(ctx, root, state, n) == NULL);
    CHECK(nix_err_code(ctx) == NIX_ERR_KEY);

    const char * byidx_name = NULL;
    nix_value * byidx = nix_get_attr_byidx(ctx, root, state, 0, &byidx_name);
    CHECK(byidx != NULL && byidx_name != NULL);
    CHECK(nix_get_type(ctx, byidx) != NIX_TYPE_THUNK);

    // --- scalars ----------------------------------------------------------
    nix_value * version = nix_get_attr_byname(ctx, root, state, "version");
    CHECK(version != NULL);
    CHECK(nix_get_type(ctx, version) == NIX_TYPE_INT);
    CHECK(nix_get_int(ctx, version) == 3);

    nix_value * ratio = nix_get_attr_byname(ctx, root, state, "ratio");
    CHECK(nix_get_float(ctx, ratio) == 0.5);

    nix_value * enabled = nix_get_attr_byname(ctx, root, state, "enabled");
    CHECK(nix_get_bool(ctx, enabled) == true);

    nix_value * nothing = nix_get_attr_byname(ctx, root, state, "nothing");
    CHECK(nix_get_type(ctx, nothing) == NIX_TYPE_NULL);

    nix_value * dir = nix_get_attr_byname(ctx, root, state, "dir");
    CHECK(nix_get_type(ctx, dir) == NIX_TYPE_PATH);
    CHECK(strcmp(nix_get_path_string(ctx, dir), "/tmp") == 0);

    // --- strings ----------------------------------------------------------
    char buf[256];
    nix_value * name = nix_get_attr_byname(ctx, root, state, "name");
    CHECK(nix_get_type(ctx, name) == NIX_TYPE_STRING);
    CHECK(nix_get_string(ctx, name, copy_str, buf) == NIX_OK);
    CHECK(strcmp(buf, "demo") == 0);

    // --- lists ------------------------------------------------------------
    nix_value * tags = nix_get_attr_byname(ctx, root, state, "tags");
    CHECK(nix_get_type(ctx, tags) == NIX_TYPE_LIST);
    CHECK(nix_get_list_size(ctx, tags) == 3);
    nix_value * tag1 = nix_get_list_byidx(ctx, tags, state, 1);
    CHECK(nix_get_string(ctx, tag1, copy_str, buf) == NIX_OK);
    CHECK(strcmp(buf, "two") == 0);
    CHECK(nix_get_list_byidx(ctx, tags, state, 3) == NULL);
    CHECK(nix_err_code(ctx) == NIX_ERR_KEY);

    // --- laziness and deep forcing ---------------------------------------
    nix_value * nested = nix_get_attr_byname_lazy(ctx, root, state, "nested");
    CHECK(nested != NULL);
    CHECK(nix_value_force_deep(ctx, state, nested) == NIX_OK);
    nix_value * deep = nix_get_attr_byname(ctx, nested, state, "deep");
    nix_value * deep0 = nix_get_list_byidx_lazy(ctx, deep, state, 0);
    CHECK(nix_get_type(ctx, deep0) == NIX_TYPE_INT);
    CHECK(nix_get_int(ctx, deep0) == 2);

    // --- calling functions ------------------------------------------------
    nix_value * greet = nix_get_attr_byname(ctx, root, state, "greet");
    CHECK(nix_get_type(ctx, greet) == NIX_TYPE_FUNCTION);
    nix_value * world = nix_alloc_value(ctx, state);
    CHECK(nix_init_string(ctx, world, "world") == NIX_OK);
    nix_value * greeting = nix_alloc_value(ctx, state);
    CHECK(nix_value_call(ctx, state, greet, world, greeting) == NIX_OK);
    CHECK(nix_get_string(ctx, greeting, copy_str, buf) == NIX_OK);
    CHECK(strcmp(buf, "hello world") == 0);

    nix_value * add = nix_get_attr_byname(ctx, root, state, "add");
    nix_value * one = nix_alloc_value(ctx, state);
    nix_value * two = nix_alloc_value(ctx, state);
    CHECK(nix_init_int(ctx, one, 1) == NIX_OK);
    CHECK(nix_init_int(ctx, two, 41) == NIX_OK);
    nix_value * sum = nix_alloc_value(ctx, state);
    NIX_VALUE_CALL(ctx, state, sum, add, one, two);
    CHECK(nix_err_code(ctx) == NIX_OK);
    CHECK(nix_get_int(ctx, sum) == 42);

    // a value that is only initialized once
    CHECK(nix_init_int(ctx, one, 2) != NIX_OK);

    // nix_init_apply stores the call instead of running it
    nix_value * lazy = nix_alloc_value(ctx, state);
    CHECK(nix_init_apply(ctx, lazy, greet, world) == NIX_OK);
    CHECK(nix_get_type(ctx, lazy) == NIX_TYPE_THUNK);
    CHECK(nix_value_force(ctx, state, lazy) == NIX_OK);
    CHECK(nix_get_type(ctx, lazy) == NIX_TYPE_STRING);

    // --- building values --------------------------------------------------
    nix_value * typeOf = nix_alloc_value(ctx, state);
    CHECK(nix_expr_eval_from_string(ctx, state, "builtins.typeOf", "/", typeOf) == NIX_OK);

    ListBuilder * lb = nix_make_list_builder(ctx, state, 2);
    CHECK(lb != NULL);
    CHECK(nix_list_builder_insert(ctx, lb, 0, one) == NIX_OK);
    CHECK(nix_list_builder_insert(ctx, lb, 1, two) == NIX_OK);
    CHECK(nix_list_builder_insert(ctx, lb, 2, two) == NIX_ERR_KEY);
    nix_value * list = nix_alloc_value(ctx, state);
    CHECK(nix_make_list(ctx, lb, list) == NIX_OK);
    nix_list_builder_free(lb);
    CHECK(nix_get_list_size(ctx, list) == 2);

    BindingsBuilder * bb = nix_make_bindings_builder(ctx, state, 2);
    CHECK(bb != NULL);
    CHECK(nix_bindings_builder_insert(ctx, bb, "who", world) == NIX_OK);
    CHECK(nix_bindings_builder_insert(ctx, bb, "count", one) == NIX_OK);
    nix_value * attrs = nix_alloc_value(ctx, state);
    CHECK(nix_make_attrs(ctx, attrs, bb) == NIX_OK);
    nix_bindings_builder_free(bb);
    CHECK(nix_get_attrs_size(ctx, attrs) == 2);
    CHECK(nix_has_attr_byname(ctx, attrs, state, "who") == true);
    CHECK(nix_has_attr_byname(ctx, attrs, state, "nope") == false);

    nix_value * kind = nix_alloc_value(ctx, state);
    CHECK(nix_value_call(ctx, state, typeOf, attrs, kind) == NIX_OK);
    CHECK(nix_get_string(ctx, kind, copy_str, buf) == NIX_OK);
    CHECK(strcmp(buf, "set") == 0);

    nix_value * copy = nix_alloc_value(ctx, state);
    CHECK(nix_copy_value(ctx, copy, version) == NIX_OK);
    CHECK(nix_get_int(ctx, copy) == 3);

    // reference counting is a no-op here, but the calls must exist and work
    CHECK(nix_value_incref(ctx, copy) == NIX_OK);
    CHECK(nix_value_decref(ctx, copy) == NIX_OK);
    CHECK(nix_gc_incref(ctx, copy) == NIX_OK);
    CHECK(nix_gc_decref(ctx, copy) == NIX_OK);
    nix_gc_now();
    CHECK(nix_get_int(ctx, copy) == 3);

    // --- error paths ------------------------------------------------------
    // a syntax error
    nix_value * bad = nix_alloc_value(ctx, state);
    CHECK(nix_expr_eval_from_string(ctx, state, "{ a = ; }", "/", bad) == NIX_ERR_NIX_ERROR);
    unsigned int len = 0;
    const char * msg = nix_err_msg(NULL, ctx, &len);
    CHECK(msg != NULL && len == strlen(msg) && len > 0);
    printf("     syntax error: %s\n", msg);
    CHECK(nix_err_name(NULL, ctx, copy_str, buf) == NIX_OK);
    printf("     error name: %s\n", buf);
    CHECK(strcmp(buf, "parse") == 0);
    CHECK(nix_err_info_msg(NULL, ctx, copy_str, buf) == NIX_OK);
    CHECK(strlen(buf) > 0);

    // an evaluation error
    nix_value * thrown = nix_alloc_value(ctx, state);
    CHECK(nix_expr_eval_from_string(ctx, state, "builtins.throw \"nope\"", "/", thrown)
          == NIX_ERR_NIX_ERROR);
    CHECK(nix_err_name(NULL, ctx, copy_str, buf) == NIX_OK);
    CHECK(strcmp(buf, "throw") == 0);

    // a wrong type
    nix_clear_err(ctx);
    CHECK(nix_get_int(ctx, name) == 0);
    CHECK(nix_err_code(ctx) == NIX_ERR_NIX_ERROR);
    CHECK(nix_err_info_msg(NULL, ctx, copy_str, buf) == NIX_OK);
    printf("     type error: %s\n", buf);

    // a missing attribute
    nix_clear_err(ctx);
    CHECK(nix_get_attr_byname(ctx, root, state, "absent") == NULL);
    CHECK(nix_err_code(ctx) == NIX_ERR_KEY);
    CHECK(nix_has_attr_byname(ctx, root, state, "absent") == false);

    // an attribute of something that is not a set
    nix_clear_err(ctx);
    CHECK(nix_get_attr_byname(ctx, version, state, "x") == NULL);
    CHECK(nix_err_code(ctx) == NIX_ERR_NIX_ERROR);

    // an uninitialized value, and a null one
    nix_clear_err(ctx);
    nix_value * unset = nix_alloc_value(ctx, state);
    CHECK(nix_get_type(ctx, unset) == NIX_TYPE_NULL);
    CHECK(nix_err_code(ctx) == NIX_ERR_UNKNOWN);
    nix_clear_err(ctx);
    CHECK(nix_get_type(ctx, NULL) == NIX_TYPE_NULL);
    CHECK(nix_err_code(ctx) == NIX_ERR_UNKNOWN);

    // a cleared context reports nothing
    nix_clear_err(ctx);
    CHECK(nix_err_code(ctx) == NIX_OK);

    nix_state_free(state);
    nix_store_free(store);
    nix_c_context_free(ctx);

    printf("%s\n", failures ? "FAILED" : "all c-api checks passed");
    return failures ? 1 : 0;
}
