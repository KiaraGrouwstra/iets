use std::collections::HashSet;

use super::Evaluator;

fn eval_str(src: &str) -> String {
    let mut ev = Evaluator::new();
    let v = ev.eval_string(src, b"/").expect("eval");
    let v = ev.force_deep(v).expect("force");
    let mut out = Vec::new();
    crate::print::print_ambiguous(&mut ev, v, 1, &mut out, &mut HashSet::new(), 0).unwrap();
    String::from_utf8(out).unwrap()
}

fn eval_err(src: &str) -> String {
    let mut ev = Evaluator::new();
    match ev.eval_string(src, b"/").and_then(|v| ev.force_deep(v)) {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.msg,
    }
}

#[test]
fn arithmetic_and_strings() {
    assert_eq!(eval_str("1 + 2 * 3 - 4 / 2"), "5");
    assert_eq!(eval_str("1.5 + 1"), "2.5");
    assert_eq!(eval_str(r#""a" + "b" + toString 3"#), r#""ab3""#);
    assert_eq!(eval_str("builtins.stringLength \"héllo\""), "6");
    assert_eq!(eval_str("-(3 - 5)"), "2");
}

#[test]
fn laziness_and_recursion() {
    assert_eq!(eval_str("let x = throw \"no\"; in 1"), "1");
    assert_eq!(
        eval_str("let fib = n: if n < 2 then n else fib (n - 1) + fib (n - 2); in fib 15"),
        "610"
    );
    assert_eq!(eval_str("rec { a = b + 1; b = 1; }.a"), "2");
    assert_eq!(
        eval_err("let x = y; y = x; in x"),
        "infinite recursion encountered"
    );
}

#[test]
fn scoping() {
    assert_eq!(eval_str("let x = 1; in with { x = 2; }; x"), "1");
    assert_eq!(eval_str("with { x = 2; }; with { x = 3; }; x"), "3");
    assert_eq!(eval_str("with { map = 1; }; map"), "<PRIMOP>");
    assert_eq!(eval_str("with { true = 1; }; true"), "true");
    assert_eq!(
        eval_str("let f = { a, b ? a + 1, ... } @ args: a + b + args.c; in f { a = 1; c = 10; }"),
        "13"
    );
    assert_eq!(
        eval_str("let inherit (builtins) length; in length [ 1 2 ]"),
        "2"
    );
    assert_eq!(
        eval_str("{ a.b = 1; a.c = 2; a = { d = 3; }; }"),
        "{ a = { b = 1; c = 2; d = 3; }; }"
    );
    assert!(eval_err("{ a = 1; a = 2; }").starts_with("attribute 'a' already defined"));
}

#[test]
fn attrs_and_lists() {
    assert_eq!(
        eval_str("{ b = 2; a = 1; } // { a = 3; }"),
        "{ a = 3; b = 2; }"
    );
    assert_eq!(eval_str("[ 1 ] ++ [ 2 3 ]"), "[ 1 2 3 ]");
    assert_eq!(eval_str("{ a = 1; } ? a"), "true");
    assert_eq!(eval_str("{ a = 1; }.b or 7"), "7");
    assert_eq!(
        eval_str("let s = \"x\"; in { ${s} = 1; \"${s}y\" = 2; }"),
        "{ x = 1; xy = 2; }"
    );
    assert_eq!(
        eval_str("builtins.sort builtins.lessThan [ 3 1 2 ]"),
        "[ 1 2 3 ]"
    );
    assert_eq!(eval_str("map (x: x * 2) [ 1 2 ]"), "[ 2 4 ]");
    assert_eq!(eval_str("builtins.foldl' (a: b: a + b) 0 [ 1 2 3 ]"), "6");
}

#[test]
fn equality() {
    assert_eq!(eval_str("let f = x: x; in f == f"), "false");
    assert_eq!(eval_str("let f = x: x; in builtins.elem f [ f ]"), "true");
    assert_eq!(eval_str("1 == 1.0"), "true");
    assert_eq!(
        eval_str("[ 1 { a = \"x\"; } ] == [ 1 { a = \"x\"; } ]"),
        "true"
    );
    assert_eq!(eval_str("\"a\" < \"b\" && [ 1 2 ] < [ 1 3 ]"), "true");
}

#[test]
fn try_eval_and_errors() {
    assert_eq!(
        eval_str("(builtins.tryEval (throw \"x\")).success"),
        "false"
    );
    assert_eq!(
        eval_str("(builtins.tryEval (assert false; 1)).success"),
        "false"
    );
    assert_eq!(
        eval_err("builtins.tryEval (abort \"x\")"),
        "evaluation aborted with the following error message: 'x'"
    );
    assert_eq!(eval_err("{ a = 1; }.b"), "attribute 'b' missing");
}

#[test]
fn printing() {
    assert_eq!(
        eval_str("[ (x: x) builtins.map (builtins.map (x: x)) ]"),
        "[ <LAMBDA> <PRIMOP> <PRIMOP-APP> ]"
    );
    assert_eq!(eval_str("1.0e10"), "1e+10");
    assert_eq!(eval_str("0.1 + 0.2"), "0.3");
    assert_eq!(
        eval_str("\"a\\n\\\"b\\\" \\${x}\""),
        "\"a\\n\\\"b\\\" \\${x}\""
    );
    assert_eq!(
        eval_str("{ \"a b\" = 1; \"1x\" = 2; \"if\" = 3; x-y = 4; }"),
        "{ \"1x\" = 2; \"a b\" = 1; \"if\" = 3; x-y = 4; }"
    );
}

#[test]
fn derivation_wrapper_exists() {
    assert_eq!(eval_str("builtins.isFunction derivation"), "true");
    assert_eq!(
        eval_str("builtins.typeOf builtins.derivation"),
        "\"lambda\""
    );
}
